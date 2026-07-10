//! Vendor frame layout — the per-frame prologue, entropy-region
//! geometry, and channel interleaving that turn a seek-table frame
//! position into residual arrays.
//!
//! The staged `docs/audio/ape/format-reference.md` §4 marks the frame
//! boundary / bit-array priming and the per-frame running-state reset
//! as GAPs; every rule in this module was therefore established
//! **black-box**, by comparing this crate's §2-pinned coder against
//! frames produced by the vendor reference binary (v13.18 console
//! encoder, file version 3990) over engineered PCM inputs — silence
//! runs, zero-prefixed noise, single-sample spikes, equal-channel and
//! one-channel-silent stereo, and a two-frame file whose second frame
//! starts word-unaligned. No implementation source of any kind was
//! consulted. The empirical findings, each validated to bit-exact /
//! full-payload-consumption fitness:
//!
//! 1. **One bit array per file**: the whole audio-data region is a
//!    single bit array of 32-bit words **loaded little-endian** and
//!    consumed MSB-first (the §2.2 word indirection, with the word
//!    grid anchored at the audio-data start). Frame `i` begins at bit
//!    `(seek[i] - audio_start) * 8`; frame starts are byte-aligned but
//!    **not** word-aligned, so every multi-byte read goes through the
//!    bit array.
//! 2. **Prologue** (read as MSB-first 32-bit values *through the bit
//!    array*): one word whose bits 30..0 are `crc32(decoded frame PCM
//!    bytes) >> 1` (standard reflected CRC-32) and whose bit 31 marks
//!    that a second word of [`FrameFlags`] follows.
//! 3. **Frame flags**: bit 0 / bit 1 — a PCM channel is silent for
//!    the whole frame; bit 2 — the two stereo channels are identical
//!    ("pseudo-stereo"). A fully-silent frame carries no entropy
//!    payload at all; a pseudo-stereo frame carries **one** coded
//!    array; partial silence changes nothing about the layout.
//! 4. **Coder head**: one byte past the prologue is structural pad the
//!    coder never treats as payload ([`FRAME_PRIME_PAD_BYTES`]); the
//!    range decoder then primes per the staged-constants inference
//!    (first byte's top `EXTRA_BITS` bits).
//! 5. **Running-state init**: `KSum = 16384` per channel per frame
//!    ([`FRAME_KSUM_INIT`], pinned by matching the coded size of a
//!    4000-zero run to the byte). `k = 10` is the ladder position that
//!    `K_SUM_MIN_BOUNDARY` assigns to that `KSum` ([`FRAME_K_INIT`];
//!    the old-path `k` cannot be exercised against the current vendor
//!    encoder, which emits 3990-era streams only).
//! 6. **Stereo interleaving**: the two arrays are coded **per-sample
//!    interleaved** over one shared coder, with *independent*
//!    per-channel running states. (The wiki's "unpack array … then the
//!    second array" listing describes the logical result, not the
//!    physical symbol order.)
//!
//! What the arrays *are* (the decorrelated X/Y pair) and how they
//! become PCM (predictor cascade + channel correlation) stays with the
//! staged predictor docs; this module stops at residual arrays, plus
//! exact PCM for the silence cases the flags fully determine.

use crate::entropy::{EntropyInit, ResidualDecoder};
use crate::error::{Error, Result};
use crate::range_coder::{BitInput, RangeDecoder};

/// Per-frame, per-channel `KSum` init (empirical; see module docs).
pub const FRAME_KSUM_INIT: u32 = 16384;

/// Per-frame `k` init — the `K_SUM_MIN_BOUNDARY` ladder position of
/// [`FRAME_KSUM_INIT`] (`boundary[10] == 16384`). Consistent with the
/// ladder but not independently verifiable against the current vendor
/// encoder (new-path streams never read `k`).
pub const FRAME_K_INIT: u32 = 10;

