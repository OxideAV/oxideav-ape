//! Monkey's Audio encoder — PCM in, a complete 3990-era `.ape` file
//! out.
//!
//! The encoder is the exact inverse of the decode pipeline the staged
//! `docs/audio/ape/format-reference.md` pins, stage by stage in
//! reverse order, and the decoder is its oracle: every stage's mirror
//! is the algebraic inverse of a decode step the format reference
//! fixes, holding an identical state trajectory, so the byte stream
//! this module writes decodes back to the input PCM bit-exactly.
//!
//! Per frame (§4, §6.1, §6.9, §6.10):
//!
//! 1. **Frame flags.** A silent PCM channel sets its §4.2 flag (mono /
//!    stored channel 1 → bit 0, stored channel 0 → bit 1); identical
//!    stereo channels set bit 2 (pseudo-stereo) and code a single
//!    array. A frame with any flag set carries the second prologue
//!    word (bit 31 of the CRC word marks it).
//! 2. **Channel decorrelation** (§6.9): `Y = s1 - s0`, `X = s0 + Y/2`
//!    with truncating division; mono / pseudo-stereo code `X = s0`.
//! 3. **Predictor chain** (§6.3 reversed): the scaled first-order
//!    stage, the integer offset predictor, then the NN cascade in
//!    construction order, each in its compress direction with the
//!    §6.1 cross terms (`Y` fed the previous block's `X`, `X` fed this
//!    block's `Y`) — [`crate::pcm::pcm_to_coded_arrays`].
//! 4. **Entropy coding** (§2.6 mirrored): the residuals fold per §2.9
//!    and range-code against the ≥ 3990 model with the `KSum` pivot,
//!    the 32-bit overflow escape and the radix split; stereo arrays
//!    interleave per sample over one coder with independent running
//!    states (§4.4), each frame starting from `k = 10, KSum = 16384`
//!    (§6.10.2).
//! 5. **Frame bytes** (§4.2, §4.3): `crc32(frame PCM bytes) >> 1` (bit
//!    31 = flags follow), the optional flags word, the one structural
//!    pad byte, then the coder payload — silent frames included (the
//!    vendor writes the pad and a flushed empty coder even when no
//!    symbol follows).
//!
//! The frames concatenate into one logical byte stream, which the
//! container writer lays out in the §4.1 little-endian-word order and
//! wraps in the §1 descriptor / header / seek table / WAV header blob,
//! with the §1.8 `cFileMD5` ([`crate::writer`]).

use crate::entropy::ResidualEncoder;
use crate::error::{Error, Result};
use crate::frame::{crc32, FrameFlags, FRAME_ENTROPY_INIT, FRAME_PRIME_PAD_BYTES};
use crate::header::CompressionLevel;
use crate::pcm::{deinterleave_pcm_bytes, interleave_pcm_bytes, pcm_to_coded_arrays};
use crate::writer::{canonical_wav_header, to_le_word_layout, FileLayout, ENCODER_FILE_VERSION};

/// Default blocks per frame — the value the vendor encoder writes for
/// every fixture in the corpus (§1.2; 8 kHz mono and 44.1 kHz stereo
/// alike).
pub const DEFAULT_BLOCKS_PER_FRAME: u32 = 73728;

/// Encoder configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderConfig {
    /// Compression profile — selects the NN cascade (§6.4).
    pub compression_level: CompressionLevel,
    /// Channel count (1 or 2 — the staged material covers no more).
    pub channels: u16,
    /// Sample rate in Hz (stored; the codec itself is rate-agnostic).
    pub sample_rate: u32,
    /// Bits per sample: 8, 16, or 24 (§6.9 reassembly table).
    pub bits_per_sample: u16,
    /// Blocks (sample frames) per non-final frame.
    pub blocks_per_frame: u32,
    /// Source WAV header to store verbatim (§1.1 `nHeaderDataBytes`);
    /// `None` synthesises the canonical 44-byte header at finish.
    pub wav_header: Option<Vec<u8>>,
    /// Bytes that followed the source WAV's PCM data, stored verbatim
    /// as the terminating blob (§1.1 `nTerminatingDataBytes`).
    pub terminating_data: Vec<u8>,
    /// Trailing tag bytes (an APEv2 / ID3v1 tag) appended verbatim
    /// after the terminating blob — pass-through only; this crate
    /// neither parses nor synthesises tags.
    pub trailing_tag: Vec<u8>,
}

