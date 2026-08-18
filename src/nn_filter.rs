//! Adaptive FIR filter stage — the staged
//! `docs/audio/ape/format-reference.md` §6.5 recurrence with the §6.6
//! per-version `delta[]` maintenance rule.
//!
//! One [`NnFilter`] instance is one `(order, shift)` stage of the
//! per-level cascade the staged `filter_config.csv` pins (§6.4); the
//! decode direction applies the per-level stages in **reverse
//! construction order** (§6.3), which [`crate::predict`] wires. Per
//! §6.5 the stage state is a 16-bit weight vector, two 16-bit rolling
//! buffers (input history + `delta[]` adaptation input) with a
//! 512-element window plus `order` history elements, and a 32-bit
//! running average (era A only).
//!
//! The decode step, in the staged order (§6.5):
//!
//! 1. 32-bit dot product of the last `order` history elements against
//!    the weight vector (products and accumulator wrap, never
//!    saturate);
//! 2. sign-sign weight update from the **incoming** value — a negative
//!    input **adds** the per-tap `delta[]` step, a positive input
//!    subtracts it — performed *before* the output is formed;
//! 3. `out = in + ((dot + (1 << (shift - 1))) >> shift)` — the
//!    half-LSB rounding constant sits before the arithmetic shift, and
//!    it is the only rounding constant anywhere in the predictor
//!    chain;
//! 4. the input history stores `out` **saturated** to `i16` (the one
//!    place the pipeline saturates rather than wraps, §6.11);
//! 5. `delta[]` maintenance per the §6.6 era rule (below);
//! 6. both rolling buffers advance, wrapping by copying their last
//!    `order` elements to the front.
//!
//! §6.6 pins the `delta[]` rule split at **file version 3980**:
//!
//! - **Era A (`>= 3980`)** — three magnitudes (32/16/8) gated by a
//!   running average of `|out|` (`> avg*3`, `> avg*4/3`, `> 0`), the
//!   truncating `avg += (|out| - avg) / 16` update, and lag-{1, 2, 8}
//!   halving decays.
//! - **Era B (`< 3980`)** — a single magnitude 4 with no running
//!   average, and lag-{4, 8} decays.
//!
//! Both eras write `delta[0]` from the **output** (decode direction)
//! with the §6.6 `sign_step` polarity: a negative output stores
//! `+magnitude`, a positive output `-magnitude`, zero stores zero. The
//! decays are arithmetic shifts on the signed 16-bit elements, so a
//! lone `-1` never decays to `0`.
//!
//! The level-5000 construction quirk (§6.4 — its filters take the era
//! rule of version 3990 regardless of the file version) is applied by
//! the caller via [`DeltaEra::for_stage`].

use crate::error::{Error, Result};
use crate::filter_config::FilterStage;
use crate::header::CompressionLevel;

/// Rolling-buffer window length shared by both §6.5 buffers.
pub const NN_WINDOW: usize = 512;

/// File-version boundary of the §6.6 `delta[]` maintenance split.
pub const DELTA_ERA_SPLIT_VERSION: u16 = 3980;

/// Which §6.6 `delta[]` maintenance rule a filter instance runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeltaEra {
    /// `nVersion >= 3980`: three magnitudes gated by a running average
    /// of `|out|`; lag-{1, 2, 8} decays.
    A,
    /// `nVersion < 3980`: single magnitude 4, no running average;
    /// lag-{4, 8} decays.
    B,
}

impl DeltaEra {
    /// The era a plain file-version dispatch selects (§6.6).
    pub const fn for_version(version: u16) -> Self {
        if version >= DELTA_ERA_SPLIT_VERSION {
            DeltaEra::A
        } else {
            DeltaEra::B
        }
    }

    /// The era for one cascade stage of a file — §6.4's level-5000
    /// construction quirk: an insane-level file builds its filters
    /// with the fixed version 3990 rather than the file's own, so a
    /// 3950–3979 level-5000 stream still gets era A.
    pub const fn for_stage(version: u16, level: CompressionLevel) -> Self {
        match level {
            CompressionLevel::Insane => DeltaEra::A,
            _ => Self::for_version(version),
        }
    }
}