/// Structural pad bytes between the prologue and the first coder
/// payload byte (empirical; see module docs).
pub const FRAME_PRIME_PAD_BYTES: usize = 1;

/// The caller-facing per-frame entropy init.
pub const FRAME_ENTROPY_INIT: EntropyInit = EntropyInit {
    k: FRAME_K_INIT,
    ksum: FRAME_KSUM_INIT,
};

/// Upper bound on a single frame's block count accepted by the frame
/// decoder. Every documented blocks-per-frame value is at most 294912
/// (`73728 * 4`); the guard leaves generous headroom while keeping a
/// corrupted header from driving multi-gigabyte allocations.
pub const MAX_FRAME_BLOCKS: u32 = 1 << 20;

/// Standard reflected CRC-32 (polynomial `0xEDB88320`, init/xorout
/// `0xFFFF_FFFF`) — the checksum family the frame prologue stores over
/// the frame's decoded PCM bytes.
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// Per-frame flags word (empirical; see module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FrameFlags(pub u32);

impl FrameFlags {
    /// Bit 0 — a PCM channel is silent for the whole frame: the mono
    /// channel, or (empirically) the *second* stored stereo channel
    /// (the right channel of a WAV pair).
    pub const CH0_SILENT: u32 = 1;
    /// Bit 1 — the *first* stored stereo channel (the left channel of
    /// a WAV pair) is silent for the whole frame.
    pub const CH1_SILENT: u32 = 2;
    /// Bit 2 — the two stereo channels are identical; one array codes
    /// them both.
    pub const PSEUDO_STEREO: u32 = 4;

    /// Whether `bit` (one of the associated constants) is set.
    pub const fn has(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    /// Whether every PCM channel of a `channels`-channel frame is
    /// flagged silent (such frames carry no entropy payload).
    pub const fn all_silent(self, channels: u16) -> bool {
        match channels {
            1 => self.has(Self::CH0_SILENT),
            2 => self.has(Self::CH0_SILENT) && self.has(Self::CH1_SILENT),
            _ => false,
        }
    }
}

/// Parsed frame prologue: the stored CRC field and optional flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePrologue {
    /// Bits 30..0 of the stored first word: `crc32(frame PCM) >> 1`.
    pub crc31: u32,
    /// The flags word, present when bit 31 of the first word is set.
    pub flags: Option<FrameFlags>,
    /// Prologue size in bytes (4 or 8).
    pub len: usize,
}

impl FramePrologue {
    /// Read the prologue through the frame bit array (one or two
    /// MSB-first 32-bit reads).
    pub fn read(input: &mut BitInput<'_>) -> Self {
        let word0 = input.read_u32();
        if word0 & 0x8000_0000 != 0 {
            FramePrologue {
                crc31: word0 & 0x7FFF_FFFF,
                flags: Some(FrameFlags(input.read_u32())),
                len: 8,
            }
        } else {
            FramePrologue {
                crc31: word0,
                flags: None,
                len: 4,
            }
        }
    }

    /// Whether `pcm_bytes` (the frame's decoded PCM, in stored byte
    /// order) matches the stored checksum (`crc32(pcm) >> 1`).
    pub fn matches_pcm_crc(&self, pcm_bytes: &[u8]) -> bool {
        crc32(pcm_bytes) >> 1 == self.crc31
    }
}

/// One frame's entropy-layer decode result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrameResiduals {
    /// The parsed prologue.
    pub prologue: FramePrologue,
    /// One residual array per **coded** array, in stream interleave
    /// order: `[mono]`, two arrays for plain stereo (empirically the
    /// difference-type array first, the mid-type second — the exact
    /// X/Y naming awaits the staged decorrelation orientation), or one
    /// shared array for pseudo-stereo.
    pub arrays: Vec<Vec<i32>>,
    /// Coder bit position after the last symbol, relative to the
    /// audio-data region (frame-payload accounting).
    pub end_bit_pos: u64,
    /// Whether the flags fully determine the frame's PCM (all-silent
    /// frames): every channel is zero for the whole frame.
    pub silent: bool,
}

