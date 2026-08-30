//! New-era container writer — the encoder-side mirror of
//! [`crate::file_header`]: the §1.1 descriptor, the §1.2 header, the
//! seek table, the stored WAV header blob, the frame payload, the
//! terminating blob, and the §1.8 `cFileMD5` digest, laid out in the
//! on-disk order `format-reference.md` §1.1 pins.
//!
//! The writer takes the frame payload **already in the §4.1 word
//! layout** (little-endian 32-bit words consumed MSB-first — see
//! [`to_le_word_layout`]) and the seek table as absolute file offsets;
//! [`crate::encoder::ApeEncoder`] produces both. Everything else is
//! pure layout.
//!
//! `cFileMD5` (§1.8): `MD5(WAV-header blob || frame data ||
//! APE_HEADER || seek table)`. The staged reference leaves the
//! position of a non-empty terminating blob open (every corpus fixture
//! carries none) and infers "immediately after the frame data" from
//! the write order; that inference is what this writer implements
//! ([`TERMINATING_DATA_IN_MD5_AFTER_FRAMES`]), and the crate's
//! black-box validation against the vendor decoder is what pins it.

use crate::file_header::{DESCRIPTOR_LEN, NEW_HEADER_LEN};
use crate::header::{CompressionLevel, MAGIC};
use crate::md5::Md5;

/// The file version this crate's encoder writes: the 3.99 "new"
/// range coder (§2.6) with the ≥ 3980 descriptor layout (§1.1) — the
/// same version the vendor fixture corpus carries.
pub const ENCODER_FILE_VERSION: u16 = 3990;

/// Where a non-empty terminating blob folds into `cFileMD5`: after the
/// frame data, before the `APE_HEADER` block (the §1.8 write-order
/// inference).
pub const TERMINATING_DATA_IN_MD5_AFTER_FRAMES: bool = true;

/// Reshape a logical byte stream into the §4.1 on-disk layout: the
/// frame-data region is addressed as little-endian 32-bit words
/// consumed MSB-first, so logical byte `p` lives at physical
/// `(p & !3) + (3 - (p & 3))`. The stream is zero-padded to a whole
/// number of words first (the vendor pads the region the same way —
/// every fixture's `nAPEFrameDataBytes` is a multiple of 4).
pub fn to_le_word_layout(mut logical: Vec<u8>) -> Vec<u8> {
    while logical.len() % 4 != 0 {
        logical.push(0);
    }
    for chunk in logical.chunks_mut(4) {
        chunk.reverse();
    }
    logical
}

/// Everything the new-era layout stores, ready to serialise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLayout {
    /// The two bytes of the §1.1 alignment gap at descriptor offsets
    /// 6..8 (`nPadding` in later SDKs). Semantically dead — every
    /// parser skips them — and the vendor encoder leaves them
    /// uninitialised (the corpus carries non-zero garbage here); this
    /// crate's encoder writes zeros. Carried so a layout can reproduce
    /// a vendor file byte-for-byte.
    pub descriptor_padding: [u8; 2],
    /// Encoder profile (§1.2 `nCompressionLevel`).
    pub compression_level: CompressionLevel,
    /// §1.6 format flags (the encoder writes `0`: a WAV header is
    /// always stored).
    pub format_flags: u16,
    /// Blocks per non-final frame.
    pub blocks_per_frame: u32,
    /// Blocks in the final frame.
    pub final_frame_blocks: u32,
    /// Frame count (`>= 1`).
    pub total_frames: u32,
    /// Bits per sample (8 / 16 / 24).
    pub bits_per_sample: u16,
    /// Channel count.
    pub channels: u16,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Per-frame byte offsets, absolute within the file.
    pub seek_table: Vec<u32>,
    /// Stored WAV header blob (verbatim).
    pub wav_header: Vec<u8>,
    /// Frame payload in the §4.1 word layout.
    pub frame_data: Vec<u8>,
    /// Trailing WAV data blob (verbatim).
    pub terminating_data: Vec<u8>,
}