impl EncoderConfig {
    /// A configuration for `channels` × `sample_rate` × `bits_per_sample`
    /// PCM at `level`, with the default frame size, a synthesised WAV
    /// header, and no trailing blobs.
    pub fn new(
        level: CompressionLevel,
        channels: u16,
        sample_rate: u32,
        bits_per_sample: u16,
    ) -> Self {
        EncoderConfig {
            compression_level: level,
            channels,
            sample_rate,
            bits_per_sample,
            blocks_per_frame: DEFAULT_BLOCKS_PER_FRAME,
            wav_header: None,
            terminating_data: Vec::new(),
            trailing_tag: Vec::new(),
        }
    }

    /// Validate the configuration against what the staged material
    /// covers.
    pub fn validate(&self) -> Result<()> {
        if !(1..=2).contains(&self.channels) {
            return Err(Error::InvalidInput("channel count outside 1..=2"));
        }
        if !matches!(self.bits_per_sample, 8 | 16 | 24) {
            return Err(Error::InvalidInput("bits per sample outside {8, 16, 24}"));
        }
        if self.blocks_per_frame == 0 || self.blocks_per_frame > crate::frame::MAX_FRAME_BLOCKS {
            return Err(Error::InvalidInput("blocks per frame outside 1..=2^20"));
        }
        if self.sample_rate == 0 {
            return Err(Error::InvalidInput("sample rate must be non-zero"));
        }
        Ok(())
    }

    /// Bytes per interleaved sample frame (§1.7 `nBlockAlign`).
    pub fn block_align(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits_per_sample / 8)
    }

    /// Inclusive sample range the bit depth can store.
    pub fn sample_range(&self) -> (i32, i32) {
        let half = 1i32 << (self.bits_per_sample - 1);
        (-half, half - 1)
    }
}

/// One encoded frame: its flags and its logical byte stream (prologue,
/// pad, coder payload — before the §4.1 word layout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodedFrame {
    /// The §4.2 frame flags (`0` when no flags word is written).
    pub flags: FrameFlags,
    /// Blocks in the frame.
    pub blocks: u32,
    /// The stored CRC field (`crc32(frame PCM bytes) >> 1`).
    pub crc31: u32,
    /// Logical frame bytes.
    pub bytes: Vec<u8>,
}

/// §4.2 flag derivation for one frame's per-channel PCM.
pub fn frame_flags(pcm: &[Vec<i32>]) -> FrameFlags {
    let silent = |ch: &Vec<i32>| ch.iter().all(|&v| v == 0);
    let mut flags = 0u32;
    match pcm {
        [mono] => {
            if silent(mono) {
                flags |= FrameFlags::CH0_SILENT;
            }
        }
        [s0, s1] => {
            // Empirical bit assignment (§4.2 / `crate::frame`): the
            // stored *second* channel's silence is bit 0, the stored
            // first channel's is bit 1.
            if silent(s1) {
                flags |= FrameFlags::CH0_SILENT;
            }
            if silent(s0) {
                flags |= FrameFlags::CH1_SILENT;
            }
            if s0 == s1 {
                flags |= FrameFlags::PSEUDO_STEREO;
            }
        }
        _ => {}
    }
    FrameFlags(flags)
}

/// Entropy-code one frame's coded arrays exactly as the frame layer
/// decodes them (§4.4): one shared coder, per-sample channel
/// interleave with independent per-channel running states, the
/// §6.10.2 init per array.
fn entropy_code(arrays: &[Vec<i32>]) -> Result<Vec<u8>> {
    let mut enc = ResidualEncoder::new(ENCODER_FILE_VERSION, FRAME_ENTROPY_INIT);
    match arrays {
        [] => {}
        [single] => {
            for &r in single {
                enc.encode_residual(r)?;
            }
        }
        [a, b] => {
            let mut states = [FRAME_ENTROPY_INIT; 2];
            for i in 0..a.len() {
                for (ch, arr) in [a, b].into_iter().enumerate() {
                    enc.reset_state(states[ch]);
                    enc.encode_residual(arr[i])?;
                    states[ch] = enc.running_state();
                }
            }
        }
        _ => return Err(Error::InvalidInput("more than two coded arrays")),
    }
    Ok(enc.finish())
}

