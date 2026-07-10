//! Residual entropy codec — the per-value decode the staged
//! `docs/audio/ape/format-reference.md` §2.5–§2.9 pins, over the
//! [`crate::range_coder`] primitives.
//!
//! Two version paths share the running-state recurrence:
//!
//! * **New path** (`file_version >= 3990`, §2.6): an overflow symbol
//!   against the ≥ 3990 frequency model (escape symbol 63 → a full
//!   32-bit overflow in two 16-bit halves), then a **base** coded in
//!   the arbitrary radix `pivot = max(KSum / 32, 1)` (split into a
//!   two-division ladder once the pivot reaches `1 << 16`), and
//!   `value = base + overflow * pivot`.
//! * **Old path** (`file_version < 3990`, §2.7): an overflow symbol
//!   against the < 3990 model, a working bit count `nTempK` (escape
//!   symbol 63 → a 5-bit literal `nTempK` with the overflow reset to
//!   0; otherwise `k - 1`, floored at 0, from the adaptive state), the
//!   `nTempK` low bits (split across the 16-bit ceiling for
//!   `file_version >= 3910` when `nTempK > 16`), and
//!   `value += overflow << nTempK`.
//!
//! Both paths finish with the §2.8 KSum recurrence
//! `KSum += (value + 1) / 2 - ((KSum + 16) >> 5)`; the old path also
//! adapts `k` against the [`K_SUM_MIN_BOUNDARY`] ladder. The decoded
//! magnitude un-zig-zags per §2.9: odd → `(value >> 1) + 1`, even →
//! `-(value >> 1)` ([`unfold_residual`]).
//!
//! ## What is NOT pinned (constructor-injected)
//!
//! The staged reference marks the **per-frame reset of `k` / `KSum`**
//! as a GAP (§4). Both directions therefore take the initial running
//! state as an explicit [`EntropyInit`] value instead of baking in a
//! guess; encode/decode round-trip exactly for *any* shared init. The
//! integer width of the `KSum` accumulator is likewise carried as the
//! natural 32-bit register the rest of the coder uses, wrapping, until
//! a trace pins otherwise.
//!
//! The encoder direction ([`ResidualEncoder`]) is crate-derived (the
//! staged reference describes only the decode); it exists so the
//! decode path can be exercised end-to-end without vendor fixtures,
//! and mirrors every branch of the decode including both escapes.

use crate::error::{Error, Result};
use crate::freq_model::{
    counts_for_version, parse_u32_col, symbol_for_cum_freq, symbol_interval, MODEL_ELEMENTS,
    RANGE_OVERFLOW_SHIFT,
};
use crate::range_coder::{RangeDecoder, RangeEncoder};
use crate::scalars::ksum_pivot;

/// §2.8 adaptive-`k` boundary ladder `K_SUM_MIN_BOUNDARY[32]` — `0`,
/// then `2^(n + 4)` for `n >= 1`, zero-padded at the top. Loaded from
/// `src/tables/ksum_min_boundary.csv` (transcribed from the staged
/// format reference) via the crate's const CSV parser, so no numeric
/// literal is retyped into this source.
pub static K_SUM_MIN_BOUNDARY: [u32; 32] =
    parse_u32_col(include_str!("tables/ksum_min_boundary.csv"));

/// Escape symbol index: the last symbol of the 64-entry model
/// (`MODEL_ELEMENTS - 1`). On the new path it introduces a full 32-bit
/// overflow; on the old path it introduces a 5-bit literal `nTempK`.
pub const ESCAPE_SYMBOL: u32 = MODEL_ELEMENTS as u32 - 1;

/// File-version boundary below which a wide `nTempK` is decoded in one
/// shot rather than as a 16-bit + remainder split (§2.7 step 3).
pub const WIDE_K_SPLIT_VERSION: u16 = 3910;

/// Initial running state of the residual codec for one array. The
/// staged reference marks the per-frame reset of both fields as a GAP
/// (§4), so the caller supplies them; encode/decode round-trip for any
/// shared value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EntropyInit {
    /// Initial adaptive bit-count state `k` (old path only).
    pub k: u32,
    /// Initial `KSum` accumulator (drives the pivot on the new path
    /// and `k` adaptation on the old path).
    pub ksum: u32,
}