impl FileLayout {
    /// Byte offset of the first frame-data byte: descriptor + header +
    /// seek table + WAV header blob (§1.1 block order).
    pub fn audio_data_offset(&self) -> usize {
        DESCRIPTOR_LEN + NEW_HEADER_LEN + self.seek_table.len() * 4 + self.wav_header.len()
    }

    /// The 24-byte §1.2 `APE_HEADER` block.
    pub fn header_block(&self) -> [u8; NEW_HEADER_LEN] {
        let mut h = [0u8; NEW_HEADER_LEN];
        h[0..2].copy_from_slice(&self.compression_level.as_u16().to_le_bytes());
        h[2..4].copy_from_slice(&self.format_flags.to_le_bytes());
        h[4..8].copy_from_slice(&self.blocks_per_frame.to_le_bytes());
        h[8..12].copy_from_slice(&self.final_frame_blocks.to_le_bytes());
        h[12..16].copy_from_slice(&self.total_frames.to_le_bytes());
        h[16..18].copy_from_slice(&self.bits_per_sample.to_le_bytes());
        h[18..20].copy_from_slice(&self.channels.to_le_bytes());
        h[20..24].copy_from_slice(&self.sample_rate.to_le_bytes());
        h
    }

    /// The seek table serialised as little-endian `u32`s.
    pub fn seek_table_bytes(&self) -> Vec<u8> {
        self.seek_table
            .iter()
            .flat_map(|o| o.to_le_bytes())
            .collect()
    }

    /// §1.8 `cFileMD5`: `MD5(WAV header || frame data [|| terminating
    /// data] || APE_HEADER || seek table)`.
    pub fn file_md5(&self) -> [u8; 16] {
        let mut h = Md5::new();
        h.update(&self.wav_header);
        h.update(&self.frame_data);
        if TERMINATING_DATA_IN_MD5_AFTER_FRAMES {
            h.update(&self.terminating_data);
        }
        h.update(&self.header_block());
        h.update(&self.seek_table_bytes());
        h.finish()
    }

    /// The 52-byte §1.1 `APE_DESCRIPTOR`.
    pub fn descriptor_block(&self) -> [u8; DESCRIPTOR_LEN] {
        let mut d = [0u8; DESCRIPTOR_LEN];
        d[0..4].copy_from_slice(&MAGIC);
        d[4..6].copy_from_slice(&ENCODER_FILE_VERSION.to_le_bytes());
        d[6..8].copy_from_slice(&self.descriptor_padding);
        d[8..12].copy_from_slice(&(DESCRIPTOR_LEN as u32).to_le_bytes());
        d[12..16].copy_from_slice(&(NEW_HEADER_LEN as u32).to_le_bytes());
        d[16..20].copy_from_slice(&((self.seek_table.len() * 4) as u32).to_le_bytes());
        d[20..24].copy_from_slice(&(self.wav_header.len() as u32).to_le_bytes());
        let frame_len = self.frame_data.len() as u64;
        d[24..28].copy_from_slice(&(frame_len as u32).to_le_bytes());
        d[28..32].copy_from_slice(&((frame_len >> 32) as u32).to_le_bytes());
        d[32..36].copy_from_slice(&(self.terminating_data.len() as u32).to_le_bytes());
        d[36..52].copy_from_slice(&self.file_md5());
        d
    }

    /// Serialise the whole file in the §1.1 block order.
    pub fn serialize(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.audio_data_offset() + self.frame_data.len());
        out.extend_from_slice(&self.descriptor_block());
        out.extend_from_slice(&self.header_block());
        out.extend_from_slice(&self.seek_table_bytes());
        out.extend_from_slice(&self.wav_header);
        out.extend_from_slice(&self.frame_data);
        out.extend_from_slice(&self.terminating_data);
        out
    }
}