/// Encode one frame of per-channel PCM (`pcm.len()` is the channel
/// count, every channel the same length) at `level` / `bits_per_sample`
/// into its logical byte stream.
pub fn encode_frame(
    pcm: &[Vec<i32>],
    level: CompressionLevel,
    bits_per_sample: u16,
) -> Result<EncodedFrame> {
    let blocks = pcm.first().map_or(0, Vec::len);
    if pcm.is_empty() || pcm.len() > 2 {
        return Err(Error::InvalidInput("channel count outside 1..=2"));
    }
    if pcm.iter().any(|c| c.len() != blocks) {
        return Err(Error::InvalidInput("PCM channels disagree on length"));
    }
    let half = 1i64 << (bits_per_sample - 1);
    if pcm
        .iter()
        .flatten()
        .any(|&v| i64::from(v) < -half || i64::from(v) >= half)
    {
        return Err(Error::InvalidInput("sample outside the bit depth's range"));
    }
    let pcm_bytes = interleave_pcm_bytes(pcm, bits_per_sample)?;
    let flags = frame_flags(pcm);
    let channels = pcm.len() as u16;

    // The coded arrays: none for a fully-silent frame, one for mono or
    // pseudo-stereo (X = s0, cross term 0 — §6.1), two otherwise.
    let coded: Vec<Vec<i32>> = if flags.all_silent(channels) {
        Vec::new()
    } else if channels == 2 && flags.has(FrameFlags::PSEUDO_STEREO) {
        pcm_to_coded_arrays(&pcm[..1], ENCODER_FILE_VERSION, level)?
    } else {
        pcm_to_coded_arrays(pcm, ENCODER_FILE_VERSION, level)?
    };
    let payload = entropy_code(&coded)?;

    let crc31 = crc32(&pcm_bytes) >> 1;
    let mut bytes = Vec::with_capacity(8 + FRAME_PRIME_PAD_BYTES + payload.len());
    if flags.0 != 0 {
        bytes.extend_from_slice(&(crc31 | 0x8000_0000).to_be_bytes());
        bytes.extend_from_slice(&flags.0.to_be_bytes());
    } else {
        bytes.extend_from_slice(&crc31.to_be_bytes());
    }
    bytes.extend_from_slice(&[0u8; FRAME_PRIME_PAD_BYTES]);
    bytes.extend_from_slice(&payload);
    Ok(EncodedFrame {
        flags,
        blocks: blocks as u32,
        crc31,
        bytes,
    })
}

/// Streaming whole-file encoder: push PCM in any chunking, then
/// [`finish`](Self::finish) for the complete `.ape` file bytes.
#[derive(Debug, Clone)]
pub struct ApeEncoder {
    cfg: EncoderConfig,
    /// Buffered per-channel samples not yet forming a whole frame.
    pending: Vec<Vec<i32>>,
    /// Logical frame stream so far.
    logical: Vec<u8>,
    /// Per-frame offsets into `logical`.
    frame_offsets: Vec<u32>,
    /// Blocks of the most recently encoded frame.
    last_frame_blocks: u32,
    /// Total blocks encoded so far (frames + pending).
    total_blocks: u64,
}

impl ApeEncoder {
    /// Create an encoder for `cfg` (validated up front).
    pub fn new(cfg: EncoderConfig) -> Result<Self> {
        cfg.validate()?;
        let pending = vec![Vec::new(); usize::from(cfg.channels)];
        Ok(ApeEncoder {
            cfg,
            pending,
            logical: Vec::new(),
            frame_offsets: Vec::new(),
            last_frame_blocks: 0,
            total_blocks: 0,
        })
    }

    /// The configuration.
    pub fn config(&self) -> &EncoderConfig {
        &self.cfg
    }

    /// Blocks pushed so far.
    pub fn total_blocks(&self) -> u64 {
        self.total_blocks
    }

    /// Frames encoded so far (a partial frame in the buffer is not
    /// counted until it is flushed at finish).
    pub fn frames_encoded(&self) -> usize {
        self.frame_offsets.len()
    }

    /// Push per-channel samples (`channels.len()` must equal the
    /// configured channel count; every channel the same length; each
    /// sample inside the bit depth's range).
    pub fn push_samples(&mut self, channels: &[&[i32]]) -> Result<()> {
        if channels.len() != usize::from(self.cfg.channels) {
            return Err(Error::InvalidInput(
                "pushed channel count disagrees with the configuration",
            ));
        }
        let n = channels[0].len();
        if channels.iter().any(|c| c.len() != n) {
            return Err(Error::InvalidInput("PCM channels disagree on length"));
        }
        let (lo, hi) = self.cfg.sample_range();
        if channels
            .iter()
            .flat_map(|c| c.iter())
            .any(|&v| v < lo || v > hi)
        {
            return Err(Error::InvalidInput("sample outside the bit depth's range"));
        }
        for (buf, ch) in self.pending.iter_mut().zip(channels) {
            buf.extend_from_slice(ch);
        }
        self.total_blocks += n as u64;
        self.drain_full_frames()
    }