/// Shared running state + recurrence (§2.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunningState {
    k: u32,
    ksum: u32,
}

impl RunningState {
    fn new(init: EntropyInit) -> Self {
        RunningState {
            k: init.k.min(31),
            ksum: init.ksum,
        }
    }

    /// §2.8: `KSum += (value + 1) / 2 - ((KSum + 16) >> 5)`, then (old
    /// path) adapt `k` against the boundary ladder. Wrapping 32-bit
    /// arithmetic mirrors the coder's register width.
    fn update(&mut self, value: u32, adapt_k: bool) {
        let gain = u64::from(value).div_ceil(2) as u32;
        let decay = self.ksum.wrapping_add(16) >> 5;
        self.ksum = self.ksum.wrapping_add(gain).wrapping_sub(decay);
        if adapt_k {
            let k = self.k as usize;
            if self.ksum < K_SUM_MIN_BOUNDARY[k] {
                self.k -= 1;
            } else if k + 1 < K_SUM_MIN_BOUNDARY.len()
                && K_SUM_MIN_BOUNDARY[k + 1] != 0
                && self.ksum >= K_SUM_MIN_BOUNDARY[k + 1]
            {
                // The ladder's zero-padded top entries are saturation
                // padding, not real boundaries: `k` parks below them
                // (a KSum at that magnitude is already outside any
                // well-formed stream's trajectory).
                self.k += 1;
            }
        }
    }

    /// §2.7 working bit count for the non-escape old path:
    /// `(k < 1) ? 0 : k - 1`.
    fn temp_k(&self) -> u32 {
        self.k.saturating_sub(1)
    }
}

/// The working bit count is bounded by the 5-bit escape literal.
const MAX_TEMP_K: u32 = 31;

/// §2.9 signed unfold: odd → `(value >> 1) + 1`, even →
/// `-(value >> 1)`. The sign lives in the LSB after the range decode.
#[inline]
pub const fn unfold_residual(value: u32) -> i32 {
    if value & 1 != 0 {
        ((value >> 1) as i32).wrapping_add(1)
    } else {
        ((value >> 1) as i32).wrapping_neg()
    }
}

/// Inverse of [`unfold_residual`]: positive `r` → `2r - 1`, `r <= 0` →
/// `-2r` (crate-derived encoder direction).
#[inline]
pub const fn fold_residual(residual: i32) -> u32 {
    if residual > 0 {
        ((residual as u32) << 1).wrapping_sub(1)
    } else {
        (residual.wrapping_neg() as u32) << 1
    }
}

/// Split geometry for a pivot at or past the 16-bit ceiling (§2.6
/// step 4): `splitFactor = 1 << (bits - 16)` for `bits` the pivot's
/// bit length, radix A = `pivot / splitFactor + 1`, radix B =
/// `splitFactor`.
#[inline]
fn pivot_split(pivot: u32) -> (u32, u32) {
    debug_assert!(pivot >= 1 << 16);
    let bits = 32 - pivot.leading_zeros();
    let split_factor = 1u32 << (bits - 16);
    (pivot / split_factor + 1, split_factor)
}

/// Streaming residual decoder over one range-coded payload.
#[derive(Debug, Clone)]
pub struct ResidualDecoder<'a> {
    rc: RangeDecoder<'a>,
    counts: &'static [u32; MODEL_ELEMENTS + 1],
    new_path: bool,
    wide_k_split: bool,
    state: RunningState,
}

impl<'a> ResidualDecoder<'a> {
    /// Prime a decoder over `data` for `file_version`, with the
    /// caller-supplied running-state init (per-frame reset is a staged
    /// GAP — see [`EntropyInit`]).
    pub fn new(data: &'a [u8], file_version: u16, init: EntropyInit) -> Self {
        ResidualDecoder {
            rc: RangeDecoder::new(data),
            counts: counts_for_version(file_version),
            new_path: file_version >= crate::freq_model::FREQ_MODEL_VERSION_SPLIT,
            wide_k_split: file_version >= WIDE_K_SPLIT_VERSION,
            state: RunningState::new(init),
        }
    }

