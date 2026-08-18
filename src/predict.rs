//! Predictor composition — the staged
//! `docs/audio/ape/format-reference.md` §6 chain that turns one coded
//! array's entropy output into one decorrelated channel signal.
//!
//! Per §6.3 the decode direction runs, per value:
//!
//! 1. the per-level adaptive FIR cascade ([`crate::nn_filter`]) in
//!    **reverse construction order** (innermost / smallest order
//!    first);
//! 2. the integer offset predictor (§6.7) — the two-arm 4-tap + 5-tap
//!    form with the cross-channel term for `>= 3950` files
//!    ([`OffsetPredictor3950`]), or the single-arm 4-tap form for
//!    3930–3949 files ([`OffsetPredictor3930`]);
//! 3. the scaled first-order stage `state = v + ((state * 31) >> 5)`
//!    (§6.8, [`FirstOrderFilter`]) — a separate outermost stage in the
//!    `>= 3950` form, folded into the offset predictor's own output in
//!    the 3930-era form.
//!
//! [`ArrayPredictor`] owns one such chain (one per coded array; the X
//! and Y instances of §6.1 are two values of this type), performs the
//! §6.2 version dispatch, and applies the §6.4 level-5000 filter-era
//! quirk. Files below version 3930 are outside the staged material
//! (§6.2 pins that the staged-era decoder rejects them outright) and
//! surface [`Error::NotImplemented`].
//!
//! All arithmetic is 32-bit and wraps on overflow (§6.11); every shift
//! is arithmetic; the offset predictor and first-order stages carry
//! **no** rounding constant (only the FIR stage does, §6.5 step 3).

use crate::error::{Error, Result};
use crate::filter_config::FilterCascade;
use crate::header::CompressionLevel;
use crate::nn_filter::NnFilter;

/// Lowest file version the staged §6 predictor material covers.
pub const PREDICTOR_MIN_VERSION: u16 = 3930;

/// File-version boundary between the single-arm (§6.7.2) and two-arm
/// (§6.7.1) offset-predictor forms — also the boundary that flips the
/// stereo coded-array order and enables the cross-channel term (§6.1).
pub const CROSS_TERM_VERSION: u16 = 3950;

/// Offset-predictor rolling-buffer window (§6.7: window 512).
const OFFSET_WINDOW: usize = 512;

/// Offset-predictor history depth (§6.7: history 8).
const OFFSET_HISTORY: usize = 8;

/// §6.7.1 combine shift — fixed, level- and version-independent.
const COMBINE_SHIFT_3950: u32 = 10;

/// §6.7.2 combine shift — the one-arm form shifts by 9, not 10.
const COMBINE_SHIFT_3930: u32 = 9;

/// The four §6.7 weight seeds of the A arm (index 0..=3); the
/// remaining elements and the whole B arm seed to zero. `317` is the
/// `predictor_history_seed` scalar already staged in
/// `docs/audio/ape-cleanroom/tables/scalars.csv`; the other three are
/// pinned by the format reference §6.7.1 (its perturbation battery
/// §6.12 shows each of the four is load-bearing).
pub const OFFSET_WEIGHT_SEEDS: [i32; 4] = [360, 317, -109, 98];

/// §6.7.1 adaptation sign: `0` for a zero prediction slot, `+1` for a
/// negative one, `-1` for a positive one.
#[inline]
const fn sign_flag(p: i32) -> i32 {
    if p == 0 {
        0
    } else if p < 0 {
        1
    } else {
        -1
    }
}

/// The §6.8 scaled first-order filter — one word of state, constants
/// `31` / `5` (the `stage1_filter_weight` / `stage1_filter_shift`
/// scalars), no rounding constant, 32-bit wrapping arithmetic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FirstOrderFilter {
    state: i32,
}

impl FirstOrderFilter {
    /// Fresh (zero-state) filter — the per-frame reset value (§6.8).
    pub const fn new() -> Self {
        FirstOrderFilter { state: 0 }
    }

    /// Decompress direction: `state = v + ((state * 31) >> 5)`,
    /// output the new state.
    #[inline]
    pub fn decompress(&mut self, v: i32) -> i32 {
        self.state = v.wrapping_add(self.state.wrapping_mul(31) >> 5);
        self.state
    }