    /// Push interleaved little-endian PCM bytes in the §6.9 stored
    /// layout (`U8` biased by 128, `S16`, `S24`). The buffer must hold
    /// a whole number of sample frames.
    pub fn push_interleaved_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        let chans = deinterleave_pcm_bytes(bytes, self.cfg.bits_per_sample, self.cfg.channels)?;
        let refs: Vec<&[i32]> = chans.iter().map(Vec::as_slice).collect();
        self.push_samples(&refs)
    }

    fn drain_full_frames(&mut self) -> Result<()> {
        let bpf = self.cfg.blocks_per_frame as usize;
        while self.pending[0].len() >= bpf {
            let frame: Vec<Vec<i32>> = self
                .pending
                .iter_mut()
                .map(|buf| buf.drain(..bpf).collect())
                .collect();
            self.emit_frame(&frame)?;
        }
        Ok(())
    }

    fn emit_frame(&mut self, pcm: &[Vec<i32>]) -> Result<()> {
        let f = encode_frame(pcm, self.cfg.compression_level, self.cfg.bits_per_sample)?;
        self.frame_offsets.push(self.logical.len() as u32);
        self.logical.extend_from_slice(&f.bytes);
        self.last_frame_blocks = f.blocks;
        Ok(())
    }

    /// Flush the partial final frame and serialise the file.
    ///
    /// An empty input yields one zero-block final frame (the layout
    /// needs `total_frames >= 1` — §1.7 treats `0` as non-finalised).
    pub fn finish(mut self) -> Result<Vec<u8>> {
        if !self.pending[0].is_empty() || self.frame_offsets.is_empty() {
            let frame: Vec<Vec<i32>> = self.pending.iter_mut().map(core::mem::take).collect();
            self.emit_frame(&frame)?;
        }
        let total_frames = self.frame_offsets.len() as u32;
        let data_len = self.total_blocks * self.cfg.block_align() as u64;
        let wav_header = match self.cfg.wav_header.take() {
            Some(h) => h,
            None => canonical_wav_header(
                self.cfg.channels,
                self.cfg.sample_rate,
                self.cfg.bits_per_sample,
                u32::try_from(data_len).unwrap_or(u32::MAX),
            ),
        };
        let audio_start = crate::file_header::DESCRIPTOR_LEN
            + crate::file_header::NEW_HEADER_LEN
            + self.frame_offsets.len() * 4
            + wav_header.len();
        let audio_start = u32::try_from(audio_start)
            .map_err(|_| Error::InvalidInput("header region exceeds 32 bits"))?;
        let seek_table: Vec<u32> = self
            .frame_offsets
            .iter()
            .map(|&o| audio_start.wrapping_add(o))
            .collect();
        let layout = FileLayout {
            descriptor_padding: [0; 2],
            compression_level: self.cfg.compression_level,
            format_flags: 0,
            blocks_per_frame: self.cfg.blocks_per_frame,
            final_frame_blocks: self.last_frame_blocks,
            total_frames,
            bits_per_sample: self.cfg.bits_per_sample,
            channels: self.cfg.channels,
            sample_rate: self.cfg.sample_rate,
            seek_table,
            wav_header,
            frame_data: to_le_word_layout(core::mem::take(&mut self.logical)),
            terminating_data: core::mem::take(&mut self.cfg.terminating_data),
        };
        let mut file = layout.serialize();
        file.extend_from_slice(&self.cfg.trailing_tag);
        Ok(file)
    }
}

/// One-shot: encode per-channel PCM under `cfg` into a complete file.
pub fn encode_pcm(cfg: EncoderConfig, channels: &[Vec<i32>]) -> Result<Vec<u8>> {
    let mut enc = ApeEncoder::new(cfg)?;
    let refs: Vec<&[i32]> = channels.iter().map(Vec::as_slice).collect();
    enc.push_samples(&refs)?;
    enc.finish()
}