/// §6.6 `sign_step`: `+magnitude` when `w` is negative, `-magnitude`
/// when positive. Callers handle `w == 0` before calling.
#[inline]
const fn sign_step(w: i32, magnitude: i16) -> i16 {
    if w < 0 {
        magnitude
    } else {
        -magnitude
    }
}

/// Saturating `i32 -> i16` narrow — §6.5 step 4, the only saturation
/// in the predictor chain.
#[inline]
const fn saturate_i16(v: i32) -> i16 {
    if v > i16::MAX as i32 {
        i16::MAX
    } else if v < i16::MIN as i32 {
        i16::MIN
    } else {
        v as i16
    }
}

/// One adaptive FIR stage (§6.5) with its §6.6 `delta[]` rule.
#[derive(Debug, Clone)]
pub struct NnFilter {
    order: usize,
    shift: u32,
    era: DeltaEra,
    /// Weight vector `M`, `order` elements (16-bit, wrapping updates).
    m: Vec<i16>,
    /// Input history rolling buffer (`NN_WINDOW + order`).
    input: Vec<i16>,
    /// `delta[]` adaptation-input rolling buffer (same shape).
    delta: Vec<i16>,
    /// Write cursor into both rolling buffers.
    cursor: usize,
    /// Era-A running average of `|out|`.
    avg: i32,
}

impl NnFilter {
    /// Build a stage from an `(order, shift)` pair. §6.4 pins that
    /// orders are positive multiples of 16 (the staged per-level table
    /// satisfies this for every stage with a filter); anything else is
    /// rejected rather than run off-spec.
    pub fn new(order: usize, shift: u32, era: DeltaEra) -> Result<Self> {
        if order == 0 || order % 16 != 0 {
            return Err(Error::Malformed(
                "adaptive-filter order must be a positive multiple of 16",
            ));
        }
        if shift == 0 || shift > 31 {
            return Err(Error::Malformed(
                "adaptive-filter shift outside the representable range",
            ));
        }
        Ok(NnFilter {
            order,
            shift,
            era,
            m: vec![0; order],
            input: vec![0; NN_WINDOW + order],
            delta: vec![0; NN_WINDOW + order],
            cursor: order,
            avg: 0,
        })
    }

    /// Build a stage straight from a staged cascade-table row,
    /// applying the §6.4 level-5000 era quirk.
    pub fn for_stage(stage: FilterStage, version: u16, level: CompressionLevel) -> Result<Self> {
        Self::new(
            usize::from(stage.order),
            u32::from(stage.shift),
            DeltaEra::for_stage(version, level),
        )
    }

    /// The stage's filter order (tap count).
    pub fn order(&self) -> usize {
        self.order
    }

    /// The stage's fractional shift.
    pub fn shift(&self) -> u32 {
        self.shift
    }

    /// The §6.6 era this instance runs.
    pub fn era(&self) -> DeltaEra {
        self.era
    }

    /// Reset to the per-frame state (§6.10.1): both buffers and the
    /// weight vector zeroed, cursor parked at `order`, average zeroed.
    pub fn reset(&mut self) {
        self.m.fill(0);
        self.input.fill(0);
        self.delta.fill(0);
        self.cursor = self.order;
        self.avg = 0;
    }

    /// §6.5 step 1: the 32-bit wrapping dot product over the last
    /// `order` history elements.
    fn dot(&self) -> i32 {
        let base = self.cursor - self.order;
        let mut acc = 0i32;
        for j in 0..self.order {
            acc = acc
                .wrapping_add(i32::from(self.input[base + j]).wrapping_mul(i32::from(self.m[j])));
        }
        acc
    }

    /// §6.5 step 2: sign-sign weight update from the incoming value.
    fn adapt_weights(&mut self, incoming: i32) {
        let base = self.cursor - self.order;
        match incoming.cmp(&0) {
            core::cmp::Ordering::Less => {
                for j in 0..self.order {
                    self.m[j] = self.m[j].wrapping_add(self.delta[base + j]);
                }
            }
            core::cmp::Ordering::Greater => {
                for j in 0..self.order {
                    self.m[j] = self.m[j].wrapping_sub(self.delta[base + j]);
                }
            }
            core::cmp::Ordering::Equal => {}
        }
    }