    /// Compress direction: `output = v - ((state * 31) >> 5)`, then
    /// `state = v`.
    #[inline]
    pub fn compress(&mut self, v: i32) -> i32 {
        let out = v.wrapping_sub(self.state.wrapping_mul(31) >> 5);
        self.state = v;
        out
    }
}

/// Shared §6.7 rolling-buffer geometry: window 512 + history 8, write
/// cursor parked at 8, wrap by copying the last 8 elements forward.
#[derive(Debug, Clone)]
struct OffsetBuffers<const N: usize> {
    bufs: [Vec<i32>; N],
    cursor: usize,
}

impl<const N: usize> OffsetBuffers<N> {
    fn new() -> Self {
        OffsetBuffers {
            bufs: core::array::from_fn(|_| vec![0i32; OFFSET_WINDOW + OFFSET_HISTORY]),
            cursor: OFFSET_HISTORY,
        }
    }

    fn advance(&mut self) {
        self.cursor += 1;
        if self.cursor == OFFSET_WINDOW + OFFSET_HISTORY {
            for b in &mut self.bufs {
                b.copy_within(OFFSET_WINDOW.., 0);
            }
            self.cursor = OFFSET_HISTORY;
        }
    }
}

/// §6.7.1 — the `>= 3950` two-arm integer offset predictor: a 4-tap
/// arm over the signal's own history and a 5-tap arm over the
/// cross-channel term, combined at `v + ((A + (B >> 1)) >> 10)`, with
/// unit-step sign-sign weight adaptation.
#[derive(Debug, Clone)]
pub struct OffsetPredictor3950 {
    /// `[pred_a, pred_b, adapt_a, adapt_b]` rolling buffers.
    bufs: OffsetBuffers<4>,
    m_a: [i32; OFFSET_HISTORY],
    m_b: [i32; OFFSET_HISTORY],
    last_a: i32,
    /// The compress-direction first-order filter on the incoming cross
    /// term (§6.7.1 step 1 / §6.8).
    cross_filter: FirstOrderFilter,
}

impl Default for OffsetPredictor3950 {
    fn default() -> Self {
        Self::new()
    }
}

impl OffsetPredictor3950 {
    /// Fresh per-frame state: zero buffers, the §6.7 A-arm weight
    /// seeds, an all-zero B arm.
    pub fn new() -> Self {
        let mut m_a = [0i32; OFFSET_HISTORY];
        m_a[..OFFSET_WEIGHT_SEEDS.len()].copy_from_slice(&OFFSET_WEIGHT_SEEDS);
        OffsetPredictor3950 {
            bufs: OffsetBuffers::new(),
            m_a,
            m_b: [0; OFFSET_HISTORY],
            last_a: 0,
            cross_filter: FirstOrderFilter::new(),
        }
    }