/// A RIFF/WAVE file split into the three blobs the container stores
/// (§1.1): everything through the `data` chunk header, the PCM
/// payload, and whatever trails it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WavSplit<'a> {
    /// Channel count from the `fmt ` chunk.
    pub channels: u16,
    /// Sample rate from the `fmt ` chunk.
    pub sample_rate: u32,
    /// Bits per sample from the `fmt ` chunk.
    pub bits_per_sample: u16,
    /// Bytes from the RIFF magic through the `data` chunk header.
    pub header: &'a [u8],
    /// The PCM payload (a whole number of sample frames).
    pub pcm: &'a [u8],
    /// Bytes after the PCM payload (trailing chunks / padding).
    pub terminating: &'a [u8],
}

impl<'a> WavSplit<'a> {
    /// Split a canonical-layout WAV (`RIFF` → `WAVE` → chunks with a
    /// `fmt ` chunk before the `data` chunk; integer PCM only).
    pub fn parse(wav: &'a [u8]) -> Result<Self> {
        let rd = |pos: usize, n: usize| -> Result<&'a [u8]> {
            wav.get(pos..pos + n)
                .ok_or(Error::InvalidInput("WAV truncated"))
        };
        let u16_at = |pos: usize| -> Result<u16> {
            let b = rd(pos, 2)?;
            Ok(u16::from_le_bytes([b[0], b[1]]))
        };
        let u32_at = |pos: usize| -> Result<u32> {
            let b = rd(pos, 4)?;
            Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        };
        if rd(0, 4)? != b"RIFF" || rd(8, 4)? != b"WAVE" {
            return Err(Error::InvalidInput("not a RIFF/WAVE file"));
        }
        let mut pos = 12usize;
        let mut fmt: Option<(u16, u16, u32, u16)> = None;
        loop {
            let id = rd(pos, 4)?;
            let len = u32_at(pos + 4)? as usize;
            let body = pos + 8;
            if id == b"fmt " {
                if len < 16 {
                    return Err(Error::InvalidInput("fmt chunk shorter than 16 bytes"));
                }
                fmt = Some((
                    u16_at(body)?,
                    u16_at(body + 2)?,
                    u32_at(body + 4)?,
                    u16_at(body + 14)?,
                ));
            } else if id == b"data" {
                let (tag, channels, sample_rate, bits) =
                    fmt.ok_or(Error::InvalidInput("data chunk precedes fmt chunk"))?;
                if tag != 1 && tag != 0xFFFE {
                    return Err(Error::InvalidInput("WAV is not integer PCM"));
                }
                let align = usize::from(channels) * usize::from(bits / 8);
                if align == 0 {
                    return Err(Error::InvalidInput("fmt chunk has zero block align"));
                }
                let avail = wav.len().saturating_sub(body);
                let mut data_len = len.min(avail);
                data_len -= data_len % align;
                return Ok(WavSplit {
                    channels,
                    sample_rate,
                    bits_per_sample: bits,
                    header: &wav[..body],
                    pcm: &wav[body..body + data_len],
                    terminating: &wav[body + data_len..],
                });
            }
            // Chunks are word-aligned (odd lengths carry a pad byte).
            pos = body
                .checked_add(len + (len & 1))
                .ok_or(Error::InvalidInput("WAV chunk length overflow"))?;
            if pos >= wav.len() {
                return Err(Error::InvalidInput("WAV has no data chunk"));
            }
        }
    }
}

