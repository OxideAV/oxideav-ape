//! Whole-file synthetic round-trips over the branches no vendor
//! fixture can reach.
//!
//! The staged format reference (§6.13) lists the `< 3990` entropy
//! path, the `< 3980` era-B `delta[]` rule, the 3930–3949 predictor
//! form, the multi-stage cascade orderings of levels 3000/4000/5000,
//! and the 8-/24-bit reassembly as **source-only**: the vendor
//! encoder emits 3990-era 16-bit streams exclusively, so no black-box
//! fixture exercises them. This suite locks those branches with full
//! self-consistency loops — engineered PCM → the crate's own encode
//! mirrors (predictor chain inverse + residual entropy encoder +
//! header/tail writers) → a complete synthetic `.ape` byte stream →
//! [`ApeDecoder::decode_all_bytes`] — asserting byte-exact PCM
//! recovery with every stored frame CRC agreeing.
//!
//! A mirrored round-trip cannot, by itself, prove conformance with
//! real pre-3990 archives (a symmetric misreading would cancel out —
//! that closure still needs a genuine archived stream, per the staged
//! GAP list). What it does pin: the decode side stays internally
//! consistent across every version/level/depth combination, the
//! entropy layer and predictor interact correctly on
//! predictor-shaped residual streams, and any future regression in a
//! source-only branch trips a test instead of hiding behind the
//! fixture corpus's 3990-era horizon.

use oxideav_ape::decoder::ApeDecoder;
use oxideav_ape::entropy::ResidualEncoder;
use oxideav_ape::frame::{crc32, FRAME_ENTROPY_INIT, FRAME_PRIME_PAD_BYTES};
use oxideav_ape::header::CompressionLevel;
use oxideav_ape::pcm::{interleave_pcm_bytes, pcm_to_coded_arrays};

/// Deterministic xorshift noise in `[-bound, bound)`.
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

/// Reshape a logical byte stream into the on-disk §4.1 layout: the
/// audio region is addressed as little-endian 32-bit words consumed
/// MSB-first, so logical byte `p` lives at physical `(p & !3) + (3 -
/// (p & 3))` — a per-word byte reversal over the whole region.
fn to_le_word_layout(mut logical: Vec<u8>) -> Vec<u8> {
    while logical.len() % 4 != 0 {
        logical.push(0);
    }
    for chunk in logical.chunks_mut(4) {
        chunk.reverse();
    }
    logical
}

/// Entropy-code one frame's coded arrays exactly as the frame layer
/// decodes them: one shared coder, per-sample channel interleave with
/// independent per-channel running states, per-frame init.
fn entropy_code_frame(version: u16, arrays: &[Vec<i32>]) -> Vec<u8> {
    let mut enc = ResidualEncoder::new(version, FRAME_ENTROPY_INIT);
    if arrays.len() == 2 {
        let mut states = [FRAME_ENTROPY_INIT; 2];
        for i in 0..arrays[0].len() {
            for (ch, arr) in arrays.iter().enumerate() {
                enc.reset_state(states[ch]);
                enc.encode_residual(arr[i]).unwrap();
                states[ch] = enc.running_state();
            }
        }
    } else {
        for &r in &arrays[0] {
            enc.encode_residual(r).unwrap();
        }
    }
    enc.finish()
}

/// Build one frame's **logical** byte stream: the 31-bit stored CRC
/// word (flags marker clear), the structural pad byte, then the
/// range-coded payload.
fn build_frame_logical(
    version: u16,
    level: CompressionLevel,
    pcm: &[Vec<i32>],
    bits: u16,
) -> Vec<u8> {
    let pcm_bytes = interleave_pcm_bytes(pcm, bits).unwrap();
    let coded = pcm_to_coded_arrays(pcm, version, level).unwrap();
    let mut logical = Vec::new();
    logical.extend_from_slice(&(crc32(&pcm_bytes) >> 1).to_be_bytes());
    logical.extend_from_slice(&[0u8; FRAME_PRIME_PAD_BYTES]);
    logical.extend_from_slice(&entropy_code_frame(version, &coded));
    logical
}

