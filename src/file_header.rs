//! Full per-version file header / tail extraction — the container
//! layout the staged `docs/audio/ape/format-reference.md` §1 pins.
//!
//! APE has two header eras split by the file version (`version =
//! spec version × 1000`, little-endian throughout):
//!
//! * **Old era** (`version < 3980`, ≤ 3.97): one flat 32-byte header
//!   at file start, followed inline — in this exact order — by an
//!   optional peak level, an optional seek-element count, the stored
//!   WAV header blob, the seek byte table, and (for `version <=
//!   3800`) a seek *bit* table. Blocks-per-frame is **derived** from
//!   the version/level (§1.4); bits-per-sample from the format flags
//!   (§1.6).
//! * **New era** (`version >= 3980`, 3.98+): a fixed 52-byte
//!   descriptor whose length fields locate every later block (with a
//!   2-byte alignment gap after the version, so the first `u32` sits
//!   at offset 8, and a `descriptor_bytes` forward-compat skip), then
//!   a 24-byte header carrying the sound parameters and an explicit
//!   blocks-per-frame, then seek table → WAV header blob → frame
//!   payload → terminating blob.
//!
//! A junk prefix (e.g. a leading ID3v2 tag) may precede the `'MAC '`
//! magic; [`FileInfo::parse`] scans for the magic and records the skip
//! as [`FileInfo::junk_bytes`]. Trailing APEv2 / ID3v1 tags sit after
//! the terminating data and are a container concern outside this
//! layout.
//!
//! Note the era asymmetry the staged reference calls out: the **block
//! order differs** (old: peak → count → WAV header → seek tables; new:
//! header → seek table → WAV header), and only new-era files store
//! blocks-per-frame explicitly.
//!
//! The existing [`crate::header::HeaderPrefix`] Phase 1 parser reads
//! bytes 6..8 as the compression level, which holds for the old-era
//! flat header only — in the new era those bytes are the descriptor's
//! alignment padding and the level lives in the later header block.
//! This module is the version-aware parser real files need;
//! `HeaderPrefix` remains the old-era/probe view.

use crate::error::{Error, Result};
use crate::header::{CompressionLevel, MAGIC};

/// Version boundary at which the descriptor + header split begins
/// (§1.2): `>= 3980` is the new era.
pub const NEW_ERA_VERSION: u16 = 3980;

/// Last version whose seek table is followed by a per-frame *bit*
/// offset table (§1.3 step 5): `<= 3800`.
pub const SEEK_BIT_TABLE_MAX_VERSION: u16 = 3800;

/// Fixed size of the new-era descriptor in the documented snapshot;
/// `descriptor_bytes` may exceed it (forward compat — skip the
/// surplus).
pub const DESCRIPTOR_LEN: usize = 52;

/// Fixed size of the new-era header block; `header_bytes` may exceed
/// it (skip the surplus).
pub const NEW_HEADER_LEN: usize = 24;

/// Fixed size of the old-era flat header.
pub const OLD_HEADER_LEN: usize = 32;

/// Size of the WAV header a decoder synthesises when the stream
/// carries none (§1.7: `CREATE_WAV_HEADER` → 44).
pub const SYNTHESISED_WAV_HEADER_LEN: u32 = 44;

/// §1.6 format-flags bitmask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FormatFlags(pub u16);

impl FormatFlags {
    /// Bit 1 — 8-bit source (obsolete; old era only).
    pub const EIGHT_BIT: u16 = 1;
    /// Bit 2 — new CRC32 error detection (obsolete).
    pub const CRC: u16 = 2;
    /// Bit 4 — a `u32` peak level follows the old-era header
    /// (obsolete).
    pub const HAS_PEAK_LEVEL: u16 = 4;
    /// Bit 8 — 24-bit source (obsolete; old era only).
    pub const TWENTY_FOUR_BIT: u16 = 8;
    /// Bit 16 — a `u32` seek-element count follows the peak level.
    pub const HAS_SEEK_ELEMENTS: u16 = 16;
    /// Bit 32 — no WAV header is stored; synthesise one on decode.
    pub const CREATE_WAV_HEADER: u16 = 32;

    /// Whether `bit` (one of the associated constants) is set.
    pub const fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    /// Old-era bits-per-sample derivation (§1.6): 8 if the 8-bit flag
    /// is set, else 24 if the 24-bit flag is set, else 16.
    pub const fn bits_per_sample_old(self) -> u16 {
        if self.has(Self::EIGHT_BIT) {
            8
        } else if self.has(Self::TWENTY_FOUR_BIT) {
            24
        } else {
            16
        }
    }
}