/// One-shot: encode a RIFF/WAVE file at `level`, storing its header and
/// trailing bytes verbatim (§1.1 blob pass-through).
pub fn encode_wav(wav: &[u8], level: CompressionLevel) -> Result<Vec<u8>> {
    let split = WavSplit::parse(wav)?;
    let mut cfg = EncoderConfig::new(
        level,
        split.channels,
        split.sample_rate,
        split.bits_per_sample,
    );
    cfg.wav_header = Some(split.header.to_vec());
    cfg.terminating_data = split.terminating.to_vec();
    let mut enc = ApeEncoder::new(cfg)?;
    enc.push_interleaved_bytes(split.pcm)?;
    enc.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::ApeDecoder;

    fn noise(seed: u64, len: usize, bound: i32) -> Vec<i32> {
        let mut s = seed | 1;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 33) as i32) % (2 * bound) - bound
            })
            .collect()
    }

    fn round_trip(cfg: EncoderConfig, pcm: &[Vec<i32>], label: &str) -> Vec<u8> {
        let bits = cfg.bits_per_sample;
        let file = encode_pcm(cfg, pcm).unwrap_or_else(|e| panic!("{label}: encode — {e}"));
        let dec = ApeDecoder::new(&file).unwrap_or_else(|e| panic!("{label}: parse — {e}"));
        let got = dec
            .decode_all_bytes()
            .unwrap_or_else(|e| panic!("{label}: decode — {e}"));
        assert_eq!(
            got,
            interleave_pcm_bytes(pcm, bits).unwrap(),
            "{label}: PCM"
        );
        file
    }

    #[test]
    fn flags_follow_the_empirical_bit_assignment() {
        let z = vec![0; 4];
        let a = vec![1, 2, 3, 4];
        assert_eq!(
            frame_flags(std::slice::from_ref(&z)),
            FrameFlags(FrameFlags::CH0_SILENT)
        );
        assert_eq!(frame_flags(std::slice::from_ref(&a)), FrameFlags(0));
        assert_eq!(
            frame_flags(&[a.clone(), a.clone()]),
            FrameFlags(FrameFlags::PSEUDO_STEREO)
        );
        assert_eq!(
            frame_flags(&[z.clone(), a.clone()]),
            FrameFlags(FrameFlags::CH1_SILENT)
        );
        assert_eq!(
            frame_flags(&[a.clone(), z.clone()]),
            FrameFlags(FrameFlags::CH0_SILENT)
        );
        assert_eq!(frame_flags(&[z.clone(), z.clone()]), FrameFlags(7));
        assert_eq!(frame_flags(&[a.clone(), vec![1, 2, 3, 5]]), FrameFlags(0));
    }

    #[test]
    fn silent_frame_is_prologue_pad_and_empty_flush() {
        // §4.3 shape: CRC word with the flags marker, flags word, one
        // pad byte, a four-byte flushed empty coder.
        let f = encode_frame(&[vec![0; 100], vec![0; 100]], CompressionLevel::Fast, 16).unwrap();
        assert_eq!(f.flags, FrameFlags(7));
        assert_eq!(f.bytes.len(), 8 + 1 + 4);
        assert_eq!(f.bytes[0] & 0x80, 0x80);
        assert_eq!(&f.bytes[4..8], &[0, 0, 0, 7]);
        assert_eq!(f.crc31, crc32(&[0u8; 400]) >> 1);
        // No flags: a four-byte prologue.
        let f = encode_frame(&[vec![5; 3]], CompressionLevel::Fast, 16).unwrap();
        assert_eq!(f.flags, FrameFlags(0));
        assert_eq!(f.bytes[0] & 0x80, 0);
    }

    #[test]
    fn encode_frame_rejects_bad_input() {
        assert!(matches!(
            encode_frame(&[], CompressionLevel::Fast, 16),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            encode_frame(&[vec![0; 2], vec![0; 3]], CompressionLevel::Fast, 16),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            encode_frame(&[vec![40000]], CompressionLevel::Fast, 16),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            encode_frame(&[vec![-129]], CompressionLevel::Fast, 8),
            Err(Error::InvalidInput(_))
        ));
        assert!(encode_frame(&[vec![-128, 127]], CompressionLevel::Fast, 8).is_ok());
    }

    #[test]
    fn config_validation() {
        assert!(EncoderConfig::new(CompressionLevel::Fast, 3, 44100, 16)
            .validate()
            .is_err());
        assert!(EncoderConfig::new(CompressionLevel::Fast, 1, 44100, 12)
            .validate()
            .is_err());
        assert!(EncoderConfig::new(CompressionLevel::Fast, 1, 0, 16)
            .validate()
            .is_err());
        let mut c = EncoderConfig::new(CompressionLevel::Fast, 1, 44100, 16);
        c.blocks_per_frame = 0;
        assert!(c.validate().is_err());
        assert_eq!(
            EncoderConfig::new(CompressionLevel::Fast, 2, 44100, 24).block_align(),
            6
        );
        assert_eq!(
            EncoderConfig::new(CompressionLevel::Fast, 2, 44100, 8).sample_range(),
            (-128, 127)
        );
    }

    #[test]
    fn every_level_round_trips_stereo_and_mono() {
        for level in CompressionLevel::ALL {
            let ch0 = noise(u64::from(u16::from(level)), 3000, 20000);
            let ch1: Vec<i32> = ch0
                .iter()
                .zip(noise(99, 3000, 5000))
                .map(|(&a, b)| (a + b).clamp(-32768, 32767))
                .collect();
            round_trip(
                EncoderConfig::new(level, 2, 44100, 16),
                &[ch0.clone(), ch1],
                &format!("stereo {level:?}"),
            );
            round_trip(
                EncoderConfig::new(level, 1, 22050, 16),
                &[ch0],
                &format!("mono {level:?}"),
            );
        }
    }

    #[test]
    fn multi_frame_streaming_matches_one_shot_and_chunking_is_irrelevant() {
        let mut cfg = EncoderConfig::new(CompressionLevel::Normal, 2, 48000, 16);
        cfg.blocks_per_frame = 1000;
        let ch0 = noise(5, 3500, 12000);
        let ch1 = noise(6, 3500, 12000);
        let one_shot = encode_pcm(cfg.clone(), &[ch0.clone(), ch1.clone()]).unwrap();
        let dec = ApeDecoder::new(&one_shot).unwrap();
        assert_eq!(dec.info().total_frames, 4);
        assert_eq!(dec.info().final_frame_blocks, 500);
        assert_eq!(dec.info().blocks_per_frame, 1000);
        assert_eq!(dec.info().seek_table.len(), 4);
        assert_eq!(
            dec.decode_all_bytes().unwrap(),
            interleave_pcm_bytes(&[ch0.clone(), ch1.clone()], 16).unwrap()
        );
        // Streamed in ragged chunks: byte-identical file.
        let mut enc = ApeEncoder::new(cfg).unwrap();
        let mut pos = 0;
        for chunk in [1, 999, 1, 1499, 1000] {
            enc.push_samples(&[&ch0[pos..pos + chunk], &ch1[pos..pos + chunk]])
                .unwrap();
            pos += chunk;
        }
        assert_eq!(pos, 3500);
        assert_eq!(enc.frames_encoded(), 3);
        assert_eq!(enc.total_blocks(), 3500);
        assert_eq!(enc.finish().unwrap(), one_shot);
    }

    #[test]
    fn exact_frame_multiple_has_no_short_final_frame() {
        let mut cfg = EncoderConfig::new(CompressionLevel::Fast, 1, 8000, 16);
        cfg.blocks_per_frame = 500;
        let pcm = noise(7, 1000, 100);
        let file = round_trip(cfg, &[pcm], "exact multiple");
        let dec = ApeDecoder::new(&file).unwrap();
        assert_eq!(dec.info().total_frames, 2);
        assert_eq!(dec.info().final_frame_blocks, 500);
    }

    #[test]
    fn empty_input_writes_one_zero_block_frame() {
        let cfg = EncoderConfig::new(CompressionLevel::High, 2, 44100, 16);
        let file = encode_pcm(cfg, &[Vec::new(), Vec::new()]).unwrap();
        let dec = ApeDecoder::new(&file).unwrap();
        assert_eq!(dec.info().total_frames, 1);
        assert_eq!(dec.info().final_frame_blocks, 0);
        assert_eq!(dec.decode_all_bytes().unwrap(), Vec::<u8>::new());
    }

    #[test]
    fn eight_and_twenty_four_bit_round_trip() {
        let pcm8 = vec![noise(8, 2000, 128), noise(9, 2000, 128)];
        round_trip(
            EncoderConfig::new(CompressionLevel::ExtraHigh, 2, 11025, 8),
            &pcm8,
            "8-bit",
        );
        let pcm24 = vec![noise(24, 2000, 8_000_000), noise(25, 2000, 8_000_000)];
        let file = round_trip(
            EncoderConfig::new(CompressionLevel::Insane, 2, 96000, 24),
            &pcm24,
            "24-bit",
        );
        assert_eq!(ApeDecoder::new(&file).unwrap().info().bits_per_sample, 24);
    }

    #[test]
    fn interleaved_byte_push_equals_sample_push() {
        let ch0 = noise(1, 700, 30000);
        let ch1 = noise(2, 700, 30000);
        let bytes = interleave_pcm_bytes(&[ch0.clone(), ch1.clone()], 16).unwrap();
        let cfg = EncoderConfig::new(CompressionLevel::Normal, 2, 44100, 16);
        let mut a = ApeEncoder::new(cfg.clone()).unwrap();
        a.push_interleaved_bytes(&bytes).unwrap();
        let mut b = ApeEncoder::new(cfg).unwrap();
        b.push_samples(&[&ch0, &ch1]).unwrap();
        assert_eq!(a.finish().unwrap(), b.finish().unwrap());
        // A ragged byte buffer is rejected.
        let mut c =
            ApeEncoder::new(EncoderConfig::new(CompressionLevel::Fast, 2, 44100, 16)).unwrap();
        assert!(matches!(
            c.push_interleaved_bytes(&bytes[..7]),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn push_rejects_shape_and_range_violations() {
        let mut enc =
            ApeEncoder::new(EncoderConfig::new(CompressionLevel::Fast, 2, 44100, 16)).unwrap();
        assert!(matches!(
            enc.push_samples(&[&[1, 2][..]]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            enc.push_samples(&[&[1, 2][..], &[1][..]]),
            Err(Error::InvalidInput(_))
        ));
        assert!(matches!(
            enc.push_samples(&[&[32768][..], &[0][..]]),
            Err(Error::InvalidInput(_))
        ));
        assert!(enc.push_samples(&[&[32767][..], &[-32768][..]]).is_ok());
    }

    #[test]
    fn blobs_pass_through_and_the_md5_covers_them() {
        let mut cfg = EncoderConfig::new(CompressionLevel::Fast, 1, 8000, 16);
        cfg.wav_header = Some(b"RIFFfakeheader".to_vec());
        cfg.terminating_data = b"LIST-trailer".to_vec();
        cfg.trailing_tag = b"APETAGEX-not-parsed".to_vec();
        let pcm = vec![noise(3, 300, 1000)];
        let file = encode_pcm(cfg, &pcm).unwrap();
        assert!(file.ends_with(b"APETAGEX-not-parsed"));
        let dec = ApeDecoder::new(&file).unwrap();
        let info = dec.info();
        assert_eq!(info.wav_header, b"RIFFfakeheader");
        assert_eq!(info.terminating_data_bytes, 12);
        let end = info.audio_data_end() as usize;
        assert_eq!(&file[end..end + 12], b"LIST-trailer");
        assert_eq!(
            dec.decode_all_bytes().unwrap(),
            interleave_pcm_bytes(&pcm, 16).unwrap()
        );
        // A different terminating blob changes the digest.
        let mut cfg2 = EncoderConfig::new(CompressionLevel::Fast, 1, 8000, 16);
        cfg2.wav_header = Some(b"RIFFfakeheader".to_vec());
        cfg2.terminating_data = b"LIST-trailex".to_vec();
        let file2 = encode_pcm(cfg2, &pcm).unwrap();
        assert_ne!(
            ApeDecoder::new(&file2).unwrap().info().file_md5,
            info.file_md5
        );
    }

    #[test]
    fn wav_split_and_encode_wav_round_trip() {
        let ch0 = noise(11, 1234, 20000);
        let ch1 = noise(12, 1234, 20000);
        let pcm = interleave_pcm_bytes(&[ch0.clone(), ch1.clone()], 16).unwrap();
        let mut wav = canonical_wav_header(2, 44100, 16, pcm.len() as u32);
        wav.extend_from_slice(&pcm);
        wav.extend_from_slice(b"LIST\x04\x00\x00\x00abcd");
        let split = WavSplit::parse(&wav).unwrap();
        assert_eq!(split.channels, 2);
        assert_eq!(split.sample_rate, 44100);
        assert_eq!(split.bits_per_sample, 16);
        assert_eq!(split.header.len(), 44);
        assert_eq!(split.pcm, &pcm[..]);
        assert_eq!(split.terminating, b"LIST\x04\x00\x00\x00abcd");
        let file = encode_wav(&wav, CompressionLevel::High).unwrap();
        let dec = ApeDecoder::new(&file).unwrap();
        assert_eq!(dec.info().wav_header, &wav[..44]);
        assert_eq!(dec.info().terminating_data_bytes, 12);
        assert_eq!(dec.decode_all_bytes().unwrap(), pcm);
        // A WAV with a chunk before `fmt `/`data` and an odd-length
        // chunk pad.
        let mut wav2 = b"RIFF\0\0\0\0WAVEjunk\x03\x00\x00\x00abc\0".to_vec();
        wav2.extend_from_slice(&canonical_wav_header(1, 8000, 8, 3)[12..]);
        wav2.extend_from_slice(&[128, 129, 127]);
        let split = WavSplit::parse(&wav2).unwrap();
        assert_eq!(split.pcm, &[128, 129, 127]);
        assert!(split.terminating.is_empty());
        // Errors.
        assert!(WavSplit::parse(b"RIFX").is_err());
        assert!(WavSplit::parse(b"RIFF\0\0\0\0WAVEdata\x04\x00\x00\x00abcd").is_err());
    }
}