    /// §6.5 steps 4-6 shared by both directions: store the history
    /// element, run the §6.6 `delta[]` rule from `signal` (the decode
    /// direction's output / the encode direction's input — the same
    /// reconstructed signal), and advance the rolling buffers.
    fn push_and_advance(&mut self, signal: i32) {
        let c = self.cursor;
        self.input[c] = saturate_i16(signal);
        match self.era {
            DeltaEra::A => {
                let a = signal.wrapping_abs();
                self.delta[c] = if a > self.avg.wrapping_mul(3) {
                    sign_step(signal, 32)
                } else if a > self.avg.wrapping_mul(4) / 3 {
                    sign_step(signal, 16)
                } else if a > 0 {
                    sign_step(signal, 8)
                } else {
                    0
                };
                // Truncating division — the trajectory differs under a
                // floor division (§6.6).
                self.avg = self.avg.wrapping_add(a.wrapping_sub(self.avg) / 16);
                self.delta[c - 1] >>= 1;
                self.delta[c - 2] >>= 1;
                self.delta[c - 8] >>= 1;
            }
            DeltaEra::B => {
                self.delta[c] = if signal == 0 { 0 } else { sign_step(signal, 4) };
                self.delta[c - 4] >>= 1;
                self.delta[c - 8] >>= 1;
            }
        }
        self.cursor += 1;
        if self.cursor == NN_WINDOW + self.order {
            // Wrap: the last `order` elements slide to the front.
            self.input.copy_within(NN_WINDOW.., 0);
            self.delta.copy_within(NN_WINDOW.., 0);
            self.cursor = self.order;
        }
    }

    /// Decode-direction step (§6.5): residual in, signal out.
    pub fn decode(&mut self, residual: i32) -> i32 {
        let dot = self.dot();
        self.adapt_weights(residual);
        let rounding = 1i32 << (self.shift - 1);
        let out = residual.wrapping_add(dot.wrapping_add(rounding) >> self.shift);
        self.push_and_advance(out);
        out
    }

    /// Encode-direction mirror: signal in, residual out. The staged
    /// reference pins the encode expression as the same §6.5 form with
    /// a minus sign; the adaptation is keyed on the emitted residual
    /// (the value the decode direction receives) so the two directions
    /// hold identical state trajectories and round-trip exactly.
    ///
    /// §6.6 notes the vendor encode side always pairs with era A; this
    /// mirror follows the instance's own era so either decode branch
    /// can be exercised round-trip.
    pub fn encode(&mut self, signal: i32) -> i32 {
        let dot = self.dot();
        let rounding = 1i32 << (self.shift - 1);
        let residual = signal.wrapping_sub(dot.wrapping_add(rounding) >> self.shift);
        self.adapt_weights(residual);
        self.push_and_advance(signal);
        residual
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter_config::FilterCascade;

    /// Tiny deterministic PRNG for round-trip sweeps (xorshift).
    struct Rng(u64);
    impl Rng {
        /// Uniform-ish value in `[-bound, bound)`.
        fn next_i32(&mut self, bound: i32) -> i32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            ((self.0 >> 33) as i32) % (2 * bound) - bound
        }
    }

    #[test]
    fn rejects_off_spec_orders_and_shifts() {
        assert!(NnFilter::new(0, 11, DeltaEra::A).is_err());
        assert!(NnFilter::new(24, 11, DeltaEra::A).is_err());
        assert!(NnFilter::new(16, 0, DeltaEra::A).is_err());
        assert!(NnFilter::new(16, 32, DeltaEra::A).is_err());
        assert!(NnFilter::new(16, 11, DeltaEra::A).is_ok());
    }

    #[test]
    fn every_staged_stage_constructs() {
        for level in CompressionLevel::ALL {
            for stage in FilterCascade::for_level(level).stages() {
                if stage.order != 0 {
                    NnFilter::for_stage(*stage, 3990, level).unwrap();
                }
            }
        }
    }

