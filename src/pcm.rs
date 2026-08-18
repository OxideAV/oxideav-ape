//! Frame PCM reconstruction — the staged
//! `docs/audio/ape/format-reference.md` §6.1 per-block call order over
//! the [`crate::predict::ArrayPredictor`] chains, the §6.9 channel
//! decorrelation (orientation, sign, and truncating division), and the
//! §6.9 per-bit-depth sample reassembly.
//!
//! For a `>= 3950` stereo frame the coded arrays arrive Y first
//! (difference-type), X second (average-type), and the per-block order
//! is:
//!
//! ```text
//! Y = predictor_Y.decode(nY, lastX)   # cross term: previous block's X
//! X = predictor_X.decode(nX, Y)       # cross term: this block's Y
//! lastX = X
//! s0 = X - (Y / 2)                    # truncating division (§6.9)
//! s1 = s0 + Y
//! ```
//!
//! `s0` is the **first** stored sample of the interleaved pair (WAV
//! channel 0). For 3930–3949 files the coded order is X first, Y
//! second, and no cross term exists (§6.1/§6.2). Mono uses X alone; a
//! pseudo-stereo frame decodes the single shared array as X, treats
//! `Y = 0`, and emits it on both channels; a fully-silent frame is
//! handled upstream (it carries no entropy payload at all).

use crate::error::{Error, Result};
use crate::frame::{FrameFlags, FrameResiduals};
use crate::header::CompressionLevel;
use crate::predict::{ArrayPredictor, CROSS_TERM_VERSION};

/// §6.9 channel decorrelation, decode direction: recover the stored
/// sample pair `(s0, s1)` from the decorrelated `(X, Y)` pair. The
/// `/ 2` is a truncating integer division — substituting an arithmetic
/// shift is observably wrong on odd negative `Y` (§6.12).
#[inline]
pub const fn reconstruct_sample_pair(x: i32, y: i32) -> (i32, i32) {
    let s0 = x.wrapping_sub(y / 2);
    (s0, s0.wrapping_add(y))
}

/// Reconstruct one non-silent frame's per-channel PCM from its decoded
/// residual arrays. `version` / `level` / `channels` come from the
/// parsed file header. Returns one array per PCM channel.
///
/// Fully-silent frames never reach this function (their
/// [`FrameResiduals::silent`] arrays already are the PCM); files below
/// version 3930 surface [`Error::NotImplemented`] (§6.2 — outside the
/// staged predictor material).
pub fn frame_pcm(
    res: &FrameResiduals,
    version: u16,
    level: CompressionLevel,
    channels: u16,
) -> Result<Vec<Vec<i32>>> {
    if res.silent {
        return Ok(res.arrays.clone());
    }
    let flags = res.prologue.flags.unwrap_or_default();
    match (channels, res.arrays.len()) {
        (1, 1) => {
            // Mono: X alone, cross term 0 (§6.1).
            let mut x_pred = ArrayPredictor::new(version, level)?;
            Ok(vec![res.arrays[0]
                .iter()
                .map(|&r| x_pred.decode(r, 0))
                .collect()])
        }
        (2, 1) if flags.has(FrameFlags::PSEUDO_STEREO) => {
            // Pseudo-stereo: decode X only, Y = 0 collapses the §6.9
            // equations so both output channels equal X.
            let mut x_pred = ArrayPredictor::new(version, level)?;
            let x: Vec<i32> = res.arrays[0].iter().map(|&r| x_pred.decode(r, 0)).collect();
            Ok(vec![x.clone(), x])
        }
        (2, 2) => {
            let n = res.arrays[0].len();
            if res.arrays[1].len() != n {
                return Err(Error::Malformed("stereo coded arrays disagree on length"));
            }
            let mut ch0 = Vec::with_capacity(n);
            let mut ch1 = Vec::with_capacity(n);
            if version >= CROSS_TERM_VERSION {
                // §6.1: Y coded first, X second; Y's cross term is the
                // previous block's X (0 at frame start), X's is this
                // block's Y.
                let (arr_y, arr_x) = (&res.arrays[0], &res.arrays[1]);
                let mut y_pred = ArrayPredictor::new(version, level)?;
                let mut x_pred = ArrayPredictor::new(version, level)?;
                let mut last_x = 0i32;
                for i in 0..n {
                    let y = y_pred.decode(arr_y[i], last_x);
                    let x = x_pred.decode(arr_x[i], y);
                    last_x = x;
                    let (s0, s1) = reconstruct_sample_pair(x, y);
                    ch0.push(s0);
                    ch1.push(s1);
                }
            } else {
                // §6.1: below 3950 the order inverts — X first, Y
                // second — and there is no cross term at all.
                let (arr_x, arr_y) = (&res.arrays[0], &res.arrays[1]);
                let mut x_pred = ArrayPredictor::new(version, level)?;
                let mut y_pred = ArrayPredictor::new(version, level)?;
                for i in 0..n {
                    let x = x_pred.decode(arr_x[i], 0);
                    let y = y_pred.decode(arr_y[i], 0);
                    let (s0, s1) = reconstruct_sample_pair(x, y);
                    ch0.push(s0);
                    ch1.push(s1);
                }
            }
            Ok(vec![ch0, ch1])
        }
        _ => Err(Error::Malformed(
            "channel count disagrees with the coded array layout",
        )),
    }
}