    /// One §6.7.1 block: `v` arrives from the FIR cascade, `cross` is
    /// the other signal's term (§6.1 — the previous block's X for the
    /// Y instance, this block's Y for the X instance, `0` for mono /
    /// pseudo-stereo). Returns the **pre-stage-1** value (`current`);
    /// the caller feeds it through the outer [`FirstOrderFilter`].
    pub fn step(&mut self, v: i32, cross: i32) -> i32 {
        let c = self.bufs.cursor;
        let [pred_a, pred_b, adapt_a, adapt_b] = &mut self.bufs.bufs;

        // 1. Push history; each arm's lag-1 slot becomes a first
        //    difference; the B arm's lag-0 slot is the compress-form
        //    first-order filter of the cross term.
        pred_a[c] = self.last_a;
        pred_a[c - 1] = pred_a[c].wrapping_sub(pred_a[c - 1]);
        pred_b[c] = self.cross_filter.compress(cross);
        pred_b[c - 1] = pred_b[c].wrapping_sub(pred_b[c - 1]);

        // Refresh the adaptation signs for lags 0 and 1 (the deeper
        // slots are frozen after their second write, so their stored
        // signs stay correct — §6.7.1).
        adapt_a[c] = sign_flag(pred_a[c]);
        adapt_a[c - 1] = sign_flag(pred_a[c - 1]);
        adapt_b[c] = sign_flag(pred_b[c]);
        adapt_b[c - 1] = sign_flag(pred_b[c - 1]);

        // 2. Two dot products — 4 taps on A, 5 taps on B.
        let mut prediction_a = 0i32;
        for (i, w) in self.m_a.iter().enumerate().take(4) {
            prediction_a = prediction_a.wrapping_add(pred_a[c - i].wrapping_mul(*w));
        }
        let mut prediction_b = 0i32;
        for (i, w) in self.m_b.iter().enumerate().take(5) {
            prediction_b = prediction_b.wrapping_add(pred_b[c - i].wrapping_mul(*w));
        }

        // 3. Combine — B enters at half weight, then the fixed shift.
        let current =
            v.wrapping_add(prediction_a.wrapping_add(prediction_b >> 1) >> COMBINE_SHIFT_3950);

        // Unit-step sign-sign weight update, keyed on the sign of the
        // *input* v (±1 per tap per block).
        match v.cmp(&0) {
            core::cmp::Ordering::Greater => {
                for i in 0..4 {
                    self.m_a[i] = self.m_a[i].wrapping_sub(adapt_a[c - i]);
                }
                for i in 0..5 {
                    self.m_b[i] = self.m_b[i].wrapping_sub(adapt_b[c - i]);
                }
            }
            core::cmp::Ordering::Less => {
                for i in 0..4 {
                    self.m_a[i] = self.m_a[i].wrapping_add(adapt_a[c - i]);
                }
                for i in 0..5 {
                    self.m_b[i] = self.m_b[i].wrapping_add(adapt_b[c - i]);
                }
            }
            core::cmp::Ordering::Equal => {}
        }

        // The history stores the value BEFORE the scaled first-order
        // stage (§6.7.1).
        self.last_a = current;
        self.bufs.advance();
        current
    }

    /// Crate-derived encode mirror of [`Self::step`] (the staged
    /// reference pins only the decode direction): given the target
    /// pre-stage-1 value `current`, emit the value `v` the decode step
    /// would need to reproduce it, holding an identical state
    /// trajectory. Solving `current = v + P` for `v` — every other
    /// quantity in the step is independent of `v` except the weight
    /// update, which keys on the recovered `v`'s sign.
    pub fn step_encode(&mut self, current: i32, cross: i32) -> i32 {
        let c = self.bufs.cursor;
        let [pred_a, pred_b, adapt_a, adapt_b] = &mut self.bufs.bufs;

        pred_a[c] = self.last_a;
        pred_a[c - 1] = pred_a[c].wrapping_sub(pred_a[c - 1]);
        pred_b[c] = self.cross_filter.compress(cross);
        pred_b[c - 1] = pred_b[c].wrapping_sub(pred_b[c - 1]);

        adapt_a[c] = sign_flag(pred_a[c]);
        adapt_a[c - 1] = sign_flag(pred_a[c - 1]);
        adapt_b[c] = sign_flag(pred_b[c]);
        adapt_b[c - 1] = sign_flag(pred_b[c - 1]);

        let mut prediction_a = 0i32;
        for (i, w) in self.m_a.iter().enumerate().take(4) {
            prediction_a = prediction_a.wrapping_add(pred_a[c - i].wrapping_mul(*w));
        }
        let mut prediction_b = 0i32;
        for (i, w) in self.m_b.iter().enumerate().take(5) {
            prediction_b = prediction_b.wrapping_add(pred_b[c - i].wrapping_mul(*w));
        }

        let v = current
            .wrapping_sub(prediction_a.wrapping_add(prediction_b >> 1) >> COMBINE_SHIFT_3950);

        match v.cmp(&0) {
            core::cmp::Ordering::Greater => {
                for i in 0..4 {
                    self.m_a[i] = self.m_a[i].wrapping_sub(adapt_a[c - i]);
                }
                for i in 0..5 {
                    self.m_b[i] = self.m_b[i].wrapping_sub(adapt_b[c - i]);
                }
            }
            core::cmp::Ordering::Less => {
                for i in 0..4 {
                    self.m_a[i] = self.m_a[i].wrapping_add(adapt_a[c - i]);
                }
                for i in 0..5 {
                    self.m_b[i] = self.m_b[i].wrapping_add(adapt_b[c - i]);
                }
            }
            core::cmp::Ordering::Equal => {}
        }

        self.last_a = current;
        self.bufs.advance();
        v
    }
}

