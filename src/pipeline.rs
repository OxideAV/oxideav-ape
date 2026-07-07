//! Frame-decode orchestration per the wiki §"General Decoding Process".
//!
//! The staged snapshot pins the exact stage *ordering* of a frame
//! decode, reproduced verbatim:
//!
//! ```text
//!   read one frame of data
//!   unpack array of delta values from it
//!   if this is stereo signal then unpack the second array of delta values
//!   for each array
//!     apply all IIR filters onto values
//!   if this is stereo then do channel correlation
//!   output data
//! ```
//!
//! This module wires that ordering — and only that ordering — around
//! the primitives the crate already ships: the per-array filter walk
//! ([`crate::cascade`]) and the channel-correlation closed form
//! ([`crate::decorrelate`]). The *entropy layer* that produces the
//! delta arrays (the range decoder's renormalisation / byte-input
//! state machine and the `k`-parameter recurrence) is **not pinned**
//! by the staged docs, so it enters as the [`DeltaSource`] trait
//! boundary: whatever later phase pins the coder, its output plugs in
//! here without reshaping the frame walk. [`encode_frame`] is the
//! mirror-image walk (correlate → filter → pack into a [`DeltaSink`]),
//! giving the orchestrator an end-to-end self-consistency round-trip
//! while the real entropy layer is pending.
//!
//! ## Conventions the snapshot leaves open
//!
//! * Which unpacked array is the correlation's `X` and which is `Y` is
//!   not annotated; this module fixes **array 0 = X, array 1 = Y** as
//!   a crate-local convention a later trace may need to swap.
//! * The divide-vs-arithmetic-shift ambiguity in `R = X - Y/2` is
//!   carried through as [`CorrelationRounding`], mirroring the two
//!   spellings the [`crate::decorrelate`] primitives expose.

use crate::decorrelate::{
    decorrelate_pair, decorrelate_pair_arith_shift, reconstruct_block,
    reconstruct_block_arith_shift,
};
use crate::error::Result;

/// Channel shape of one frame — the wiki's decode walk distinguishes
/// exactly mono ("array of delta values") vs stereo ("the second array
/// of delta values").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameChannels {
    /// One delta array per frame.
    Mono,
    /// Two delta arrays per frame, channel-correlated after filtering.
    Stereo,
}

impl FrameChannels {
    /// Number of delta arrays the frame carries (1 or 2).
    pub const fn count(self) -> usize {
        match self {
            FrameChannels::Mono => 1,
            FrameChannels::Stereo => 2,
        }
    }
}

/// Which spelling of `R = X - Y/2` the correlation stage uses — the
/// one ambiguity the staged docs leave open in the closed form (Rust
/// integer division rounds toward zero, an arithmetic right shift
/// toward `-∞`; they differ for odd negative `Y` only). Each variant
/// pairs losslessly with its own inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CorrelationRounding {
    /// `Y / 2` — Rust (truncating) integer division.
    #[default]
    TruncatingDiv,
    /// `Y >> 1` — arithmetic right shift.
    ArithShift,
}

/// The entropy boundary: yields one frame's array of delta values per
/// channel. The range-coder state machine that would implement this
/// against real bytes is unpinned by the staged docs; a later phase
/// implements this trait, and tests drive the orchestrator with
/// vector-backed sources.
pub trait DeltaSource {
    /// Unpack the delta array for `channel` (0-based) into `out`,
    /// filling it completely.
    fn unpack_deltas(&mut self, channel: usize, out: &mut [i32]) -> Result<()>;
}

/// Mirror of [`DeltaSource`] for the encoder direction: consumes one
/// frame's residual array per channel.
pub trait DeltaSink {
    /// Pack the residual array for `channel` (0-based).
    fn pack_deltas(&mut self, channel: usize, deltas: &[i32]) -> Result<()>;
}