/// Crate-derived encode mirror of [`frame_pcm`] for the mono and
/// plain-stereo shapes: per-channel PCM in, coded residual arrays out
/// (in stream order — Y then X for `>= 3950` stereo, X then Y below,
/// per §6.1), with predictor state trajectories identical to the
/// decode direction's. The staged reference pins only the decode
/// direction; this inverse exists to round-trip-validate the branches
/// no vendor fixture reaches (§6.13).
pub fn pcm_to_coded_arrays(
    channels: &[Vec<i32>],
    version: u16,
    level: CompressionLevel,
) -> Result<Vec<Vec<i32>>> {
    match channels {
        [mono] => {
            let mut x_pred = ArrayPredictor::new(version, level)?;
            Ok(vec![mono.iter().map(|&x| x_pred.encode(x, 0)).collect()])
        }
        [ch0, ch1] => {
            if ch0.len() != ch1.len() {
                return Err(Error::Malformed("PCM channels disagree on length"));
            }
            let mut x_pred = ArrayPredictor::new(version, level)?;
            let mut y_pred = ArrayPredictor::new(version, level)?;
            let n = ch0.len();
            let mut arr_first = Vec::with_capacity(n);
            let mut arr_second = Vec::with_capacity(n);
            let mut last_x = 0i32;
            for i in 0..n {
                // §6.9 encode direction: Y = s1 - s0; X = s0 + Y/2.
                let y = ch1[i].wrapping_sub(ch0[i]);
                let x = ch0[i].wrapping_add(y / 2);
                if version >= CROSS_TERM_VERSION {
                    arr_first.push(y_pred.encode(y, last_x));
                    arr_second.push(x_pred.encode(x, y));
                    last_x = x;
                } else {
                    arr_first.push(x_pred.encode(x, 0));
                    arr_second.push(y_pred.encode(y, 0));
                }
            }
            Ok(vec![arr_first, arr_second])
        }
        _ => Err(Error::Malformed(
            "encode mirror covers mono and plain stereo only",
        )),
    }
}