/// §6.7.2 — the 3930–3949 single-arm integer offset predictor: four
/// explicit first-difference taps, a fixed `>> 9` combine, and the
/// order-1 stage folded into its own output.
#[derive(Debug, Clone)]
pub struct OffsetPredictor3930 {
    bufs: OffsetBuffers<1>,
    m: [i32; OFFSET_HISTORY],
    last_out: i32,
}

impl Default for OffsetPredictor3930 {
    fn default() -> Self {
        Self::new()
    }
}

impl OffsetPredictor3930 {
    /// Fresh per-frame state with the same §6.7 weight seeds in a
    /// single 8-element array.
    pub fn new() -> Self {
        let mut m = [0i32; OFFSET_HISTORY];
        m[..OFFSET_WEIGHT_SEEDS.len()].copy_from_slice(&OFFSET_WEIGHT_SEEDS);
        OffsetPredictor3930 {
            bufs: OffsetBuffers::new(),
            m,
            last_out: 0,
        }
    }

    /// One §6.7.2 block: `v` arrives from the FIR cascade; the return
    /// value is the finished channel signal (this form folds the
    /// order-1 stage in directly). No cross-channel term exists in
    /// this era (§6.2).
    pub fn step(&mut self, v: i32) -> i32 {
        let c = self.bufs.cursor;
        let hist = &mut self.bufs.bufs[0];

        let p1 = hist[c - 1];
        let p2 = hist[c - 1].wrapping_sub(hist[c - 2]);
        let p3 = hist[c - 2].wrapping_sub(hist[c - 3]);
        let p4 = hist[c - 3].wrapping_sub(hist[c - 4]);

        let dot = p1
            .wrapping_mul(self.m[0])
            .wrapping_add(p2.wrapping_mul(self.m[1]))
            .wrapping_add(p3.wrapping_mul(self.m[2]))
            .wrapping_add(p4.wrapping_mul(self.m[3]));
        hist[c] = v.wrapping_add(dot >> COMBINE_SHIFT_3930);

        // Same ±1 sign-sign rule and polarity as §6.7.1, over the four
        // taps (a zero tap contributes no step, matching the §6.7.1
        // zero-slot adaptation sign).
        match v.cmp(&0) {
            core::cmp::Ordering::Greater => {
                for (w, p) in self.m.iter_mut().zip([p1, p2, p3, p4]) {
                    *w = w.wrapping_sub(sign_flag(p));
                }
            }
            core::cmp::Ordering::Less => {
                for (w, p) in self.m.iter_mut().zip([p1, p2, p3, p4]) {
                    *w = w.wrapping_add(sign_flag(p));
                }
            }
            core::cmp::Ordering::Equal => {}
        }

        // The folded order-1 stage: the buffer keeps the pre-stage-1
        // value, the `last_out` scalar the post-stage-1 value (§6.7.2).
        let out = hist[c].wrapping_add(self.last_out.wrapping_mul(31) >> 5);
        self.last_out = out;
        self.bufs.advance();
        out
    }

    /// Crate-derived encode mirror of [`Self::step`]: given the target
    /// channel signal `out`, emit the value the decode step would need
    /// to reproduce it, holding an identical state trajectory.
    pub fn step_encode(&mut self, out: i32) -> i32 {
        let c = self.bufs.cursor;
        let hist = &mut self.bufs.bufs[0];

        let p1 = hist[c - 1];
        let p2 = hist[c - 1].wrapping_sub(hist[c - 2]);
        let p3 = hist[c - 2].wrapping_sub(hist[c - 3]);
        let p4 = hist[c - 3].wrapping_sub(hist[c - 4]);

        let dot = p1
            .wrapping_mul(self.m[0])
            .wrapping_add(p2.wrapping_mul(self.m[1]))
            .wrapping_add(p3.wrapping_mul(self.m[2]))
            .wrapping_add(p4.wrapping_mul(self.m[3]));

        // Unfold the folded order-1 stage first, then the dot term.
        let pre_stage1 = out.wrapping_sub(self.last_out.wrapping_mul(31) >> 5);
        let v = pre_stage1.wrapping_sub(dot >> COMBINE_SHIFT_3930);
        hist[c] = pre_stage1;

        match v.cmp(&0) {
            core::cmp::Ordering::Greater => {
                for (w, p) in self.m.iter_mut().zip([p1, p2, p3, p4]) {
                    *w = w.wrapping_sub(sign_flag(p));
                }
            }
            core::cmp::Ordering::Less => {
                for (w, p) in self.m.iter_mut().zip([p1, p2, p3, p4]) {
                    *w = w.wrapping_add(sign_flag(p));
                }
            }
            core::cmp::Ordering::Equal => {}
        }

        self.last_out = out;
        self.bufs.advance();
        v
    }
}