/// Decode one frame following the pinned §"General Decoding Process"
/// ordering: unpack every channel's delta array first (channel 0, then
/// channel 1 for stereo), then apply the caller's filter walk to each
/// array in channel order, then — for stereo — reconstruct `(L, R)`
/// from the filtered `(X, Y)` pair, and return the output arrays.
///
/// `filter(channel, values)` is the *"apply all IIR filters onto
/// values"* stage; pass a closure over [`crate::cascade::cascade_decode`]
/// with per-channel states (each channel keeps its own filter state).
/// The returned `Vec` holds one output array per channel: `[mono]`, or
/// `[left, right]` after correlation.
pub fn decode_frame<S, F>(
    source: &mut S,
    channels: FrameChannels,
    frame_len: usize,
    mut filter: F,
    rounding: CorrelationRounding,
) -> Result<Vec<Vec<i32>>>
where
    S: DeltaSource + ?Sized,
    F: FnMut(usize, &mut [i32]) -> Result<()>,
{
    // "unpack array of delta values" (+ the second array for stereo):
    // every unpack precedes any filtering, per the pinned ordering.
    let mut arrays: Vec<Vec<i32>> = Vec::with_capacity(channels.count());
    for ch in 0..channels.count() {
        let mut buf = vec![0i32; frame_len];
        source.unpack_deltas(ch, &mut buf)?;
        arrays.push(buf);
    }
    // "for each array / apply all IIR filters onto values".
    for (ch, arr) in arrays.iter_mut().enumerate() {
        filter(ch, arr)?;
    }
    // "if this is stereo then do channel correlation".
    match channels {
        FrameChannels::Mono => Ok(arrays),
        FrameChannels::Stereo => {
            let (x, y) = (&arrays[0], &arrays[1]);
            let mut left = vec![0i32; frame_len];
            let mut right = vec![0i32; frame_len];
            match rounding {
                CorrelationRounding::TruncatingDiv => {
                    reconstruct_block(x, y, &mut left, &mut right)?
                }
                CorrelationRounding::ArithShift => {
                    reconstruct_block_arith_shift(x, y, &mut left, &mut right)?
                }
            }
            Ok(vec![left, right])
        }
    }
}