/// Assemble a complete old-era (`version < 3980`) file: the §1.3 flat
/// header with the `CREATE_WAV_HEADER` flag (no stored WAV blob, no
/// peak level, seek count = total frames), the seek byte table, then
/// the frame payload in §4.1 word layout. 16-bit only (the old-era
/// depth is flag-derived and no depth flag is set).
fn build_old_era_file(
    version: u16,
    level: CompressionLevel,
    sample_rate: u32,
    frames: &[&[Vec<i32>]],
) -> Vec<u8> {
    let channels = frames[0].len() as u16;
    let mut logical = Vec::new();
    let mut offsets = Vec::new();
    let audio_start = 32 + 4 * frames.len() as u32;
    for pcm in frames {
        offsets.push(audio_start + logical.len() as u32);
        logical.extend_from_slice(&build_frame_logical(version, level, pcm, 16));
    }
    let mut file = Vec::new();
    file.extend_from_slice(b"MAC ");
    file.extend_from_slice(&version.to_le_bytes());
    file.extend_from_slice(&u16::from(level).to_le_bytes());
    file.extend_from_slice(&32u16.to_le_bytes()); // flags: CREATE_WAV_HEADER
    file.extend_from_slice(&channels.to_le_bytes());
    file.extend_from_slice(&sample_rate.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes()); // stored WAV header bytes
    file.extend_from_slice(&0u32.to_le_bytes()); // terminating bytes
    file.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    file.extend_from_slice(&(frames.last().unwrap()[0].len() as u32).to_le_bytes());
    for off in offsets {
        file.extend_from_slice(&off.to_le_bytes());
    }
    assert_eq!(file.len() as u32, audio_start);
    file.extend_from_slice(&to_le_word_layout(logical));
    file
}

/// Assemble a complete new-era (`version >= 3980`) file: §1.1
/// descriptor (zeroed MD5 — the parser stores it without verifying),
/// §1.2 header with explicit blocks-per-frame and bit depth, seek
/// table, no WAV blob, then the frame payload in §4.1 word layout.
fn build_new_era_file(
    version: u16,
    level: CompressionLevel,
    sample_rate: u32,
    bits: u16,
    blocks_per_frame: u32,
    frames: &[&[Vec<i32>]],
) -> Vec<u8> {
    let channels = frames[0].len() as u16;
    let seek_bytes = 4 * frames.len() as u32;
    let audio_start = 52 + 24 + seek_bytes;
    let mut logical = Vec::new();
    let mut offsets = Vec::new();
    for pcm in frames {
        offsets.push(audio_start + logical.len() as u32);
        logical.extend_from_slice(&build_frame_logical(version, level, pcm, bits));
    }
    let payload = to_le_word_layout(logical);
    let mut file = Vec::new();
    file.extend_from_slice(b"MAC ");
    file.extend_from_slice(&version.to_le_bytes());
    file.extend_from_slice(&[0u8; 2]); // §1.1 alignment gap
    file.extend_from_slice(&52u32.to_le_bytes());
    file.extend_from_slice(&24u32.to_le_bytes());
    file.extend_from_slice(&seek_bytes.to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes()); // WAV header blob
    file.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    file.extend_from_slice(&0u32.to_le_bytes()); // high 32 bits
    file.extend_from_slice(&0u32.to_le_bytes()); // terminating blob
    file.extend_from_slice(&[0u8; 16]); // MD5 (unverified on parse)
    file.extend_from_slice(&u16::from(level).to_le_bytes());
    file.extend_from_slice(&0u16.to_le_bytes()); // format flags
    file.extend_from_slice(&blocks_per_frame.to_le_bytes());
    file.extend_from_slice(&(frames.last().unwrap()[0].len() as u32).to_le_bytes());
    file.extend_from_slice(&(frames.len() as u32).to_le_bytes());
    file.extend_from_slice(&bits.to_le_bytes());
    file.extend_from_slice(&channels.to_le_bytes());
    file.extend_from_slice(&sample_rate.to_le_bytes());
    for off in offsets {
        file.extend_from_slice(&off.to_le_bytes());
    }
    assert_eq!(file.len() as u32, audio_start);
    file.extend_from_slice(&payload);
    file
}