    /// Re-arm the running state (e.g. at an array boundary) without
    /// disturbing the coder registers. Whether the real format resets
    /// between channel arrays is part of the staged init GAP; exposing
    /// the reset keeps both alternatives expressible.
    pub fn reset_state(&mut self, init: EntropyInit) {
        self.state = RunningState::new(init);
    }

    /// Current running state (diagnostics / trace comparison).
    pub fn running_state(&self) -> EntropyInit {
        EntropyInit {
            k: self.state.k,
            ksum: self.state.ksum,
        }
    }

    /// §2.5 overflow-symbol read: look the scaled cumulative frequency
    /// up against the model, then narrow the interval.
    fn decode_symbol(&mut self) -> Result<u32> {
        let cf = self.rc.decode_culfreq(RANGE_OVERFLOW_SHIFT)?;
        let sym = symbol_for_cum_freq(self.counts, cf);
        let (low, width) = symbol_interval(self.counts, sym)
            .ok_or(Error::CorruptStream("overflow symbol out of model"))?;
        self.rc.consume(low, width);
        Ok(sym as u32)
    }

    /// Decode one folded (unsigned) residual magnitude.
    pub fn decode_value(&mut self) -> Result<u32> {
        if self.new_path {
            self.decode_value_ge3990()
        } else {
            self.decode_value_lt3990()
        }
    }

    /// Decode one signed residual (§2.9 unfold of
    /// [`ResidualDecoder::decode_value`]).
    pub fn decode_residual(&mut self) -> Result<i32> {
        Ok(unfold_residual(self.decode_value()?))
    }

    /// §2.6 new path.
    fn decode_value_ge3990(&mut self) -> Result<u32> {
        let pivot64 = ksum_pivot(u64::from(self.state.ksum));
        // ksum is 32-bit, so pivot = max(ksum / 32, 1) < 2^27.
        let pivot = pivot64 as u32;
        let sym = self.decode_symbol()?;
        let overflow = if sym == ESCAPE_SYMBOL {
            let hi = self.rc.decode_bits(16)?;
            let lo = self.rc.decode_bits(16)?;
            (hi << 16) | lo
        } else {
            sym
        };
        let base = if pivot >= 1 << 16 {
            let (radix_a, split_factor) = pivot_split(pivot);
            let base_a = self.rc.decode_base(radix_a)?;
            let base_b = self.rc.decode_base(split_factor)?;
            base_a.wrapping_mul(split_factor).wrapping_add(base_b)
        } else {
            self.rc.decode_base(pivot)?
        };
        let value = base.wrapping_add(overflow.wrapping_mul(pivot));
        self.state.update(value, false);
        Ok(value)
    }

    /// §2.7 old path.
    fn decode_value_lt3990(&mut self) -> Result<u32> {
        let sym = self.decode_symbol()?;
        let (overflow, temp_k) = if sym == ESCAPE_SYMBOL {
            (0, self.rc.decode_bits(5)?)
        } else {
            (sym, self.state.temp_k())
        };
        if temp_k > MAX_TEMP_K {
            return Err(Error::CorruptStream("working bit count exceeds 31"));
        }
        let low_bits = if temp_k <= 16 || !self.wide_k_split {
            self.rc.decode_bits(temp_k)?
        } else {
            let x1 = self.rc.decode_bits(16)?;
            let x2 = self.rc.decode_bits(temp_k - 16)?;
            x1 | (x2 << 16)
        };
        let value = low_bits.wrapping_add(overflow.wrapping_shl(temp_k));
        self.state.update(value, true);
        Ok(value)
    }

    /// Bit position of the underlying input (frame accounting).
    pub fn bit_pos(&self) -> u64 {
        self.rc.bit_pos()
    }

    /// Zero-filled lookahead reads past the end of the payload.
    pub fn overrun_bytes(&self) -> u32 {
        self.rc.overrun_bytes()
    }
}