/// Assemble per-channel PCM into the stored interleaved byte order
/// (the byte stream the per-frame CRC covers and a WAV `data` chunk
/// carries), per the §6.9 bit-depth table: 8-bit stores `value + 128`
/// as one unsigned byte, 16-bit one little-endian `i16`, 24-bit three
/// little-endian bytes (two's-complement truncation).
pub fn interleave_pcm_bytes(channels: &[Vec<i32>], bits_per_sample: u16) -> Result<Vec<u8>> {
    let n = channels.first().map_or(0, Vec::len);
    if channels.iter().any(|c| c.len() != n) {
        return Err(Error::Malformed("PCM channels disagree on length"));
    }
    let bytes_per = match bits_per_sample {
        8 => 1usize,
        16 => 2,
        24 => 3,
        _ => {
            return Err(Error::Malformed(
                "bits-per-sample outside the staged {8, 16, 24} set",
            ))
        }
    };
    let mut out = Vec::with_capacity(n * channels.len() * bytes_per);
    for i in 0..n {
        for ch in channels {
            let v = ch[i];
            match bits_per_sample {
                8 => out.push(v.wrapping_add(128) as u8),
                16 => out.extend_from_slice(&(v as i16).to_le_bytes()),
                _ => out.extend_from_slice(&(v as u32).to_le_bytes()[..3]),
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::FramePrologue;

    fn residuals(arrays: Vec<Vec<i32>>, flags: Option<u32>) -> FrameResiduals {
        FrameResiduals {
            prologue: FramePrologue {
                crc31: 0,
                flags: flags.map(FrameFlags),
                len: if flags.is_some() { 8 } else { 4 },
            },
            arrays,
            end_bit_pos: 0,
            silent: false,
        }
    }

    #[test]
    fn reconstruct_pair_uses_truncating_division() {
        // Odd negative Y: -3 / 2 == -1 under truncation (an arithmetic
        // shift would give -2, which §6.12 pins as observably wrong).
        assert_eq!(reconstruct_sample_pair(10, -3), (11, 8));
        assert_eq!(reconstruct_sample_pair(10, 3), (9, 12));
        assert_eq!(reconstruct_sample_pair(0, 0), (0, 0));
    }

    #[test]
    fn reconstruct_inverts_the_pinned_encoder_form() {
        // §6.9 encode: Y = s1 - s0; X = s0 + Y/2. Round-trip a signed
        // grid including odd differences of both signs.
        for s0 in -5i32..=5 {
            for s1 in -5i32..=5 {
                let y = s1 - s0;
                let x = s0 + y / 2;
                assert_eq!(reconstruct_sample_pair(x, y), (s0, s1), "s0={s0} s1={s1}");
            }
        }
    }

    #[test]
    fn mono_first_sample_identity() {
        let res = residuals(vec![vec![1234, 0, 0, 0]], None);
        let pcm = frame_pcm(&res, 3990, CompressionLevel::Fast, 1).unwrap();
        assert_eq!(pcm.len(), 1);
        assert_eq!(pcm[0][0], 1234);
    }

    #[test]
    fn pseudo_stereo_duplicates_x_on_both_channels() {
        let res = residuals(vec![vec![5, -3, 8]], Some(FrameFlags::PSEUDO_STEREO));
        let pcm = frame_pcm(&res, 3990, CompressionLevel::Normal, 2).unwrap();
        assert_eq!(pcm.len(), 2);
        assert_eq!(pcm[0], pcm[1]);
    }

    #[test]
    fn stereo_zero_arrays_stay_zero() {
        let res = residuals(vec![vec![0; 32], vec![0; 32]], None);
        for version in [3930u16, 3949, 3950, 3990] {
            let pcm = frame_pcm(&res, version, CompressionLevel::Fast, 2).unwrap();
            assert!(pcm.iter().all(|c| c.iter().all(|&v| v == 0)));
        }
    }

    #[test]
    fn layout_mismatches_are_rejected() {
        let res = residuals(vec![vec![0; 4], vec![0; 5]], None);
        assert!(matches!(
            frame_pcm(&res, 3990, CompressionLevel::Fast, 2),
            Err(Error::Malformed(_))
        ));
        let res = residuals(vec![vec![0; 4]], None);
        assert!(matches!(
            frame_pcm(&res, 3990, CompressionLevel::Fast, 2),
            Err(Error::Malformed(_))
        ));
        let res = residuals(vec![vec![0; 4], vec![0; 4]], None);
        assert!(matches!(
            frame_pcm(&res, 3990, CompressionLevel::Fast, 1),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn unpinned_versions_surface_not_implemented() {
        let res = residuals(vec![vec![0; 4]], None);
        assert!(matches!(
            frame_pcm(&res, 3920, CompressionLevel::Fast, 1),
            Err(Error::NotImplemented)
        ));
    }

    /// Deterministic pseudo-noise for round-trip sweeps.
    fn noise(seed: u64, len: usize, bound: i32) -> Vec<i32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 33) as i32) % (2 * bound) - bound
            })
            .collect()
    }

    #[test]
    fn pcm_round_trips_through_the_encode_mirror_every_form_and_level() {
        // Every (version-form, level) pair the staged material covers,
        // mono and stereo, including all the branches no vendor
        // fixture reaches: the 3930 single-arm form, the era-B delta
        // rule (3950 with level < 5000), the level-5000 era quirk, and
        // the multi-stage cascade orderings.
        let versions_levels: &[(u16, &[CompressionLevel])] = &[
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
            (3979, &CompressionLevel::ALL[..]),
            (3990, &CompressionLevel::ALL[..]),
        ];
        for &(version, levels) in versions_levels {
            for &level in levels {
                // Mono.
                let mono = vec![noise(version as u64, 700, 20000)];
                let coded = pcm_to_coded_arrays(&mono, version, level).unwrap();
                let res = residuals(coded, None);
                let back = frame_pcm(&res, version, level, 1).unwrap();
                assert_eq!(back, mono, "mono v{version} {level:?}");
                // Stereo (correlated channels, so X/Y are both active).
                let ch0 = noise(version as u64 ^ 7, 700, 20000);
                let ch1: Vec<i32> = ch0
                    .iter()
                    .zip(noise(version as u64 ^ 9, 700, 3000))
                    .map(|(&a, b)| a + b)
                    .collect();
                let stereo = vec![ch0, ch1];
                let coded = pcm_to_coded_arrays(&stereo, version, level).unwrap();
                let res = residuals(coded, None);
                let back = frame_pcm(&res, version, level, 2).unwrap();
                assert_eq!(back, stereo, "stereo v{version} {level:?}");
            }
        }
    }

    #[test]
    fn interleave_16_bit_is_le_sample_pairs() {
        let bytes = interleave_pcm_bytes(&[vec![1, -2], vec![0x0100, -1]], 16).unwrap();
        assert_eq!(bytes, [0x01, 0x00, 0x00, 0x01, 0xFE, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn interleave_8_bit_biases_by_128() {
        let bytes = interleave_pcm_bytes(&[vec![0, -128, 127]], 8).unwrap();
        assert_eq!(bytes, [128, 0, 255]);
    }

    #[test]
    fn interleave_24_bit_truncates_two_complement() {
        let bytes = interleave_pcm_bytes(&[vec![1, -1, -8_388_608]], 24).unwrap();
        assert_eq!(bytes, [1, 0, 0, 0xFF, 0xFF, 0xFF, 0x00, 0x00, 0x80]);
    }

    #[test]
    fn interleave_rejects_ragged_and_unstaged_depths() {
        assert!(interleave_pcm_bytes(&[vec![0; 2], vec![0; 3]], 16).is_err());
        assert!(interleave_pcm_bytes(&[vec![0; 2]], 32).is_err());
        assert_eq!(interleave_pcm_bytes(&[], 16).unwrap(), Vec::<u8>::new());
    }
}