/// Synthesise the canonical 44-byte RIFF/WAVE PCM header for a stream
/// of `data_len` PCM bytes — what the encoder stores when the caller
/// supplies no source WAV header to pass through.
pub fn canonical_wav_header(
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    data_len: u32,
) -> Vec<u8> {
    let block_align = channels * bits_per_sample.div_ceil(8);
    let byte_rate = sample_rate * u32::from(block_align);
    let mut h = Vec::with_capacity(44);
    h.extend_from_slice(b"RIFF");
    h.extend_from_slice(&data_len.wrapping_add(36).to_le_bytes());
    h.extend_from_slice(b"WAVE");
    h.extend_from_slice(b"fmt ");
    h.extend_from_slice(&16u32.to_le_bytes());
    h.extend_from_slice(&1u16.to_le_bytes()); // PCM
    h.extend_from_slice(&channels.to_le_bytes());
    h.extend_from_slice(&sample_rate.to_le_bytes());
    h.extend_from_slice(&byte_rate.to_le_bytes());
    h.extend_from_slice(&block_align.to_le_bytes());
    h.extend_from_slice(&bits_per_sample.to_le_bytes());
    h.extend_from_slice(b"data");
    h.extend_from_slice(&data_len.to_le_bytes());
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::file_header::FileInfo;

    /// Rebuild every vendor fixture's layout from its parsed fields and
    /// check the writer reproduces the file byte-for-byte — descriptor
    /// (MD5 included), header, seek table, blobs, payload.
    #[test]
    fn writer_reproduces_every_vendor_fixture_byte_for_byte() {
        for (name, data) in [
            (
                "left_silent_stereo",
                &include_bytes!("../tests/fixtures/left_silent_stereo.ape")[..],
            ),
            (
                "noise_stereo",
                &include_bytes!("../tests/fixtures/noise_stereo.ape")[..],
            ),
            (
                "silence_mono8k",
                &include_bytes!("../tests/fixtures/silence_mono8k.ape")[..],
            ),
            (
                "silence_stereo",
                &include_bytes!("../tests/fixtures/silence_stereo.ape")[..],
            ),
            (
                "tone_lr_equal",
                &include_bytes!("../tests/fixtures/tone_lr_equal.ape")[..],
            ),
            (
                "two_frame_mono8k",
                &include_bytes!("../tests/fixtures/two_frame_mono8k.ape")[..],
            ),
            (
                "zeros_then_noise_mono",
                &include_bytes!("../tests/fixtures/zeros_then_noise_mono.ape")[..],
            ),
        ] {
            let info = FileInfo::parse(data).unwrap();
            let start = info.audio_data_offset;
            let end = info.audio_data_end() as usize;
            let layout = FileLayout {
                descriptor_padding: [data[6], data[7]],
                compression_level: info.compression_level,
                format_flags: info.format_flags.0,
                blocks_per_frame: info.blocks_per_frame,
                final_frame_blocks: info.final_frame_blocks,
                total_frames: info.total_frames,
                bits_per_sample: info.bits_per_sample,
                channels: info.channels,
                sample_rate: info.sample_rate,
                seek_table: info.seek_table.clone(),
                wav_header: info.wav_header.clone(),
                frame_data: data[start..end].to_vec(),
                terminating_data: data[end..].to_vec(),
            };
            assert_eq!(layout.audio_data_offset(), start, "{name}");
            assert_eq!(
                layout.file_md5(),
                info.file_md5.unwrap(),
                "{name}: cFileMD5"
            );
            assert_eq!(layout.serialize(), data, "{name}: whole file");
        }
    }

    #[test]
    fn word_layout_pads_and_reverses_per_word() {
        assert_eq!(
            to_le_word_layout(vec![1, 2, 3, 4, 5]),
            vec![4, 3, 2, 1, 0, 0, 0, 5]
        );
        assert_eq!(to_le_word_layout(Vec::new()), Vec::<u8>::new());
    }

    #[test]
    fn canonical_wav_header_matches_the_vendor_stored_blob() {
        // noise_stereo's stored header is the canonical 44-byte form
        // for 24000 data bytes of 16-bit stereo 44.1 kHz.
        let data = include_bytes!("../tests/fixtures/noise_stereo.ape");
        let info = FileInfo::parse(data).unwrap();
        assert_eq!(canonical_wav_header(2, 44100, 16, 24000), info.wav_header);
    }
}