/// Decode a synthetic file and assert byte-exact PCM recovery (every
/// frame CRC-verified inside `decode_all_bytes`).
fn assert_round_trip(file: &[u8], frames: &[&[Vec<i32>]], bits: u16, label: &str) {
    let dec = ApeDecoder::new(file).unwrap_or_else(|e| panic!("{label}: parse — {e}"));
    let got = dec
        .decode_all_bytes()
        .unwrap_or_else(|e| panic!("{label}: decode — {e}"));
    let mut expected = Vec::new();
    for pcm in frames {
        expected.extend_from_slice(&interleave_pcm_bytes(pcm, bits).unwrap());
    }
    assert_eq!(got, expected, "{label}: PCM bytes");
}

#[test]
fn old_era_files_round_trip_every_predictor_form_and_level() {
    // 3930/3949: the single-arm §6.7.2 form (levels 1000-4000 only);
    // 3950/3970: the two-arm form with era-B delta[] filters. All
    // stereo with correlated channels so both X and Y stay active.
    let cases: &[(u16, &[CompressionLevel])] = &[
        (
            3930,
            &[
                CompressionLevel::Fast,
                CompressionLevel::Normal,
                CompressionLevel::High,
                CompressionLevel::ExtraHigh,
            ],
        ),
        (
            3949,
            &[CompressionLevel::Normal, CompressionLevel::ExtraHigh],
        ),
        (3950, &CompressionLevel::ALL[..]),
        (3970, &CompressionLevel::ALL[..]),
    ];
    for &(version, levels) in cases {
        for &level in levels {
            let ch0 = noise(u64::from(version) ^ 0xA5, 1500, 12000);
            let ch1: Vec<i32> = ch0
                .iter()
                .zip(noise(u64::from(version) ^ 0x5A, 1500, 2500))
                .map(|(&a, b)| (a + b).clamp(-32768, 32767))
                .collect();
            let pcm = [ch0, ch1];
            let frames: &[&[Vec<i32>]] = &[&pcm];
            let file = build_old_era_file(version, level, 44100, frames);
            let label = format!("old-era v{version} level {level:?}");
            let dec = ApeDecoder::new(&file).unwrap();
            assert_eq!(dec.info().version, version);
            assert_eq!(dec.info().bits_per_sample, 16, "{label}: flag-derived");
            assert_round_trip(&file, frames, 16, &label);
        }
    }
}

#[test]
fn old_era_mono_and_multi_frame_share_the_word_grid() {
    // Two frames at version 3930 (73728 blocks per non-final frame,
    // §1.4): the second frame starts wherever the first's payload
    // ends, exercising per-frame state reset and the shared LE-word
    // bit-array grid on the old-era header path.
    let f0 = [noise(11, 73728, 900)];
    let f1 = [noise(13, 2200, 900)];
    let frames: &[&[Vec<i32>]] = &[&f0, &f1];
    let file = build_old_era_file(3930, CompressionLevel::Normal, 8000, frames);
    let dec = ApeDecoder::new(&file).unwrap();
    assert_eq!(dec.info().total_frames, 2);
    assert_eq!(dec.info().blocks_per_frame, 73728);
    assert_round_trip(&file, frames, 16, "old-era two-frame mono");
}

#[test]
fn new_era_3980_pairs_the_old_entropy_path_with_era_a_filters() {
    // Version 3980 sits between the two boundaries: new-era header,
    // era-A delta[] rule, but still the < 3990 (Model 1, adaptive-k)
    // entropy path. No vendor fixture reaches this combination.
    for level in CompressionLevel::ALL {
        let ch0 = noise(0x3980 ^ u64::from(u16::from(level)), 1500, 12000);
        let ch1: Vec<i32> = ch0.iter().map(|&a| a / 2 + 7).collect();
        let pcm = [ch0, ch1];
        let frames: &[&[Vec<i32>]] = &[&pcm];
        let file = build_new_era_file(3980, level, 44100, 16, 73728, frames);
        assert_round_trip(&file, frames, 16, &format!("new-era v3980 {level:?}"));
    }
}