    #[test]
    fn delta_era_dispatch_and_level_5000_quirk() {
        assert_eq!(DeltaEra::for_version(3979), DeltaEra::B);
        assert_eq!(DeltaEra::for_version(3980), DeltaEra::A);
        // §6.4: level 5000 constructs with the fixed 3990 version.
        assert_eq!(
            DeltaEra::for_stage(3950, CompressionLevel::Insane),
            DeltaEra::A
        );
        assert_eq!(
            DeltaEra::for_stage(3950, CompressionLevel::ExtraHigh),
            DeltaEra::B
        );
        assert_eq!(
            DeltaEra::for_stage(3990, CompressionLevel::Normal),
            DeltaEra::A
        );
    }

    #[test]
    fn zero_state_first_step_is_identity_plus_rounding_of_zero() {
        // Fresh state: dot = 0, so out = in + ((0 + 1<<(shift-1)) >> shift)
        // = in + 0 for every shift >= 1.
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        assert_eq!(f.decode(1234), 1234);
        let mut f = NnFilter::new(16, 11, DeltaEra::B).unwrap();
        assert_eq!(f.decode(-77), -77);
    }

    #[test]
    fn rounding_constant_sits_before_the_shift() {
        // Drive a state where dot != 0, then check the exact §6.5
        // step-3 arithmetic against a hand-tracked replica.
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        for v in [100, -50, 30, -20, 10, 5, -5, 60] {
            f.decode(v);
        }
        let dot = f.dot();
        // Replicate step 2's weight update on a clone to read the
        // pre-push prediction (the dot uses pre-update weights).
        let out = f.clone().decode(7);
        assert_eq!(out, 7i32.wrapping_add(dot.wrapping_add(1 << 10) >> 11));
    }

    #[test]
    fn weight_update_polarity_negative_adds() {
        // Prime one nonzero delta element, then check that a negative
        // incoming value ADDS it to the weight (§6.5 step 2 polarity).
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        f.decode(1000); // writes delta[cursor] = -32 (positive out)
        let before = f.m.clone();
        f.decode(-1); // negative incoming: m[j] += delta[base + j]
                      // The freshly written delta now sits at the top of the window:
                      // delta[base + 15] was the lag-0 write of the previous step
                      // (halved once by the era-A lag-1 decay of this step? no —
                      // decays happen after the update, in push_and_advance).
        assert_eq!(f.m[15], before[15].wrapping_add(-32));
        // A zero incoming value leaves the weights untouched.
        let frozen = f.m.clone();
        f.decode(0);
        assert_eq!(f.m, frozen);
    }

    #[test]
    fn era_a_decays_lags_1_2_8_and_era_b_lags_4_8() {
        // Seed a recognisable ramp into the delta window, run one step
        // with a zero input/output (which writes delta[0] = 0 and
        // touches nothing else), and read back exactly which lags of
        // the *write* cursor got halved.
        for (era, decayed_lags) in [
            (DeltaEra::A, &[1usize, 2, 8][..]),
            (DeltaEra::B, &[4, 8][..]),
        ] {
            let mut f = NnFilter::new(16, 11, era).unwrap();
            let w = f.cursor; // this step's write position
            for lag in 1..=10 {
                f.delta[w - lag] = 64;
            }
            f.decode(0);
            assert_eq!(f.delta[w], 0, "{era:?}: zero output stores zero");
            for lag in 1..=10 {
                let expected = if decayed_lags.contains(&lag) { 32 } else { 64 };
                assert_eq!(f.delta[w - lag], expected, "{era:?} lag {lag}");
            }
        }
    }

    #[test]
    fn era_a_magnitude_ladder_follows_the_running_average() {
        // §6.6 era A: magnitude 32 above avg*3, 16 above avg*4/3,
        // 8 above zero, 0 at zero. Pin the gates with a hand-set avg.
        let cases = [
            (30i32, 301i32, -32i16), // 301 > 90
            (30, 90, -16),           // not > 90, but > 40
            (30, 41, -16),
            (30, 40, -8), // not > (30*4)/3 = 40
            (30, 1, -8),
            (30, 0, 0),
            (30, -90, 16), // |out| = 90; sign_step(-90, 16) = +16
        ];
        for (avg, out_signal, expected) in cases {
            let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
            f.avg = avg;
            let w = f.cursor;
            // decode(v) with zero state yields out == v, driving the
            // delta write from exactly `out_signal`.
            f.decode(out_signal);
            assert_eq!(f.delta[w], expected, "avg={avg} out={out_signal}");
        }
    }