/// §1.1 new-era descriptor (`version >= 3980`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApeDescriptor {
    /// Total descriptor size on disk (>= 52; the surplus is skipped).
    pub descriptor_bytes: u32,
    /// Size of the header block that follows (>= 24).
    pub header_bytes: u32,
    /// Size of the seek table in bytes (element count = bytes / 4).
    pub seek_table_bytes: u32,
    /// Size of the stored WAV/RIFF header blob.
    pub header_data_bytes: u32,
    /// Low 32 bits of the compressed-audio byte count.
    pub ape_frame_data_bytes: u32,
    /// High 32 bits of the compressed-audio byte count.
    pub ape_frame_data_bytes_high: u32,
    /// Size of the trailing WAV data blob (excludes any tag).
    pub terminating_data_bytes: u32,
    /// MD5 of the file, computed over a region the staged reference
    /// leaves as a GAP (not needed for decode).
    pub file_md5: [u8; 16],
}

impl ApeDescriptor {
    /// The 64-bit compressed-audio byte count
    /// (`low + (high << 32)`).
    pub const fn frame_data_len(&self) -> u64 {
        self.ape_frame_data_bytes as u64 | ((self.ape_frame_data_bytes_high as u64) << 32)
    }
}

/// Little-endian cursor over the parsed buffer.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Reader { data, pos }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated)?;
        if end > self.data.len() {
            return Err(Error::Truncated);
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        self.take(n).map(|_| ())
    }
}

/// Unified per-version view of everything the header/tail layout pins,
/// for both eras.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    /// Bytes skipped before the `'MAC '` magic (e.g. a leading ID3v2
    /// tag).
    pub junk_bytes: usize,
    /// Raw version field (spec version × 1000).
    pub version: u16,
    /// Encoder profile.
    pub compression_level: CompressionLevel,
    /// §1.6 format flags.
    pub format_flags: FormatFlags,
    /// Channel count.
    pub channels: u16,
    /// Sample rate in Hz.
    pub sample_rate: u32,
    /// Bits per sample (stored in the new era; flag-derived in the
    /// old).
    pub bits_per_sample: u16,
    /// Audio blocks (sample-frames) per APE frame — stored (new era)
    /// or version/level-derived per §1.4 (old era).
    pub blocks_per_frame: u32,
    /// Blocks in the final frame.
    pub final_frame_blocks: u32,
    /// Number of APE frames.
    pub total_frames: u32,
    /// Old-era stored peak level, when present.
    pub peak_level: Option<u32>,
    /// Per-frame byte offsets (§1.3 pins them absolute for the old
    /// era; validated against the computed audio start for the new).
    pub seek_table: Vec<u32>,
    /// Old-era (`version <= 3800`) per-frame starting-bit offsets.
    pub seek_bit_table: Option<Vec<u8>>,
    /// Verbatim stored WAV/RIFF header blob (empty when the stream
    /// says to synthesise one).
    pub wav_header: Vec<u8>,
    /// Absolute offset of the first compressed frame byte within the
    /// parsed buffer.
    pub audio_data_offset: usize,
    /// Compressed-audio byte count: descriptor-stored (new era) or
    /// derived from the buffer remainder minus the terminating blob
    /// (old era — any trailing tag inflates the derivation, which is
    /// the best the flat header allows).
    pub audio_data_len: u64,
    /// Size of the trailing WAV data blob.
    pub terminating_data_bytes: u32,
    /// New-era whole-file MD5.
    pub file_md5: Option<[u8; 16]>,
    /// The raw new-era descriptor, when the era carries one.
    pub descriptor: Option<ApeDescriptor>,
    /// Length of the buffer the info was parsed from (private so the
    /// struct can only be built by parsing — keeps the derived audio
    /// region and the buffer in lockstep).
    data_len: usize,
}

impl FileInfo {
    /// Locate the `'MAC '` magic in `data`, allowing a junk prefix.
    pub fn find_magic(data: &[u8]) -> Option<usize> {
        data.windows(MAGIC.len()).position(|w| w == MAGIC)
    }