/// Decode one frame into its residual arrays.
///
/// `audio` is the **whole audio-data region** (the file from
/// [`crate::file_header::FileInfo::audio_data_offset`] to
/// [`crate::file_header::FileInfo::audio_data_end`]) — the bit array's
/// word grid is anchored at its start. `frame_byte_offset` is the
/// frame's byte offset within that region (`seek[i] - audio_start`);
/// `blocks` is the frame's audio block count
/// ([`crate::file_header::FileInfo::frame_blocks`]); `channels` is 1
/// or 2 (multichannel ≥ 3 is outside the staged material).
pub fn decode_frame_residuals(
    audio: &[u8],
    frame_byte_offset: usize,
    file_version: u16,
    channels: u16,
    blocks: u32,
) -> Result<FrameResiduals> {
    if !(1..=2).contains(&channels) {
        return Err(Error::Malformed("channel count outside the staged 1..=2"));
    }
    if blocks > MAX_FRAME_BLOCKS {
        return Err(Error::Malformed("frame block count exceeds the sane bound"));
    }
    if frame_byte_offset + 4 > audio.len() {
        return Err(Error::Truncated);
    }
    let mut input = BitInput::new_le_words(audio, (frame_byte_offset as u64) * 8);
    let prologue = FramePrologue::read(&mut input);
    let flags = prologue.flags.unwrap_or_default();
    let n = blocks as usize;

    // Fully-silent frames carry no entropy payload.
    if flags.all_silent(channels) {
        return Ok(FrameResiduals {
            prologue,
            arrays: vec![vec![0i32; n]; usize::from(channels)],
            end_bit_pos: input.bit_pos(),
            silent: true,
        });
    }

    // One structural pad byte, then the coder primes off the array.
    for _ in 0..FRAME_PRIME_PAD_BYTES {
        input.read_byte();
    }
    let rc = RangeDecoder::with_input(input);
    let mut dec = ResidualDecoder::with_coder(rc, file_version, FRAME_ENTROPY_INIT);

    let coded_arrays = if channels == 2 && !flags.has(FrameFlags::PSEUDO_STEREO) {
        2usize
    } else {
        1usize
    };
    let mut arrays = vec![Vec::with_capacity(n); coded_arrays];
    if coded_arrays == 2 {
        // Per-sample interleave with independent running states.
        let mut states = [FRAME_ENTROPY_INIT; 2];
        for _ in 0..n {
            for (ch, arr) in arrays.iter_mut().enumerate() {
                dec.reset_state(states[ch]);
                arr.push(dec.decode_residual()?);
                states[ch] = dec.running_state();
            }
        }
    } else {
        for _ in 0..n {
            arrays[0].push(dec.decode_residual()?);
        }
    }
    Ok(FrameResiduals {
        prologue,
        arrays,
        end_bit_pos: dec.bit_pos(),
        silent: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc32_matches_the_standard_vectors() {
        // The canonical IEEE CRC-32 check value.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn prologue_without_flag_bit_is_four_bytes() {
        // Word-aligned start: the bit-array 32-bit read equals the
        // little-endian load of the four raw bytes.
        let raw = [0x4D, 0x2B, 0x04, 0x71, 0xAA, 0xBB, 0xCC, 0xDD];
        let mut input = BitInput::new_le_words(&raw, 0);
        let p = FramePrologue::read(&mut input);
        assert_eq!(p.crc31, 0x7104_2B4D);
        assert_eq!(p.flags, None);
        assert_eq!(p.len, 4);
        assert_eq!(input.bit_pos(), 32);
    }

    #[test]
    fn prologue_with_flag_bit_reads_the_flags_word() {
        // First word with bit 31 set, then flags = 7.
        let raw = [0xBB, 0x36, 0x37, 0xAC, 0x07, 0x00, 0x00, 0x00];
        let mut input = BitInput::new_le_words(&raw, 0);
        let p = FramePrologue::read(&mut input);
        assert_eq!(p.crc31, 0x2C37_36BB);
        assert_eq!(p.flags, Some(FrameFlags(7)));
        assert_eq!(p.len, 8);
        assert_eq!(input.bit_pos(), 64);
    }

    #[test]
    fn prologue_reads_through_the_word_grid_when_unaligned() {
        // A frame starting at byte 1 of the audio region: the 32-bit
        // read must assemble across the LE-word grid, not from raw
        // sequential bytes. Grid words: LE(b3 b2 b1 b0), consumed
        // MSB-first — so bits 8..40 are b2 b1 b0 b7.
        let raw = [0xB0, 0xB1, 0x12, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7];
        let mut input = BitInput::new_le_words(&raw, 8);
        let p = FramePrologue::read(&mut input);
        assert_eq!(p.crc31, 0x12B1_B0B7);
        assert_eq!(p.flags, None);
        assert_eq!(p.len, 4);
    }

    #[test]
    fn all_silent_shapes() {
        assert!(FrameFlags(1).all_silent(1));
        assert!(!FrameFlags(0).all_silent(1));
        assert!(FrameFlags(3).all_silent(2));
        assert!(FrameFlags(7).all_silent(2));
        assert!(!FrameFlags(1).all_silent(2));
        assert!(!FrameFlags(2).all_silent(2));
        assert!(!FrameFlags(4).all_silent(2));
    }

    #[test]
    fn silent_frame_decodes_to_zero_arrays_without_entropy() {
        // A fully-silent stereo frame: prologue only, no payload.
        let audio = [0xBB, 0x36, 0x37, 0xAC, 0x07, 0x00, 0x00, 0x00];
        let out = decode_frame_residuals(&audio, 0, 3990, 2, 64).unwrap();
        assert!(out.silent);
        assert_eq!(out.end_bit_pos, 64);
        assert_eq!(out.arrays.len(), 2);
        assert!(out
            .arrays
            .iter()
            .all(|a| a.len() == 64 && a.iter().all(|&v| v == 0)));
    }

    #[test]
    fn crc_verify_accepts_matching_pcm() {
        // Stored word = crc32(pcm) >> 1 with bit 31 as the flag marker.
        let pcm = vec![0u8; 256];
        let p = FramePrologue {
            crc31: crc32(&pcm) >> 1,
            flags: Some(FrameFlags(3)),
            len: 8,
        };
        assert!(p.matches_pcm_crc(&pcm));
        assert!(!p.matches_pcm_crc(&[1, 2, 3]));
    }

    #[test]
    fn rejects_unstaged_channel_counts_and_truncation() {
        let audio = [0u8; 16];
        assert!(matches!(
            decode_frame_residuals(&audio, 0, 3990, 0, 4),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            decode_frame_residuals(&audio, 0, 3990, 3, 4),
            Err(Error::Malformed(_))
        ));
        assert_eq!(
            decode_frame_residuals(&audio, 14, 3990, 1, 4).unwrap_err(),
            Error::Truncated
        );
    }

    #[test]
    fn oversized_block_counts_are_rejected_before_allocation() {
        // A corrupted header must not drive a multi-gigabyte residual
        // allocation: the guard fires before any buffer is sized.
        let audio = [0u8; 16];
        assert!(matches!(
            decode_frame_residuals(&audio, 0, 3990, 1, MAX_FRAME_BLOCKS + 1),
            Err(Error::Malformed(_))
        ));
        assert!(matches!(
            decode_frame_residuals(&audio, 0, 3990, 2, u32::MAX),
            Err(Error::Malformed(_))
        ));
        // Every documented blocks-per-frame value stays well inside.
        const { assert!(294912 < MAX_FRAME_BLOCKS) };
    }
}