/// Crate-derived encoder mirror of [`ResidualDecoder`] (test/fixture
/// harness — the staged reference describes only the decode).
#[derive(Debug, Clone)]
pub struct ResidualEncoder {
    rc: RangeEncoder,
    counts: &'static [u32; MODEL_ELEMENTS + 1],
    new_path: bool,
    wide_k_split: bool,
    state: RunningState,
}

impl ResidualEncoder {
    /// Fresh encoder for `file_version` with the same caller-supplied
    /// running-state init the paired decoder must use.
    pub fn new(file_version: u16, init: EntropyInit) -> Self {
        ResidualEncoder {
            rc: RangeEncoder::new(),
            counts: counts_for_version(file_version),
            new_path: file_version >= crate::freq_model::FREQ_MODEL_VERSION_SPLIT,
            wide_k_split: file_version >= WIDE_K_SPLIT_VERSION,
            state: RunningState::new(init),
        }
    }

    /// Mirror of [`ResidualDecoder::reset_state`].
    pub fn reset_state(&mut self, init: EntropyInit) {
        self.state = RunningState::new(init);
    }

    fn encode_symbol(&mut self, sym: u32) -> Result<()> {
        let (low, width) = symbol_interval(self.counts, sym as usize)
            .ok_or(Error::CorruptStream("symbol out of model"))?;
        self.rc.encode_interval(low, width, RANGE_OVERFLOW_SHIFT);
        Ok(())
    }

    /// Encode one folded (unsigned) residual magnitude.
    pub fn encode_value(&mut self, value: u32) -> Result<()> {
        if self.new_path {
            self.encode_value_ge3990(value)
        } else {
            self.encode_value_lt3990(value)
        }
    }

    /// Encode one signed residual (fold + encode).
    pub fn encode_residual(&mut self, residual: i32) -> Result<()> {
        self.encode_value(fold_residual(residual))
    }

    fn encode_value_ge3990(&mut self, value: u32) -> Result<()> {
        let pivot = ksum_pivot(u64::from(self.state.ksum)) as u32;
        let overflow = value / pivot;
        let base = value % pivot;
        if overflow >= ESCAPE_SYMBOL {
            self.encode_symbol(ESCAPE_SYMBOL)?;
            self.rc.encode_bits(overflow >> 16, 16);
            self.rc.encode_bits(overflow & 0xFFFF, 16);
        } else {
            self.encode_symbol(overflow)?;
        }
        if pivot >= 1 << 16 {
            let (radix_a, split_factor) = pivot_split(pivot);
            self.rc.encode_base(base / split_factor, radix_a);
            self.rc.encode_base(base % split_factor, split_factor);
        } else {
            self.rc.encode_base(base, pivot);
        }
        self.state.update(value, false);
        Ok(())
    }

    fn encode_value_lt3990(&mut self, value: u32) -> Result<()> {
        let temp_k = self.state.temp_k();
        let overflow = if temp_k >= 32 { 0 } else { value >> temp_k };
        if overflow >= ESCAPE_SYMBOL {
            // Escape: a literal bit count wide enough that the whole
            // value fits below it (overflow contribution zero).
            let esc_k = 32 - value.leading_zeros();
            if esc_k > MAX_TEMP_K {
                return Err(Error::CorruptStream(
                    "value too wide for the old-path escape",
                ));
            }
            self.encode_symbol(ESCAPE_SYMBOL)?;
            self.rc.encode_bits(esc_k, 5);
            self.encode_low_bits(value, esc_k);
        } else {
            self.encode_symbol(overflow)?;
            let low_bits = if temp_k == 0 {
                0
            } else {
                value & (u32::MAX >> (32 - temp_k))
            };
            self.encode_low_bits(low_bits, temp_k);
        }
        self.state.update(value, true);
        Ok(())
    }

    fn encode_low_bits(&mut self, low_bits: u32, temp_k: u32) {
        if temp_k <= 16 || !self.wide_k_split {
            self.rc.encode_bits(low_bits, temp_k);
        } else {
            self.rc.encode_bits(low_bits & 0xFFFF, 16);
            self.rc.encode_bits(low_bits >> 16, temp_k - 16);
        }
    }