    /// Parse the header/tail layout out of `data` (a whole file or at
    /// least its head through the WAV-header blob), skipping any junk
    /// prefix before the magic.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let junk = Self::find_magic(data).ok_or(Error::InvalidMagic)?;
        Self::parse_at(data, junk)
    }

    /// Parse with an explicit junk-prefix size (the magic must sit at
    /// `data[junk..]`).
    pub fn parse_at(data: &[u8], junk: usize) -> Result<Self> {
        let mut r = Reader::new(data, junk);
        if r.take(4)? != MAGIC {
            return Err(Error::InvalidMagic);
        }
        let version = r.u16()?;
        if version >= NEW_ERA_VERSION {
            Self::parse_new_era(data, junk, version, r)
        } else {
            Self::parse_old_era(data, junk, version, r)
        }
    }

    /// §1.1 + §1.2 new-era walk: descriptor → header → seek table →
    /// WAV header blob → frame payload.
    fn parse_new_era(data: &[u8], junk: usize, version: u16, mut r: Reader<'_>) -> Result<Self> {
        // §1.1: 2-byte alignment gap after the version, so the first
        // u32 starts at descriptor offset 8.
        r.skip(2)?;
        let descriptor_bytes = r.u32()?;
        if (descriptor_bytes as usize) < DESCRIPTOR_LEN {
            return Err(Error::Malformed("descriptor shorter than 52 bytes"));
        }
        let desc = ApeDescriptor {
            descriptor_bytes,
            header_bytes: r.u32()?,
            seek_table_bytes: r.u32()?,
            header_data_bytes: r.u32()?,
            ape_frame_data_bytes: r.u32()?,
            ape_frame_data_bytes_high: r.u32()?,
            terminating_data_bytes: r.u32()?,
            file_md5: {
                let b = r.take(16)?;
                let mut md5 = [0u8; 16];
                md5.copy_from_slice(b);
                md5
            },
        };
        if (desc.header_bytes as usize) < NEW_HEADER_LEN {
            return Err(Error::Malformed("header block shorter than 24 bytes"));
        }
        // Forward compat: skip descriptor surplus beyond 52.
        r.skip(descriptor_bytes as usize - DESCRIPTOR_LEN)?;

        // §1.2 header block.
        let level_raw = r.u16()?;
        let compression_level = CompressionLevel::from_u16(level_raw)?;
        let format_flags = FormatFlags(r.u16()?);
        let blocks_per_frame = r.u32()?;
        let final_frame_blocks = r.u32()?;
        let total_frames = r.u32()?;
        let bits_per_sample = r.u16()?;
        let channels = r.u16()?;
        let sample_rate = r.u32()?;
        r.skip(desc.header_bytes as usize - NEW_HEADER_LEN)?;

        // Seek table: u32[seek_table_bytes / 4].
        let seek_elements = (desc.seek_table_bytes / 4) as usize;
        let mut seek_table = Vec::with_capacity(seek_elements.min(1 << 20));
        for _ in 0..seek_elements {
            seek_table.push(r.u32()?);
        }
        r.skip((desc.seek_table_bytes % 4) as usize)?;

        // Stored WAV header blob.
        let wav_header = r.take(desc.header_data_bytes as usize)?.to_vec();

        if total_frames == 0 {
            return Err(Error::NonFinalized);
        }
        Ok(FileInfo {
            junk_bytes: junk,
            version,
            compression_level,
            format_flags,
            channels,
            sample_rate,
            bits_per_sample,
            blocks_per_frame,
            final_frame_blocks,
            total_frames,
            peak_level: None,
            seek_table,
            seek_bit_table: None,
            wav_header,
            audio_data_offset: r.pos,
            audio_data_len: desc.frame_data_len(),
            terminating_data_bytes: desc.terminating_data_bytes,
            file_md5: Some(desc.file_md5),
            descriptor: Some(desc),
            data_len: data.len(),
        })
    }

    /// §1.3 old-era walk: flat header → peak → seek count → WAV header
    /// blob → seek byte table → (≤ 3800) seek bit table.
    fn parse_old_era(data: &[u8], junk: usize, version: u16, mut r: Reader<'_>) -> Result<Self> {
        let compression_level = CompressionLevel::from_u16(r.u16()?)?;
        let format_flags = FormatFlags(r.u16()?);
        let channels = r.u16()?;
        let sample_rate = r.u32()?;
        let wav_header_bytes = r.u32()?;
        let terminating_bytes = r.u32()?;
        let total_frames = r.u32()?;
        let final_frame_blocks = r.u32()?;

        // Tail, in the pinned order.
        let peak_level = if format_flags.has(FormatFlags::HAS_PEAK_LEVEL) {
            Some(r.u32()?)
        } else {
            None
        };
        let seek_elements = if format_flags.has(FormatFlags::HAS_SEEK_ELEMENTS) {
            r.u32()? as usize
        } else {
            total_frames as usize
        };
        let wav_header = if format_flags.has(FormatFlags::CREATE_WAV_HEADER) {
            Vec::new()
        } else {
            r.take(wav_header_bytes as usize)?.to_vec()
        };
        let mut seek_table = Vec::with_capacity(seek_elements.min(1 << 20));
        for _ in 0..seek_elements {
            seek_table.push(r.u32()?);
        }
        let seek_bit_table = if version <= SEEK_BIT_TABLE_MAX_VERSION {
            Some(r.take(seek_elements)?.to_vec())
        } else {
            None
        };

        if total_frames == 0 {
            return Err(Error::NonFinalized);
        }
        let audio_data_offset = r.pos;
        // The flat header stores no frame-data byte count; derive it
        // from the buffer remainder minus the terminating blob.
        let audio_data_len = (data.len() as u64)
            .saturating_sub(audio_data_offset as u64)
            .saturating_sub(u64::from(terminating_bytes));
        Ok(FileInfo {
            junk_bytes: junk,
            version,
            compression_level,
            format_flags,
            channels,
            sample_rate,
            bits_per_sample: format_flags.bits_per_sample_old(),
            blocks_per_frame: old_blocks_per_frame(version, compression_level),
            final_frame_blocks,
            total_frames,
            peak_level,
            seek_table,
            seek_bit_table,
            wav_header,
            audio_data_offset,
            audio_data_len,
            terminating_data_bytes: terminating_bytes,
            file_md5: None,
            descriptor: None,
            data_len: data.len(),
        })
    }

    /// §1.7 `nBytesPerSample`.
    pub fn bytes_per_sample(&self) -> u16 {
        self.bits_per_sample / 8
    }

    /// §1.7 `nBlockAlign` (bytes per multichannel sample-frame).
    pub fn block_align(&self) -> u32 {
        u32::from(self.bytes_per_sample()) * u32::from(self.channels)
    }

    /// §1.7 `nTotalBlocks`: `(total_frames - 1) * blocks_per_frame +
    /// final_frame_blocks` (parse already rejects `total_frames == 0`).
    pub fn total_blocks(&self) -> u64 {
        u64::from(self.total_frames - 1) * u64::from(self.blocks_per_frame)
            + u64::from(self.final_frame_blocks)
    }

    /// §1.7 `nWAVHeaderBytes`: the synthesised 44 when the stream
    /// carries no header, else the stored blob size.
    pub fn wav_header_bytes(&self) -> u32 {
        if self.format_flags.has(FormatFlags::CREATE_WAV_HEADER) {
            SYNTHESISED_WAV_HEADER_LEN
        } else {
            self.wav_header.len() as u32
        }
    }

    /// Number of audio blocks in frame `index` (the final frame is
    /// short).
    pub fn frame_blocks(&self, index: u32) -> Result<u32> {
        if index >= self.total_frames {
            return Err(Error::Malformed("frame index past total_frames"));
        }
        Ok(if index + 1 == self.total_frames {
            self.final_frame_blocks
        } else {
            self.blocks_per_frame
        })
    }

    /// Absolute end of the compressed frame payload within the parsed
    /// buffer.
    pub fn audio_data_end(&self) -> u64 {
        self.audio_data_offset as u64 + self.audio_data_len
    }

    /// Byte range `[start, end)` of frame `index` within the parsed
    /// buffer, from the seek table (absolute per-frame byte offsets;
    /// the last frame runs to the end of the frame payload).
    pub fn frame_byte_range(&self, index: u32) -> Result<(u64, u64)> {
        let i = index as usize;
        if index >= self.total_frames {
            return Err(Error::Malformed("frame index past total_frames"));
        }
        let start = u64::from(
            *self
                .seek_table
                .get(i)
                .ok_or(Error::Malformed("seek table shorter than total_frames"))?,
        );
        let end = match self.seek_table.get(i + 1) {
            Some(&next) if u64::from(next) <= self.audio_data_end() => u64::from(next),
            _ => self.audio_data_end(),
        };
        if start < self.audio_data_offset as u64 || end < start {
            return Err(Error::Malformed("seek table entry outside frame payload"));
        }
        Ok((start, end))
    }

    /// Length of the buffer the info was parsed from.
    pub fn data_len(&self) -> usize {
        self.data_len
    }
}