/// The complete per-coded-array decode chain of §6.3: FIR cascade →
/// integer offset predictor → scaled first-order stage.
#[derive(Debug, Clone)]
pub struct ArrayPredictor {
    /// FIR stages in decode application order (reverse of the §6.4
    /// construction order).
    nn: Vec<NnFilter>,
    form: PredictorForm,
}

#[derive(Debug, Clone)]
enum PredictorForm {
    /// `>= 3950`: two-arm offset predictor + separate outer stage-1.
    V3950 {
        offset: OffsetPredictor3950,
        output: FirstOrderFilter,
    },
    /// 3930–3949: single-arm offset predictor with stage-1 folded in.
    V3930 { offset: OffsetPredictor3930 },
}

impl ArrayPredictor {
    /// Build the chain for one coded array of a
    /// `(file version, compression level)` stream, applying the §6.2
    /// form dispatch and the §6.4 level-5000 era quirk.
    ///
    /// Versions below 3930 are not covered by the staged material
    /// (§6.2) and return [`Error::NotImplemented`]; §6.2 also pins
    /// that the 3930-era form rejects level 5000.
    pub fn new(version: u16, level: CompressionLevel) -> Result<Self> {
        if version < PREDICTOR_MIN_VERSION {
            return Err(Error::NotImplemented);
        }
        if version < CROSS_TERM_VERSION && level == CompressionLevel::Insane {
            return Err(Error::Malformed(
                "level 5000 requires the >= 3950 predictor form",
            ));
        }
        let cascade = FilterCascade::for_level(level);
        let mut nn = Vec::with_capacity(cascade.len());
        // Decode application order is the reverse of construction
        // order (§6.3); an order-0 row (the fast level) runs no stage.
        for stage in cascade.stages().iter().rev() {
            if stage.order != 0 {
                nn.push(NnFilter::for_stage(*stage, version, level)?);
            }
        }
        let form = if version >= CROSS_TERM_VERSION {
            PredictorForm::V3950 {
                offset: OffsetPredictor3950::new(),
                output: FirstOrderFilter::new(),
            }
        } else {
            PredictorForm::V3930 {
                offset: OffsetPredictor3930::new(),
            }
        };
        Ok(ArrayPredictor { nn, form })
    }

    /// Whether this chain consumes a cross-channel term (§6.1: only
    /// the `>= 3950` form does).
    pub fn uses_cross_term(&self) -> bool {
        matches!(self.form, PredictorForm::V3950 { .. })
    }

    /// Decode one value: entropy residual in, channel signal out.
    /// `cross` is the §6.1 cross-channel term; the 3930-era form
    /// ignores it (that era has none).
    pub fn decode(&mut self, residual: i32, cross: i32) -> i32 {
        let mut v = residual;
        for f in &mut self.nn {
            v = f.decode(v);
        }
        match &mut self.form {
            PredictorForm::V3950 { offset, output } => {
                let current = offset.step(v, cross);
                output.decompress(current)
            }
            PredictorForm::V3930 { offset } => offset.step(v),
        }
    }