    /// Flush and return the range-coded payload.
    pub fn finish(self) -> Vec<u8> {
        self.rc.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_ladder_matches_the_staged_closed_form() {
        // 0, then 2^(n+4) for n >= 1, zero-padded at the top.
        assert_eq!(K_SUM_MIN_BOUNDARY.len(), 32);
        assert_eq!(K_SUM_MIN_BOUNDARY[0], 0);
        for (n, &b) in K_SUM_MIN_BOUNDARY.iter().enumerate().take(28).skip(1) {
            assert_eq!(b, 1u32 << (n + 4), "index {n}");
        }
        for (n, &b) in K_SUM_MIN_BOUNDARY.iter().enumerate().skip(28) {
            assert_eq!(b, 0, "padding index {n}");
        }
    }

    #[test]
    fn unfold_matches_the_staged_orientation() {
        // §2.9 worked orientation: odd -> positive, even -> negative.
        assert_eq!(unfold_residual(0), 0);
        assert_eq!(unfold_residual(1), 1);
        assert_eq!(unfold_residual(2), -1);
        assert_eq!(unfold_residual(3), 2);
        assert_eq!(unfold_residual(4), -2);
        assert_eq!(unfold_residual(5), 3);
    }

    #[test]
    fn fold_is_the_exact_inverse_of_unfold() {
        for r in -70000i32..=70000 {
            assert_eq!(unfold_residual(fold_residual(r)), r, "residual {r}");
        }
        // And value-side: every magnitude round-trips.
        for v in 0u32..=200000 {
            assert_eq!(fold_residual(unfold_residual(v)), v, "value {v}");
        }
    }

    #[test]
    fn ksum_recurrence_matches_the_staged_arithmetic() {
        // KSum += (value + 1) / 2 - ((KSum + 16) >> 5), computed
        // against the pre-update KSum.
        let mut st = RunningState::new(EntropyInit { k: 0, ksum: 100 });
        st.update(9, false);
        // gain = 5, decay = (100 + 16) >> 5 = 3.
        assert_eq!(st.ksum, 102);
        st.update(0, false);
        // gain = 0, decay = (102 + 16) >> 5 = 3.
        assert_eq!(st.ksum, 99);
    }

    #[test]
    fn adaptive_k_walks_the_boundary_ladder() {
        let mut st = RunningState::new(EntropyInit { k: 5, ksum: 0 });
        // ksum 0 < boundary[5] = 512 -> k decrements per update.
        st.update(0, true);
        assert_eq!(st.k, 4);
        // Push ksum far above boundary[k + 1] and watch k climb.
        let mut st2 = RunningState::new(EntropyInit { k: 4, ksum: 0 });
        for _ in 0..2000 {
            st2.update(5000, true);
        }
        assert!(st2.k > 4, "k must climb under a large-magnitude run");
        // k never leaves the ladder.
        assert!((st2.k as usize) < K_SUM_MIN_BOUNDARY.len());
    }

    #[test]
    fn k_parks_below_the_zero_padded_ladder_top() {
        // Even with an absurd KSum, k saturates below the padding.
        let mut st = RunningState::new(EntropyInit {
            k: 26,
            ksum: u32::MAX / 2,
        });
        for _ in 0..64 {
            st.update(u32::MAX / 4, true);
        }
        assert!(st.k <= 27, "k = {} escaped past the padded top", st.k);
    }

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// Round-trip `residuals` through the codec pair at `version` with
    /// the given shared init.
    fn round_trip(version: u16, init: EntropyInit, residuals: &[i32]) {
        let mut enc = ResidualEncoder::new(version, init);
        for &r in residuals {
            enc.encode_residual(r).unwrap();
        }
        let bytes = enc.finish();
        let mut dec = ResidualDecoder::new(&bytes, version, init);
        for (i, &r) in residuals.iter().enumerate() {
            assert_eq!(
                dec.decode_residual().unwrap(),
                r,
                "version {version} residual index {i}"
            );
        }
    }

    #[test]
    fn small_residuals_round_trip_across_every_version_boundary() {
        let residuals: Vec<i32> = (-40..=40).chain([0, 1, -1, 63, -63, 64, -64]).collect();
        for version in [3800u16, 3900, 3909, 3910, 3950, 3980, 3989, 3990, 3999] {
            for init in [
                EntropyInit::default(),
                EntropyInit { k: 10, ksum: 16384 },
                EntropyInit { k: 3, ksum: 90 },
            ] {
                round_trip(version, init, &residuals);
            }
        }
    }

    #[test]
    fn audio_shaped_noise_round_trips_on_both_paths() {
        for version in [3920u16, 3990] {
            let mut rng = 0xA0D1_0000_1234_5678u64 ^ u64::from(version);
            let residuals: Vec<i32> = (0..20000)
                .map(|_| (xorshift(&mut rng) as i32) % 32768)
                .collect();
            round_trip(version, EntropyInit { k: 10, ksum: 16384 }, &residuals);
        }
    }

    #[test]
    fn silence_round_trips_and_encodes_compactly() {
        // An all-zero residual run must decode to zeros and the coded
        // payload must be far smaller than the run (the model's symbol
        // 0 has the widest interval).
        for version in [3920u16, 3990] {
            let residuals = vec![0i32; 8192];
            let mut enc = ResidualEncoder::new(version, EntropyInit::default());
            for &r in &residuals {
                enc.encode_residual(r).unwrap();
            }
            let bytes = enc.finish();
            // Symbol 0 spans ~23% (old model) / ~30% (new model) of the
            // cumulative range, i.e. ~2.1 / ~1.7 bits per zero.
            assert!(
                bytes.len() < residuals.len() / 3,
                "version {version}: silence coded to {} bytes",
                bytes.len()
            );
            let mut dec = ResidualDecoder::new(&bytes, version, EntropyInit::default());
            for _ in 0..residuals.len() {
                assert_eq!(dec.decode_residual().unwrap(), 0);
            }
        }
    }

    #[test]
    fn new_path_escape_carries_a_full_32_bit_overflow() {
        // Values whose overflow part exceeds the 62 direct symbols
        // force the §2.6 escape; magnitudes chosen so overflow spans
        // both 16-bit halves.
        let residuals = [1_000_000i32, -1_000_000, 500_000, -250_000, 2_000_000];
        round_trip(3990, EntropyInit::default(), &residuals);
        // With a tiny ksum the pivot floors at 1, making the overflow
        // equal the whole value — the deepest escape.
        round_trip(3990, EntropyInit { k: 0, ksum: 0 }, &residuals);
    }

    #[test]
    fn new_path_pivot_split_engages_past_the_16_bit_ceiling() {
        // Drive ksum high enough that pivot = ksum/32 >= 1<<16 and the
        // two-division base split is exercised.
        let init = EntropyInit {
            k: 0,
            ksum: 64 << 16, // pivot = 2 << 16
        };
        let residuals: Vec<i32> = (0..64).flat_map(|i| [i * 100_000, -i * 99_991]).collect();
        round_trip(3990, init, &residuals);
    }

    #[test]
    fn pivot_split_geometry_matches_the_staged_recipe() {
        // bits = bit length, splitFactor = 1 << (bits - 16),
        // A = pivot / splitFactor + 1, B = splitFactor.
        let (a, b) = pivot_split(1 << 16);
        assert_eq!(b, 2); // bits = 17
        assert_eq!(a, (1 << 16) / 2 + 1);
        let (a, b) = pivot_split(0x0002_5000);
        assert_eq!(b, 1 << 2); // bits = 18
        assert_eq!(a, 0x0002_5000 / 4 + 1);
        // Any base < pivot must be representable as
        // (base / B) in [0, A) and (base % B) in [0, B).
        for pivot in [1u32 << 16, (1 << 16) + 1, 0x0003_FFFF, 1 << 26] {
            let (a, b) = pivot_split(pivot);
            let max_base = pivot - 1;
            assert!(max_base / b < a, "pivot {pivot:#x}");
        }
    }

    #[test]
    fn old_path_escape_resets_overflow_and_reads_a_literal_k() {
        // Large magnitudes at a small adaptive k force the §2.7 escape
        // (overflow would exceed 62).
        let residuals = [100_000i32, -70_000, 65_536, -65_535, 1 << 20];
        for version in [3800u16, 3920, 3989] {
            round_trip(version, EntropyInit { k: 0, ksum: 0 }, &residuals);
        }
    }

    #[test]
    fn old_path_wide_k_split_boundary_at_3910() {
        // A residual run that drives k past 17 so nTempK > 16: below
        // 3910 the low bits are one shot, at/after 3910 they split.
        // Both must round-trip (the doc pins the two spellings decode
        // to the same integer).
        let mut rng = 0x5EED_5EED_5EED_5EEDu64;
        let residuals: Vec<i32> = (0..4000)
            .map(|_| (xorshift(&mut rng) as i32) % (1 << 24))
            .collect();
        let init = EntropyInit {
            k: 20,
            ksum: 1 << 24,
        };
        round_trip(3909, init, &residuals);
        round_trip(3910, init, &residuals);
        round_trip(3980, init, &residuals);
    }

    #[test]
    fn running_state_trajectories_agree_between_directions() {
        // After any shared sequence, encoder and decoder running
        // states must be identical — the property that keeps long
        // arrays in sync.
        let mut rng = 0xD00D_D00D_0000_1111u64;
        let residuals: Vec<i32> = (0..5000)
            .map(|_| (xorshift(&mut rng) as i32) % 20000)
            .collect();
        for version in [3920u16, 3990] {
            let init = EntropyInit { k: 10, ksum: 16384 };
            let mut enc = ResidualEncoder::new(version, init);
            for &r in &residuals {
                enc.encode_residual(r).unwrap();
            }
            let enc_state = EntropyInit {
                k: enc.state.k,
                ksum: enc.state.ksum,
            };
            let bytes = enc.finish();
            let mut dec = ResidualDecoder::new(&bytes, version, init);
            for _ in 0..residuals.len() {
                dec.decode_residual().unwrap();
            }
            assert_eq!(dec.running_state(), enc_state, "version {version}");
        }
    }

    #[test]
    fn reset_state_rearms_the_running_state_mid_stream() {
        // Two arrays over one continuous byte stream, state re-armed at
        // the boundary on both sides — the shape a stereo frame uses.
        let version = 3990u16;
        let init = EntropyInit { k: 10, ksum: 16384 };
        let a: Vec<i32> = (0..500).map(|i| (i % 97) - 48).collect();
        let b: Vec<i32> = (0..500).map(|i| ((i * 7) % 89) - 44).collect();
        let mut enc = ResidualEncoder::new(version, init);
        for &r in &a {
            enc.encode_residual(r).unwrap();
        }
        enc.reset_state(init);
        for &r in &b {
            enc.encode_residual(r).unwrap();
        }
        let bytes = enc.finish();
        let mut dec = ResidualDecoder::new(&bytes, version, init);
        for &r in &a {
            assert_eq!(dec.decode_residual().unwrap(), r);
        }
        dec.reset_state(init);
        for &r in &b {
            assert_eq!(dec.decode_residual().unwrap(), r);
        }
    }

    #[test]
    fn decoder_is_panic_free_on_corrupt_bytes() {
        let mut rng = 0xBAD0_BAD0_BAD0_BAD0u64;
        for version in [3800u16, 3910, 3920, 3990] {
            for _ in 0..32 {
                let bytes: Vec<u8> = (0..64).map(|_| xorshift(&mut rng) as u8).collect();
                let mut dec = ResidualDecoder::new(&bytes, version, EntropyInit::default());
                for _ in 0..512 {
                    // Values may be garbage or CorruptStream; no panics.
                    let _ = dec.decode_residual();
                }
            }
        }
    }

    #[test]
    fn escape_symbol_is_the_models_last_entry() {
        assert_eq!(ESCAPE_SYMBOL, 63);
        assert_eq!(ESCAPE_SYMBOL as usize, MODEL_ELEMENTS - 1);
    }
}