    #[test]
    fn arithmetic_decay_never_kills_minus_one() {
        // §6.6: the decays are arithmetic shifts on i16, so -1 >> 1
        // stays -1 (a lone -1 never decays to 0).
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        let c = f.cursor;
        f.delta[c - 1] = -1;
        f.decode(0);
        assert_eq!(f.delta[c - 1], -1);
    }

    #[test]
    fn history_saturates_but_output_does_not() {
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        let out = f.decode(1_000_000);
        assert_eq!(out, 1_000_000, "output is the full 32-bit value");
        assert_eq!(
            f.input[f.cursor - 1],
            i16::MAX,
            "history stores the saturated narrow"
        );
        let out = f.decode(-1_000_000);
        assert_eq!(out, -1_000_000);
        assert_eq!(f.input[f.cursor - 1], i16::MIN);
    }

    #[test]
    fn truncating_average_matches_the_staged_rule() {
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        f.decode(100); // avg = 0 + (100 - 0)/16 = 6
        assert_eq!(f.avg, 6);
        f.decode(1); // avg = 6 + (1 - 6)/16 = 6 (truncation toward zero)
        assert_eq!(f.avg, 6, "truncating division: -5/16 == 0");
    }

    #[test]
    fn encode_decode_round_trip_all_staged_stages_both_eras() {
        for (order, shift) in [(16usize, 11u32), (64, 11), (256, 13), (32, 10), (1280, 15)] {
            for era in [DeltaEra::A, DeltaEra::B] {
                let mut enc = NnFilter::new(order, shift, era).unwrap();
                let mut dec = NnFilter::new(order, shift, era).unwrap();
                let mut rng = Rng(0x1234_5678_9ABC_DEF0 ^ order as u64);
                for i in 0..1600 {
                    let x = if i % 97 == 0 { 0 } else { rng.next_i32(40000) };
                    let r = enc.encode(x);
                    assert_eq!(dec.decode(r), x, "stage ({order},{shift}) {era:?} step {i}");
                }
                // The two directions hold identical state trajectories.
                assert_eq!(enc.m, dec.m);
                assert_eq!(enc.delta, dec.delta);
                assert_eq!(enc.avg, dec.avg);
                assert_eq!(enc.cursor, dec.cursor);
            }
        }
    }

    #[test]
    fn round_trip_survives_the_rolling_buffer_wrap() {
        // 1600 steps crosses the 512-element window multiple times for
        // order 16 (wrap at cursor 528); pin the wrap geometry too.
        let mut f = NnFilter::new(16, 11, DeltaEra::A).unwrap();
        for i in 0..NN_WINDOW {
            f.decode(i as i32);
        }
        assert_eq!(f.cursor, 16, "cursor re-parks at order after the wrap");
    }

    #[test]
    fn reset_restores_the_fresh_trajectory() {
        let mut f = NnFilter::new(32, 10, DeltaEra::B).unwrap();
        let fresh: Vec<i32> = {
            let mut g = f.clone();
            (0..64).map(|i| g.decode(i * 37 % 101 - 50)).collect()
        };
        for i in 0..500 {
            f.decode(i);
        }
        f.reset();
        let after: Vec<i32> = (0..64).map(|i| f.decode(i * 37 % 101 - 50)).collect();
        assert_eq!(fresh, after);
    }

    #[test]
    fn extreme_inputs_never_panic() {
        for era in [DeltaEra::A, DeltaEra::B] {
            let mut f = NnFilter::new(16, 11, era).unwrap();
            for v in [i32::MAX, i32::MIN, i32::MIN + 1, 0, -1, 1] {
                let _ = f.decode(v);
                let _ = f.encode(v);
            }
        }
    }
}