    /// Crate-derived encode mirror of [`Self::decode`]: channel signal
    /// in, entropy residual out, with a state trajectory identical to
    /// the decode direction's — so `decode(encode(x, c), c) == x` at
    /// every step for any signal. The staged reference pins only the
    /// decode direction; this inverse exists for round-trip validation
    /// of the branches no vendor fixture can reach (§6.13).
    pub fn encode(&mut self, signal: i32, cross: i32) -> i32 {
        let mut v = match &mut self.form {
            PredictorForm::V3950 { offset, output } => {
                let current = output.compress(signal);
                offset.step_encode(current, cross)
            }
            PredictorForm::V3930 { offset } => offset.step_encode(signal),
        };
        for f in self.nn.iter_mut().rev() {
            v = f.encode(v);
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scalars::PREDICTOR_HISTORY_SEED;

    #[test]
    fn weight_seed_317_is_the_staged_history_seed() {
        // The scalars.csv `predictor_history_seed` is the §6.7 A-arm
        // index-1 seed; cross-check the transcriptions agree.
        assert_eq!(OFFSET_WEIGHT_SEEDS[1], PREDICTOR_HISTORY_SEED);
    }

    #[test]
    fn first_order_filter_round_trips() {
        let mut comp = FirstOrderFilter::new();
        let mut deco = FirstOrderFilter::new();
        let xs = [0i32, 5, -3, 100, 99, 98, -1000, 12345, 0, 0, 7];
        for &x in &xs {
            let r = comp.compress(x);
            assert_eq!(deco.decompress(r), x);
        }
        assert_eq!(comp.state, deco.state);
    }

    #[test]
    fn first_order_decompress_matches_the_pinned_recurrence() {
        // state' = v + ((state * 31) >> 5), no rounding constant,
        // arithmetic shift.
        let mut f = FirstOrderFilter::new();
        assert_eq!(f.decompress(64), 64);
        assert_eq!(f.decompress(0), 62); // 64*31 >> 5 = 62
        assert_eq!(f.decompress(0), 60); // 62*31 = 1922, >> 5 = 60
        let mut f = FirstOrderFilter { state: -1 };
        assert_eq!(f.decompress(0), -1, "arithmetic shift: -31 >> 5 = -1");
    }

    #[test]
    fn offset_3950_first_blocks_track_the_seeded_arm() {
        // Fresh state, zero cross term. Block 1: all history zero,
        // prediction 0, current = v. Block 2: pred_a[0] = last_a = v1,
        // lag-1 slot becomes v1 - 0 = v1, so
        // prediction_a = v1*360 + v1*317; B arm stays all-zero.
        let mut p = OffsetPredictor3950::new();
        assert_eq!(p.step(1000, 0), 1000);
        let expected = 20i32 + (1000 * 360 + 1000 * 317) / 1024;
        // v = 20 > 0; the >> 10 is arithmetic on a positive sum here.
        assert_eq!(p.step(20, 0), expected);
    }

    #[test]
    fn offset_3950_weight_update_is_unit_step() {
        let mut p = OffsetPredictor3950::new();
        p.step(1000, 500);
        let m_a0 = p.m_a;
        let m_b0 = p.m_b;
        // Positive input: every tap moves by -sign(slot), i.e. at most
        // 1 in magnitude.
        p.step(7, -100);
        for (before, after) in m_a0.iter().zip(p.m_a.iter()) {
            assert!((after - before).abs() <= 1);
        }
        for (before, after) in m_b0.iter().zip(p.m_b.iter()) {
            assert!((after - before).abs() <= 1);
        }
        // Zero input freezes the weights.
        let (ma, mb) = (p.m_a, p.m_b);
        p.step(0, 42);
        assert_eq!(ma, p.m_a);
        assert_eq!(mb, p.m_b);
    }

    #[test]
    fn offset_3950_cross_arm_sees_the_filtered_cross_term() {
        // The B arm's lag-0 slot is the compress-direction first-order
        // filter of the cross term: c - ((prev*31)>>5).
        let mut p = OffsetPredictor3950::new();
        p.step(0, 64);
        let c = p.bufs.cursor;
        let pred_b = &p.bufs.bufs[1];
        assert_eq!(pred_b[c - 1], 64, "first block: 64 - 0");
        p.step(0, 64);
        // Second block wrote 64 - (64*31>>5) = 64 - 62 = 2 at lag 0,
        // then the lag-1 slot became 2 - 64 = -62 on... not yet — the
        // difference rewrite happens on the NEXT push. Lag 1 (the slot
        // just written) still holds 2.
        let c = p.bufs.cursor;
        assert_eq!(p.bufs.bufs[1][c - 1], 2);
    }

    #[test]
    fn offset_3930_folds_the_order_1_stage() {
        let mut p = OffsetPredictor3930::new();
        // Block 1: empty history -> hist = v, out = v + 0.
        assert_eq!(p.step(100), 100);
        // Block 2: p1 = 100, p2 = 100, p3 = p4 = 0 ->
        // dot = 100*360 + 100*317 = 67700, >> 9 = 132;
        // hist = 5 + 132 = 137; out = 137 + ((100*31)>>5 = 96) = 233.
        assert_eq!(p.step(5), 233);
    }

    #[test]
    fn array_predictor_version_dispatch() {
        assert!(matches!(
            ArrayPredictor::new(3920, CompressionLevel::Fast),
            Err(Error::NotImplemented)
        ));
        assert!(matches!(
            ArrayPredictor::new(3930, CompressionLevel::Insane),
            Err(Error::Malformed(_))
        ));
        let p = ArrayPredictor::new(3930, CompressionLevel::ExtraHigh).unwrap();
        assert!(!p.uses_cross_term());
        let p = ArrayPredictor::new(3950, CompressionLevel::Insane).unwrap();
        assert!(p.uses_cross_term());
        let p = ArrayPredictor::new(3990, CompressionLevel::Fast).unwrap();
        assert!(p.uses_cross_term());
        assert!(p.nn.is_empty(), "fast level runs no FIR stage");
    }

    #[test]
    fn cascade_applies_in_reverse_construction_order() {
        // Insane constructs (1280,15), (256,13), (16,11); decode must
        // apply (16,11) first (§6.3).
        let p = ArrayPredictor::new(3990, CompressionLevel::Insane).unwrap();
        let orders: Vec<usize> = p.nn.iter().map(|f| f.order()).collect();
        assert_eq!(orders, [16, 256, 1280]);
        let shifts: Vec<u32> = p.nn.iter().map(|f| f.shift()).collect();
        assert_eq!(shifts, [11, 13, 15]);
        let p = ArrayPredictor::new(3990, CompressionLevel::ExtraHigh).unwrap();
        let orders: Vec<usize> = p.nn.iter().map(|f| f.order()).collect();
        assert_eq!(orders, [32, 256]);
    }

    #[test]
    fn fast_level_zero_history_first_value_is_identity() {
        // With no FIR stage and all-zero offset history, the first
        // decoded value passes through unchanged — the empirical
        // first-sample identity the fixture corpus pins.
        for version in [3950, 3990] {
            let mut p = ArrayPredictor::new(version, CompressionLevel::Fast).unwrap();
            assert_eq!(p.decode(-674, 0), -674);
        }
        let mut p = ArrayPredictor::new(3930, CompressionLevel::Fast).unwrap();
        assert_eq!(p.decode(1234, 0), 1234);
    }

    #[test]
    fn zero_stream_stays_zero_for_every_form_and_level() {
        for (version, levels) in [
            (
                3930u16,
                &[
                    CompressionLevel::Fast,
                    CompressionLevel::Normal,
                    CompressionLevel::High,
                    CompressionLevel::ExtraHigh,
                ][..],
            ),
            (3990, &CompressionLevel::ALL[..]),
        ] {
            for &level in levels {
                let mut p = ArrayPredictor::new(version, level).unwrap();
                for _ in 0..600 {
                    assert_eq!(p.decode(0, 0), 0);
                }
            }
        }
    }

    #[test]
    fn extreme_inputs_never_panic() {
        let mut p = ArrayPredictor::new(3990, CompressionLevel::Insane).unwrap();
        for v in [i32::MAX, i32::MIN, -1, 1, i32::MIN + 1] {
            let _ = p.decode(v, v);
        }
        let mut p = ArrayPredictor::new(3940, CompressionLevel::High).unwrap();
        for v in [i32::MAX, i32::MIN, -1, 1] {
            let _ = p.decode(v, 0);
        }
    }

    #[test]
    fn offset_buffers_wrap_preserves_the_step_stream() {
        // Drive well past the 512-window wrap and check continuity by
        // comparing against a fresh replica fed the same prefix.
        let mut p = OffsetPredictor3950::new();
        let mut outputs = Vec::new();
        for i in 0..1300i32 {
            outputs.push(p.step(i % 23 - 11, i % 7 - 3));
        }
        // The wrap must have happened at least twice.
        assert!(outputs.len() > 2 * OFFSET_WINDOW);
        // Replay: identical trajectory.
        let mut q = OffsetPredictor3950::new();
        for i in 0..1300i32 {
            assert_eq!(q.step(i % 23 - 11, i % 7 - 3), outputs[i as usize]);
        }
    }
}