#[test]
fn new_era_3990_deep_cascades_round_trip() {
    // Levels 3000/4000/5000 at 3990: the multi-stage cascade decode
    // ordering (§6.3) that the vendor corpus (levels 1000/2000 only)
    // never exercises.
    for level in [
        CompressionLevel::High,
        CompressionLevel::ExtraHigh,
        CompressionLevel::Insane,
    ] {
        let ch0 = noise(0x3990 ^ u64::from(u16::from(level)), 2000, 15000);
        let ch1: Vec<i32> = ch0
            .iter()
            .zip(noise(0x1234, 2000, 4000))
            .map(|(&a, b)| (a + b).clamp(-32768, 32767))
            .collect();
        let pcm = [ch0, ch1];
        let frames: &[&[Vec<i32>]] = &[&pcm];
        let file = build_new_era_file(3990, level, 48000, 16, 73728, frames);
        assert_round_trip(&file, frames, 16, &format!("new-era v3990 {level:?}"));
    }
}

#[test]
fn high_magnitude_24_bit_stream_exercises_the_wide_coder_branches() {
    // 24-bit PCM at ±4M drives KSum past 2^21, so the >= 3990 base
    // decode takes the §2.6 radix-split (pivot >= 65536) branch, and
    // the first large residuals against the fresh KSum = 16384 state
    // take the 32-bit overflow escape (nOverflow == 63) — both marked
    // corpus-unreachable in §6.13. Also the only 24-bit reassembly
    // coverage (§6.9 table).
    let ch0 = noise(0x24B17, 2500, 4_000_000);
    let ch1: Vec<i32> = ch0
        .iter()
        .zip(noise(77, 2500, 1_000_000))
        .map(|(&a, b)| (a + b).clamp(-8_388_608, 8_388_607))
        .collect();
    let pcm = [ch0, ch1];
    let frames: &[&[Vec<i32>]] = &[&pcm];
    let file = build_new_era_file(3990, CompressionLevel::Normal, 96000, 24, 73728, frames);
    let dec = ApeDecoder::new(&file).unwrap();
    assert_eq!(dec.info().bits_per_sample, 24);
    assert_round_trip(&file, frames, 24, "new-era 24-bit high-magnitude");
}

#[test]
fn old_era_wide_k_split_boundary_3910() {
    // §2.7: for versions >= 3910 a wide nTempK decodes as a 16-bit +
    // remainder split; below 3910 it reads in one shot. Drive both
    // sides of the boundary with the same high-energy stream (16-bit
    // range keeps the old-era flag-derived depth valid).
    for version in [3930u16, 3949] {
        let ch0 = noise(u64::from(version), 1200, 30000);
        let pcm = [ch0];
        let frames: &[&[Vec<i32>]] = &[&pcm];
        let file = build_old_era_file(version, CompressionLevel::Fast, 22050, frames);
        assert_round_trip(&file, frames, 16, &format!("wide-k v{version}"));
    }
}

#[test]
fn eight_bit_new_era_round_trips_with_biased_bytes() {
    // 8-bit reassembly: stored bytes are value + 128 (§6.9). The
    // predictor runs in the signed domain; the byte layer biases.
    let ch0: Vec<i32> = noise(88, 900, 128);
    let pcm = [ch0];
    let frames: &[&[Vec<i32>]] = &[&pcm];
    let file = build_new_era_file(3990, CompressionLevel::Fast, 11025, 8, 73728, frames);
    let dec = ApeDecoder::new(&file).unwrap();
    assert_eq!(dec.info().bits_per_sample, 8);
    assert_round_trip(&file, frames, 8, "new-era 8-bit");
}