/// §1.4 old-era blocks-per-frame derivation: 9216, promoted to 73728
/// for `version >= 3900` (or `>= 3800` at the extra-high level), and
/// to 294912 (`73728 * 4`) for `version >= 3950`.
pub fn old_blocks_per_frame(version: u16, level: CompressionLevel) -> u32 {
    if version >= 3950 {
        73728 * 4
    } else if version >= 3900 || (version >= 3800 && level == CompressionLevel::ExtraHigh) {
        73728
    } else {
        9216
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le16(v: u16) -> [u8; 2] {
        v.to_le_bytes()
    }
    fn le32(v: u32) -> [u8; 4] {
        v.to_le_bytes()
    }

    /// Assemble a §1.1/§1.2 new-era file head with the given knobs and
    /// `payload` as the frame data.
    #[allow(clippy::too_many_arguments)]
    fn build_new_era(
        version: u16,
        level: u16,
        flags: u16,
        blocks_per_frame: u32,
        final_frame_blocks: u32,
        seek_entries: &[u32],
        wav_blob: &[u8],
        payload: &[u8],
        descriptor_surplus: usize,
        header_surplus: usize,
    ) -> Vec<u8> {
        let total_frames = seek_entries.len() as u32;
        let mut f = Vec::new();
        f.extend_from_slice(&MAGIC);
        f.extend_from_slice(&le16(version));
        f.extend_from_slice(&le16(0)); // alignment gap
        f.extend_from_slice(&le32((DESCRIPTOR_LEN + descriptor_surplus) as u32));
        f.extend_from_slice(&le32((NEW_HEADER_LEN + header_surplus) as u32));
        f.extend_from_slice(&le32(seek_entries.len() as u32 * 4));
        f.extend_from_slice(&le32(wav_blob.len() as u32));
        f.extend_from_slice(&le32(payload.len() as u32));
        f.extend_from_slice(&le32(0)); // high half
        f.extend_from_slice(&le32(7)); // terminating bytes
        f.extend_from_slice(&[0xA5; 16]); // md5
        f.extend(std::iter::repeat_n(0xEE, descriptor_surplus));
        // Header block.
        f.extend_from_slice(&le16(level));
        f.extend_from_slice(&le16(flags));
        f.extend_from_slice(&le32(blocks_per_frame));
        f.extend_from_slice(&le32(final_frame_blocks));
        f.extend_from_slice(&le32(total_frames));
        f.extend_from_slice(&le16(16));
        f.extend_from_slice(&le16(2));
        f.extend_from_slice(&le32(44100));
        f.extend(std::iter::repeat_n(0xDD, header_surplus));
        for &s in seek_entries {
            f.extend_from_slice(&le32(s));
        }
        f.extend_from_slice(wav_blob);
        f.extend_from_slice(payload);
        f.extend_from_slice(&[0xCC; 7]); // terminating blob
        f
    }

    /// Assemble a §1.3 old-era file with the pinned tail order.
    #[allow(clippy::too_many_arguments)]
    fn build_old_era(
        version: u16,
        level: u16,
        flags: u16,
        total_frames: u32,
        final_frame_blocks: u32,
        wav_blob: &[u8],
        seek_entries: &[u32],
        payload: &[u8],
    ) -> Vec<u8> {
        let mut f = Vec::new();
        f.extend_from_slice(&MAGIC);
        f.extend_from_slice(&le16(version));
        f.extend_from_slice(&le16(level));
        f.extend_from_slice(&le16(flags));
        f.extend_from_slice(&le16(2)); // channels
        f.extend_from_slice(&le32(44100));
        f.extend_from_slice(&le32(wav_blob.len() as u32));
        f.extend_from_slice(&le32(5)); // terminating bytes
        f.extend_from_slice(&le32(total_frames));
        f.extend_from_slice(&le32(final_frame_blocks));
        if flags & FormatFlags::HAS_PEAK_LEVEL != 0 {
            f.extend_from_slice(&le32(31000));
        }
        if flags & FormatFlags::HAS_SEEK_ELEMENTS != 0 {
            f.extend_from_slice(&le32(seek_entries.len() as u32));
        }
        if flags & FormatFlags::CREATE_WAV_HEADER == 0 {
            f.extend_from_slice(wav_blob);
        }
        for &s in seek_entries {
            f.extend_from_slice(&le32(s));
        }
        if version <= SEEK_BIT_TABLE_MAX_VERSION {
            f.extend(std::iter::repeat_n(3u8, seek_entries.len()));
        }
        f.extend_from_slice(payload);
        f.extend_from_slice(&[0xBB; 5]); // terminating blob
        f
    }

    #[test]
    fn new_era_layout_round_trips_every_field() {
        // Compute the audio start by construction: 52 + 24 + 8 + 5.
        let audio_start = (DESCRIPTOR_LEN + NEW_HEADER_LEN + 8 + 5) as u32;
        let payload = [0x11u8; 40];
        let file = build_new_era(
            3990,
            2000,
            0,
            0x48000,
            123,
            &[audio_start, audio_start + 25],
            b"RIFFx",
            &payload,
            0,
            0,
        );
        let info = FileInfo::parse(&file).unwrap();
        assert_eq!(info.junk_bytes, 0);
        assert_eq!(info.version, 3990);
        assert_eq!(info.compression_level, CompressionLevel::Normal);
        assert_eq!(info.format_flags, FormatFlags(0));
        assert_eq!(info.channels, 2);
        assert_eq!(info.sample_rate, 44100);
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.blocks_per_frame, 0x48000);
        assert_eq!(info.final_frame_blocks, 123);
        assert_eq!(info.total_frames, 2);
        assert_eq!(info.seek_table, vec![audio_start, audio_start + 25]);
        assert_eq!(info.wav_header, b"RIFFx");
        assert_eq!(info.audio_data_offset, audio_start as usize);
        assert_eq!(info.audio_data_len, payload.len() as u64);
        assert_eq!(info.terminating_data_bytes, 7);
        assert_eq!(info.file_md5, Some([0xA5; 16]));
        let desc = info.descriptor.unwrap();
        assert_eq!(desc.frame_data_len(), payload.len() as u64);
        // Derived quantities (§1.7).
        assert_eq!(info.bytes_per_sample(), 2);
        assert_eq!(info.block_align(), 4);
        assert_eq!(info.total_blocks(), 0x48000 + 123);
        assert_eq!(info.wav_header_bytes(), 5);
        // Frame slicing off the seek table.
        assert_eq!(
            info.frame_byte_range(0).unwrap(),
            (u64::from(audio_start), u64::from(audio_start) + 25)
        );
        assert_eq!(
            info.frame_byte_range(1).unwrap(),
            (u64::from(audio_start) + 25, info.audio_data_end())
        );
        assert_eq!(info.frame_blocks(0).unwrap(), 0x48000);
        assert_eq!(info.frame_blocks(1).unwrap(), 123);
    }

    #[test]
    fn new_era_skips_descriptor_and_header_surplus() {
        // Forward compat: both length fields exceed the fixed sizes and
        // the parser must skip the surplus before the next block.
        let audio_start = (DESCRIPTOR_LEN + 12 + NEW_HEADER_LEN + 6 + 4) as u32;
        let file = build_new_era(
            3995,
            1000,
            0,
            0x48000,
            9,
            &[audio_start],
            b"",
            &[0x22; 16],
            12,
            6,
        );
        let info = FileInfo::parse(&file).unwrap();
        assert_eq!(info.compression_level, CompressionLevel::Fast);
        assert_eq!(info.audio_data_offset, audio_start as usize);
        assert_eq!(info.total_frames, 1);
    }

    #[test]
    fn new_era_rejects_undersized_descriptor_and_header() {
        let mut file = build_new_era(3990, 2000, 0, 1, 1, &[100], b"", &[0; 4], 0, 0);
        // Corrupt descriptor_bytes to 51.
        file[8..12].copy_from_slice(&le32(51));
        assert_eq!(
            FileInfo::parse(&file).unwrap_err(),
            Error::Malformed("descriptor shorter than 52 bytes")
        );
        let mut file2 = build_new_era(3990, 2000, 0, 1, 1, &[100], b"", &[0; 4], 0, 0);
        file2[12..16].copy_from_slice(&le32(23));
        assert_eq!(
            FileInfo::parse(&file2).unwrap_err(),
            Error::Malformed("header block shorter than 24 bytes")
        );
    }

    #[test]
    fn new_era_rejects_zero_total_frames_as_non_finalized() {
        let file = build_new_era(3990, 2000, 0, 1, 1, &[], b"", &[], 0, 0);
        assert_eq!(FileInfo::parse(&file).unwrap_err(), Error::NonFinalized);
    }

    #[test]
    fn junk_prefix_is_skipped_and_recorded() {
        let audio_start = (DESCRIPTOR_LEN + NEW_HEADER_LEN + 4) as u32;
        let inner = build_new_era(
            3990,
            3000,
            0,
            64,
            64,
            &[audio_start + 10],
            b"",
            &[0; 8],
            0,
            0,
        );
        let mut file = b"ID3\x04\x00junkjunk".to_vec();
        let junk = file.len();
        file.extend_from_slice(&inner);
        let info = FileInfo::parse(&file).unwrap();
        assert_eq!(info.junk_bytes, junk);
        assert_eq!(info.compression_level, CompressionLevel::High);
        // The audio offset is absolute within the parsed buffer:
        // junk + descriptor + header + one seek entry.
        assert_eq!(info.audio_data_offset, junk + audio_start as usize);
    }

    #[test]
    fn old_era_flat_header_and_tail_order() {
        // Flags: peak level + seek elements + stored WAV header.
        let flags = FormatFlags::HAS_PEAK_LEVEL | FormatFlags::HAS_SEEK_ELEMENTS;
        let payload = [0x33u8; 30];
        let file = build_old_era(3920, 2000, flags, 2, 77, b"RIFFRIFF", &[100, 115], &payload);
        let info = FileInfo::parse(&file).unwrap();
        assert_eq!(info.version, 3920);
        assert_eq!(info.compression_level, CompressionLevel::Normal);
        assert_eq!(info.peak_level, Some(31000));
        assert_eq!(info.seek_table, vec![100, 115]);
        assert_eq!(info.seek_bit_table, None, "3920 > 3800: no bit table");
        assert_eq!(info.wav_header, b"RIFFRIFF");
        assert_eq!(info.bits_per_sample, 16);
        assert_eq!(info.blocks_per_frame, 73728, "3920 -> 73728");
        assert_eq!(info.total_frames, 2);
        assert_eq!(info.final_frame_blocks, 77);
        assert_eq!(info.terminating_data_bytes, 5);
        assert_eq!(info.file_md5, None);
        assert_eq!(info.descriptor, None);
        // Audio region: everything after the tail minus terminating.
        assert_eq!(info.audio_data_len, payload.len() as u64);
        assert_eq!(
            info.audio_data_offset,
            OLD_HEADER_LEN + 4 + 4 + 8 + 8 // peak + count + wav + seek
        );
    }

    #[test]
    fn old_era_seek_bit_table_present_at_or_below_3800() {
        let file = build_old_era(3800, 1000, 0, 3, 9, b"", &[50, 60, 70], &[0; 25]);
        let info = FileInfo::parse(&file).unwrap();
        assert_eq!(info.seek_bit_table, Some(vec![3, 3, 3]));
        assert_eq!(info.blocks_per_frame, 9216, "3800 fast stays 9216");
        // Without HAS_SEEK_ELEMENTS the element count falls back to
        // total_frames.
        assert_eq!(info.seek_table.len(), 3);
    }

    #[test]
    fn old_era_create_wav_header_stores_no_blob() {
        let file = build_old_era(
            3970,
            5000,
            FormatFlags::CREATE_WAV_HEADER,
            1,
            5,
            b"IGNORED!",
            &[44],
            &[0; 10],
        );
        let info = FileInfo::parse(&file).unwrap();
        assert!(info.wav_header.is_empty());
        // §1.7: synthesise 44 bytes on decode.
        assert_eq!(info.wav_header_bytes(), SYNTHESISED_WAV_HEADER_LEN);
        assert_eq!(info.blocks_per_frame, 73728 * 4, "3970 -> 294912");
    }

    #[test]
    fn old_era_bits_per_sample_follow_the_flags() {
        for (flags, bits) in [
            (0u16, 16u16),
            (FormatFlags::EIGHT_BIT, 8),
            (FormatFlags::TWENTY_FOUR_BIT, 24),
            // 8-bit wins when both obsolete flags are set.
            (FormatFlags::EIGHT_BIT | FormatFlags::TWENTY_FOUR_BIT, 8),
        ] {
            let file = build_old_era(3920, 2000, flags, 1, 1, b"", &[40], &[0; 6]);
            let info = FileInfo::parse(&file).unwrap();
            assert_eq!(info.bits_per_sample, bits, "flags {flags:#x}");
        }
    }

    #[test]
    fn old_blocks_per_frame_matches_the_staged_ladder() {
        use CompressionLevel as L;
        assert_eq!(old_blocks_per_frame(3790, L::Normal), 9216);
        assert_eq!(old_blocks_per_frame(3800, L::Normal), 9216);
        assert_eq!(old_blocks_per_frame(3800, L::ExtraHigh), 73728);
        assert_eq!(old_blocks_per_frame(3890, L::ExtraHigh), 73728);
        assert_eq!(old_blocks_per_frame(3890, L::Insane), 9216);
        assert_eq!(old_blocks_per_frame(3900, L::Fast), 73728);
        assert_eq!(old_blocks_per_frame(3949, L::Normal), 73728);
        assert_eq!(old_blocks_per_frame(3950, L::Normal), 294912);
        assert_eq!(old_blocks_per_frame(3979, L::Insane), 294912);
    }

    #[test]
    fn truncated_buffers_error_cleanly_at_every_length() {
        // Every prefix of a well-formed new-era file must produce a
        // clean error (or parse, once all pinned blocks are present) —
        // never a panic.
        let audio_start = (DESCRIPTOR_LEN + NEW_HEADER_LEN + 4 + 3) as u32;
        let full = build_new_era(3990, 4000, 0, 64, 4, &[audio_start], b"abc", &[0; 12], 0, 0);
        for len in 0..full.len() {
            match FileInfo::parse(&full[..len]) {
                Ok(info) => {
                    // Parse succeeds once the WAV blob is in-buffer;
                    // the audio region then derives from the shorter
                    // buffer view.
                    assert!(len >= audio_start as usize);
                    assert_eq!(info.total_frames, 1);
                }
                Err(
                    Error::Truncated
                    | Error::InvalidMagic
                    | Error::Malformed(_)
                    | Error::NonFinalized,
                ) => {}
                Err(other) => panic!("unexpected error {other:?} at length {len}"),
            }
        }
        // Same walk for the old era.
        let full_old = build_old_era(3820, 2000, 0, 2, 2, b"hdr", &[50, 55], &[0; 9]);
        for len in 0..full_old.len() {
            match FileInfo::parse(&full_old[..len]) {
                Ok(_) => {}
                Err(
                    Error::Truncated
                    | Error::InvalidMagic
                    | Error::Malformed(_)
                    | Error::NonFinalized,
                ) => {}
                Err(other) => panic!("unexpected error {other:?} at length {len}"),
            }
        }
    }

    #[test]
    fn frame_range_guards_reject_bad_seek_geometry() {
        let audio_start = (DESCRIPTOR_LEN + NEW_HEADER_LEN + 8) as u32;
        // Seek entry pointing before the audio region.
        let file = build_new_era(3990, 2000, 0, 64, 4, &[4, audio_start], b"", &[0; 10], 0, 0);
        let info = FileInfo::parse(&file).unwrap();
        assert!(matches!(info.frame_byte_range(0), Err(Error::Malformed(_))));
        // Index past the frame count.
        assert!(matches!(info.frame_byte_range(9), Err(Error::Malformed(_))));
        assert!(matches!(info.frame_blocks(9), Err(Error::Malformed(_))));
    }

    #[test]
    fn missing_magic_is_invalid_magic() {
        assert_eq!(
            FileInfo::parse(b"no monkeys here").unwrap_err(),
            Error::InvalidMagic
        );
        assert_eq!(FileInfo::parse(b"").unwrap_err(), Error::InvalidMagic);
    }

    #[test]
    fn format_flags_query_matches_the_staged_bit_table() {
        let all = FormatFlags(0b111111);
        for bit in [
            FormatFlags::EIGHT_BIT,
            FormatFlags::CRC,
            FormatFlags::HAS_PEAK_LEVEL,
            FormatFlags::TWENTY_FOUR_BIT,
            FormatFlags::HAS_SEEK_ELEMENTS,
            FormatFlags::CREATE_WAV_HEADER,
        ] {
            assert!(all.has(bit));
            assert!(!FormatFlags(0).has(bit));
        }
        assert_eq!(FormatFlags::EIGHT_BIT, 1);
        assert_eq!(FormatFlags::CRC, 2);
        assert_eq!(FormatFlags::HAS_PEAK_LEVEL, 4);
        assert_eq!(FormatFlags::TWENTY_FOUR_BIT, 8);
        assert_eq!(FormatFlags::HAS_SEEK_ELEMENTS, 16);
        assert_eq!(FormatFlags::CREATE_WAV_HEADER, 32);
    }
}