/// Encoder-direction mirror of [`decode_frame`]: for stereo, first map
/// `(L, R)` to the decorrelated `(X, Y)` pair (the inverse of the
/// pinned correlation, under the same rounding variant), then run the
/// caller's filter walk over each array (encoder direction), then pack
/// every channel into the sink in channel order.
///
/// With mirrored filter closures and equal starting states,
/// `encode_frame` followed by [`decode_frame`] over the packed deltas
/// reproduces the input channels exactly — the orchestrator-level
/// self-consistency check available while the entropy layer is
/// unpinned.
pub fn encode_frame<K, F>(
    input: &[Vec<i32>],
    sink: &mut K,
    channels: FrameChannels,
    mut filter: F,
    rounding: CorrelationRounding,
) -> Result<()>
where
    K: DeltaSink + ?Sized,
    F: FnMut(usize, &mut [i32]) -> Result<()>,
{
    debug_assert_eq!(input.len(), channels.count());
    let mut arrays: Vec<Vec<i32>> = match channels {
        FrameChannels::Mono => vec![input[0].clone()],
        FrameChannels::Stereo => {
            let (l, r) = (&input[0], &input[1]);
            let mut x = vec![0i32; l.len()];
            let mut y = vec![0i32; l.len()];
            for i in 0..l.len() {
                let (xi, yi) = match rounding {
                    CorrelationRounding::TruncatingDiv => decorrelate_pair(l[i], r[i]),
                    CorrelationRounding::ArithShift => decorrelate_pair_arith_shift(l[i], r[i]),
                };
                x[i] = xi;
                y[i] = yi;
            }
            vec![x, y]
        }
    };
    for (ch, arr) in arrays.iter_mut().enumerate() {
        filter(ch, arr)?;
    }
    for (ch, arr) in arrays.iter().enumerate() {
        sink.pack_deltas(ch, arr)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cascade::{cascade_decode, cascade_encode, StageState};
    use crate::error::Error;
    use crate::filter_config::FilterCascade;
    use crate::header::CompressionLevel;

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// Vector-backed delta source: one pre-baked array per channel.
    struct VecSource {
        arrays: Vec<Vec<i32>>,
        calls: Vec<usize>,
    }

    impl DeltaSource for VecSource {
        fn unpack_deltas(&mut self, channel: usize, out: &mut [i32]) -> Result<()> {
            self.calls.push(channel);
            out.copy_from_slice(&self.arrays[channel]);
            Ok(())
        }
    }

    /// Vector-backed sink capturing packed residual arrays.
    #[derive(Default)]
    struct VecSink {
        arrays: Vec<(usize, Vec<i32>)>,
    }

    impl DeltaSink for VecSink {
        fn pack_deltas(&mut self, channel: usize, deltas: &[i32]) -> Result<()> {
            self.arrays.push((channel, deltas.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn channel_count_matches_the_wiki_shapes() {
        assert_eq!(FrameChannels::Mono.count(), 1);
        assert_eq!(FrameChannels::Stereo.count(), 2);
    }

    #[test]
    fn mono_decode_is_unpack_then_filter_only() {
        // Mono: no correlation stage; the output is the filtered array.
        let mut src = VecSource {
            arrays: vec![vec![5, -3, 7, 0]],
            calls: Vec::new(),
        };
        let out = decode_frame(
            &mut src,
            FrameChannels::Mono,
            4,
            |_ch, arr| {
                for v in arr.iter_mut() {
                    *v += 1; // marker "filter"
                }
                Ok(())
            },
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        assert_eq!(out, vec![vec![6, -2, 8, 1]]);
        assert_eq!(src.calls, vec![0]);
    }

    #[test]
    fn stereo_stage_ordering_is_pinned() {
        // The pinned listing: unpack ch0, unpack ch1, filter ch0,
        // filter ch1, correlate. Record the event trail and assert it.
        let mut src = VecSource {
            arrays: vec![vec![10, 4], vec![4, 2]],
            calls: Vec::new(),
        };
        let mut filter_calls = Vec::new();
        let out = decode_frame(
            &mut src,
            FrameChannels::Stereo,
            2,
            |ch, _arr| {
                filter_calls.push(ch);
                Ok(())
            },
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        // Both unpacks happened, in channel order, before filtering
        // (VecSource records its own trail; the filter trail follows).
        assert_eq!(src.calls, vec![0, 1]);
        assert_eq!(filter_calls, vec![0, 1]);
        // Correlation applied the README worked pair per sample:
        // (X, Y) = (10, 4) -> R = 8, L = 12; (4, 2) -> R = 3, L = 5.
        assert_eq!(out, vec![vec![12, 5], vec![8, 3]]);
    }

    #[test]
    fn stereo_uses_array0_as_x_and_array1_as_y() {
        // The crate-local convention: swapping the arrays must change
        // the output (Y is the difference channel).
        let mut src_a = VecSource {
            arrays: vec![vec![10], vec![4]],
            calls: Vec::new(),
        };
        let mut src_b = VecSource {
            arrays: vec![vec![4], vec![10]],
            calls: Vec::new(),
        };
        let no_filter = |_ch: usize, _arr: &mut [i32]| Ok(());
        let a = decode_frame(
            &mut src_a,
            FrameChannels::Stereo,
            1,
            no_filter,
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        let b = decode_frame(
            &mut src_b,
            FrameChannels::Stereo,
            1,
            no_filter,
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        assert_ne!(a, b);
        assert_eq!(a, vec![vec![12], vec![8]]);
    }

    #[test]
    fn rounding_variants_differ_exactly_on_odd_negative_y() {
        let mut src = VecSource {
            arrays: vec![vec![0], vec![-3]],
            calls: Vec::new(),
        };
        let no_filter = |_ch: usize, _arr: &mut [i32]| Ok(());
        let div = decode_frame(
            &mut src,
            FrameChannels::Stereo,
            1,
            no_filter,
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        let mut src2 = VecSource {
            arrays: vec![vec![0], vec![-3]],
            calls: Vec::new(),
        };
        let shift = decode_frame(
            &mut src2,
            FrameChannels::Stereo,
            1,
            no_filter,
            CorrelationRounding::ArithShift,
        )
        .unwrap();
        // Y = -3: R = 0 - (-1) = 1 (div) vs 0 - (-2) = 2 (shift).
        assert_eq!(div, vec![vec![-2], vec![1]]);
        assert_eq!(shift, vec![vec![-1], vec![2]]);
    }

    #[test]
    fn filter_errors_propagate() {
        let mut src = VecSource {
            arrays: vec![vec![1, 2]],
            calls: Vec::new(),
        };
        let err = decode_frame(
            &mut src,
            FrameChannels::Mono,
            2,
            |_ch, _arr| Err(Error::NotImplemented),
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap_err();
        assert_eq!(err, Error::NotImplemented);
    }

    #[test]
    fn frame_round_trips_end_to_end_for_every_level_and_rounding() {
        // encode_frame -> decode_frame over the full pinned cascade per
        // level, both rounding variants, stereo. The orchestrator-level
        // self-consistency: PCM in == PCM out, exactly.
        for level in CompressionLevel::ALL {
            for rounding in [
                CorrelationRounding::TruncatingDiv,
                CorrelationRounding::ArithShift,
            ] {
                let cascade = FilterCascade::for_level(level);
                let mut rng = 0xC0FF_EE00_u64 ^ u64::from(u16::from(level));
                let frame_len = 48usize;
                let left: Vec<i32> = (0..frame_len)
                    .map(|_| (xorshift(&mut rng) as i32) % 32768)
                    .collect();
                let right: Vec<i32> = (0..frame_len)
                    .map(|_| (xorshift(&mut rng) as i32) % 32768)
                    .collect();
                let input = vec![left.clone(), right.clone()];

                // Per-channel encoder filter states.
                let mut enc_states = [
                    StageState::for_cascade(&cascade),
                    StageState::for_cascade(&cascade),
                ];
                let mut sink = VecSink::default();
                encode_frame(
                    &input,
                    &mut sink,
                    FrameChannels::Stereo,
                    |ch, arr| cascade_encode(arr, &mut enc_states[ch], |_i, r, _f| r),
                    rounding,
                )
                .unwrap();
                assert_eq!(sink.arrays.len(), 2);
                assert_eq!(sink.arrays[0].0, 0);
                assert_eq!(sink.arrays[1].0, 1);

                // Feed the packed residuals back through the decoder.
                let mut dec_states = [
                    StageState::for_cascade(&cascade),
                    StageState::for_cascade(&cascade),
                ];
                let mut src = VecSource {
                    arrays: vec![sink.arrays[0].1.clone(), sink.arrays[1].1.clone()],
                    calls: Vec::new(),
                };
                let out = decode_frame(
                    &mut src,
                    FrameChannels::Stereo,
                    frame_len,
                    |ch, arr| cascade_decode(arr, &mut dec_states[ch], |_i, r, _f| r),
                    rounding,
                )
                .unwrap();
                assert_eq!(out[0], left, "{level:?}/{rounding:?} left");
                assert_eq!(out[1], right, "{level:?}/{rounding:?} right");
                assert_eq!(enc_states, dec_states, "{level:?}/{rounding:?} states");
            }
        }
    }

    #[test]
    fn mono_frame_round_trips() {
        let cascade = FilterCascade::for_level(CompressionLevel::High);
        let mut rng = 0x5EED_u64;
        let frame_len = 32usize;
        let pcm: Vec<i32> = (0..frame_len)
            .map(|_| (xorshift(&mut rng) as i32) % 65536)
            .collect();

        let mut enc_states = StageState::for_cascade(&cascade);
        let mut sink = VecSink::default();
        encode_frame(
            core::slice::from_ref(&pcm),
            &mut sink,
            FrameChannels::Mono,
            |_ch, arr| cascade_encode(arr, &mut enc_states, |_i, r, _f| r),
            CorrelationRounding::default(),
        )
        .unwrap();

        let mut dec_states = StageState::for_cascade(&cascade);
        let mut src = VecSource {
            arrays: vec![sink.arrays[0].1.clone()],
            calls: Vec::new(),
        };
        let out = decode_frame(
            &mut src,
            FrameChannels::Mono,
            frame_len,
            |_ch, arr| cascade_decode(arr, &mut dec_states, |_i, r, _f| r),
            CorrelationRounding::default(),
        )
        .unwrap();
        assert_eq!(out, vec![pcm]);
    }
}
