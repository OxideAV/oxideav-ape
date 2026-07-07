//! Scalar range-coder / predictor constants the staged tables pin, and
//! the closed-form **stage-1 order-1 integer prediction** they commit to.
//!
//! The clean-room extractor staged a `scalars.csv` table of scalar
//! functional constants under `docs/audio/ape-cleanroom/tables/`. Each
//! row is `name,value,role`; the `role` text is a generic algorithm
//! description, the `value` is the functional integer fact (unprotectable
//! under *Feist v. Rural*). The seven rows the extractor pinned:
//!
//! | name                         | value   | what it bounds |
//! |------------------------------|--------:|----------------|
//! | `model_elements`             | `64`    | frequency-model symbol count (see [`crate::freq_model::MODEL_ELEMENTS`]) |
//! | `range_overflow_total_width` | `65536` | total cumulative-frequency range ([`crate::freq_model::RANGE_TOTAL_WIDTH`]) |
//! | `range_overflow_shift`       | `16`    | shift pairing the total width ([`crate::freq_model::RANGE_OVERFLOW_SHIFT`]) |
//! | `ksum_pivot_divisor`         | `32`    | `KSum` pivot divisor for the `>= 3990` value decode |
//! | `stage1_filter_weight`       | `31`    | weight of the order-1 integer prediction stage (`x*weight>>shift`) |
//! | `stage1_filter_shift`        | `5`     | right shift pairing `stage1_filter_weight` |
//! | `predictor_history_seed`     | `317`   | initial adaptation coefficient seeded into the order-1 history slot |
//!
//! This module ships the **data + the two closed forms the scalar `role`
//! text fully spells out** — the stage-1 order-1 integer prediction
//! `x * stage1_filter_weight >> stage1_filter_shift`, and the `>= 3990`
//! `KSum` pivot `max(KSum / ksum_pivot_divisor, 1)` — and nothing more.
//! The cleanroom README lists *"stage-1 integer order-1 predictor +
//! channel decorrelation"* as in-scope-from-the-wiki, and the `role`
//! column states the stage-1 operation verbatim as `x*weight>>shift` and
//! the pivot as `pivot value = max(KSum / this, 1)`. That makes both
//! pinned, stateless closed forms, distinct from the adaptive predictor
//! cascade recurrence (whose `delta[]` history maintenance the wiki
//! declines to pin — see [`crate::predictor`]).
//!
//! [`PREDICTOR_HISTORY_SEED`] is exposed as a named constant because the
//! extractor pinned it as functional data, but the recurrence it feeds
//! (the per-version adaptation-window seeding) is **narrative the staged
//! `tables/` do not commit to** and the cleanroom `spec/` directory has
//! not been authored. Likewise, the surrounding `k`-parameter recurrence
//! — how `KSum` itself accumulates across decoded values, and how the
//! pivot splits a value into range-coded parts — is unpinned: only the
//! pivot's own closed form ([`ksum_pivot`]) is committed to by the
//! `role` text, so only that map is wired here.
//!
//! ## Data provenance (clean-room)
//!
//! `src/tables/scalars.csv` is a byte-for-byte copy of the extractor's
//! `docs/audio/ape-cleanroom/tables/scalars.csv`. The values are loaded
//! with [`include_str!`] + a `const` parser so no numeric literal from
//! the table is retyped into this source.

/// Compile-time extractor for the numeric `value` column (column index
/// `1`) of the `scalars.csv` `name,value,role` table.
///
/// `name` carries no embedded separators and `value` is always a plain
/// unsigned integer, so the parser walks to the first comma, then reads
/// the digit run that follows. (The `role` column may itself be quoted
/// and contain commas, but it is never read here.) Matching on the row's
/// `name` keeps the lookup independent of row order. `const`-evaluated so
/// the constant is baked in with no runtime parse and no numeric literal
/// retyped from the CSV into this file.
const fn parse_named_value(csv: &str, name: &str) -> u32 {
    let bytes = csv.as_bytes();
    let key = name.as_bytes();
    let mut i = 0usize;
    // Skip the header line.
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i += 1;
    while i < bytes.len() {
        // Row start: try to match `name` against the first column.
        let row_start = i;
        let mut k = 0usize;
        let mut matched = true;
        while k < key.len() {
            if row_start + k >= bytes.len() || bytes[row_start + k] != key[k] {
                matched = false;
                break;
            }
            k += 1;
        }
        // A full key match must be immediately followed by the column
        // comma, so `model_elements` does not match a longer name with
        // that prefix.
        if matched && row_start + k < bytes.len() && bytes[row_start + k] == b',' {
            // Advance to the comma, step past it, read the digit run.
            i = row_start + k + 1;
            let mut v: u32 = 0;
            while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'9' {
                v = v * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            return v;
        }
        // Not this row: advance to the next line.
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i += 1;
    }
    // The table is fixed and every key below is present, so this is
    // unreachable for the shipped CSV; a const-fn cannot panic on stable
    // here, so return a sentinel that the unit tests assert is never hit.
    u32::MAX
}

/// Raw scalars CSV, byte-for-byte from the extractor table.
const SCALARS_CSV: &str = include_str!("tables/scalars.csv");

/// Frequency-model symbol count (`scalars.csv` `model_elements`). Equal
/// to [`crate::freq_model::MODEL_ELEMENTS`]; sourced here from the scalar
/// table for an independent cross-check.
pub const MODEL_ELEMENTS: u32 = parse_named_value(SCALARS_CSV, "model_elements");

/// Total cumulative-frequency range (`scalars.csv`
/// `range_overflow_total_width`). Equal to
/// [`crate::freq_model::RANGE_TOTAL_WIDTH`].
pub const RANGE_OVERFLOW_TOTAL_WIDTH: u32 =
    parse_named_value(SCALARS_CSV, "range_overflow_total_width");

/// Shift pairing the overflow total width (`scalars.csv`
/// `range_overflow_shift`). Equal to
/// [`crate::freq_model::RANGE_OVERFLOW_SHIFT`].
pub const RANGE_OVERFLOW_SHIFT: u32 = parse_named_value(SCALARS_CSV, "range_overflow_shift");

/// `KSum` pivot divisor for the `>= 3990` value decode (`scalars.csv`
/// `ksum_pivot_divisor`). The `role` text pins the pivot's closed form —
/// `pivot value = max(KSum / this, 1)` — which is wired as
/// [`ksum_pivot`]. The surrounding recurrence (how `KSum` accumulates
/// across decoded values, and how the pivot splits a value into
/// range-coded parts) is **not** pinned by the staged tables and remains
/// a later-phase input.
pub const KSUM_PIVOT_DIVISOR: u32 = parse_named_value(SCALARS_CSV, "ksum_pivot_divisor");

/// Fixed weight of the order-1 integer prediction stage
/// (`scalars.csv` `stage1_filter_weight`). The stage computes
/// `x * STAGE1_FILTER_WEIGHT >> STAGE1_FILTER_SHIFT` — see
/// [`stage1_predict`].
pub const STAGE1_FILTER_WEIGHT: i32 = parse_named_value(SCALARS_CSV, "stage1_filter_weight") as i32;

/// Right shift pairing [`STAGE1_FILTER_WEIGHT`] (`scalars.csv`
/// `stage1_filter_shift`).
pub const STAGE1_FILTER_SHIFT: u32 = parse_named_value(SCALARS_CSV, "stage1_filter_shift");

/// Initial adaptation coefficient seeded into the order-1 history slot
/// (`scalars.csv` `predictor_history_seed`). The per-version seeding /
/// adaptation recurrence this constant feeds is **not** pinned by the
/// staged tables, so it is exposed for a later phase but no logic is
/// wired around it here.
pub const PREDICTOR_HISTORY_SEED: i32 =
    parse_named_value(SCALARS_CSV, "predictor_history_seed") as i32;

/// The stage-1 order-1 integer prediction the scalar `role` text spells
/// out verbatim: `x * STAGE1_FILTER_WEIGHT >> STAGE1_FILTER_SHIFT`
/// (`x * 31 >> 5`).
///
/// This is the fixed-weight order-1 stage that runs ahead of the adaptive
/// cascade — a stateless closed form, with no per-version branching and
/// no `delta[]` history. The shift is an **arithmetic** right shift
/// (rounds toward `-∞`), matching the integer-prediction semantics of the
/// rest of the codec's fixed-point arithmetic; the multiply is widened to
/// `i64` so a full-`i32` input cannot overflow the intermediate before
/// the shift narrows it back.
///
/// ```
/// use oxideav_ape::scalars::{stage1_predict, STAGE1_FILTER_WEIGHT, STAGE1_FILTER_SHIFT};
///
/// // x * 31 >> 5.
/// assert_eq!(stage1_predict(64), (64 * STAGE1_FILTER_WEIGHT) >> STAGE1_FILTER_SHIFT);
/// assert_eq!(stage1_predict(64), 62); // 64*31 = 1984; 1984 >> 5 = 62.
/// assert_eq!(stage1_predict(0), 0);
/// ```
#[inline]
pub const fn stage1_predict(x: i32) -> i32 {
    // Widen before multiplying so `x == i32::MIN` * 31 does not overflow
    // the multiply; the arithmetic right shift then narrows back into the
    // i32 sample range the stage feeds.
    ((x as i64 * STAGE1_FILTER_WEIGHT as i64) >> STAGE1_FILTER_SHIFT) as i32
}

/// The `>= 3990` value-decode `KSum` pivot the scalar `role` text spells
/// out verbatim: `pivot value = max(KSum / KSUM_PIVOT_DIVISOR, 1)`
/// (`max(ksum / 32, 1)`).
///
/// This is the second (and last) closed form the `scalars.csv` `role`
/// column commits to. It is a stateless map from the running `KSum`
/// accumulator to the pivot the `>= 3990` decode splits values against;
/// the *recurrence* that maintains `KSum` across decoded values — and
/// the split/reassembly the pivot drives — are narrative the staged
/// tables do not pin, so those stay out of scope until the cleanroom
/// `spec/` is authored. The floor at `1` means the pivot can never be
/// zero, so a later phase may divide by it unconditionally.
///
/// `KSum` is carried as `u64` so a caller-side accumulator over long
/// residual runs cannot saturate the argument type; the divisor is the
/// pinned [`KSUM_PIVOT_DIVISOR`] constant loaded from the table.
///
/// ```
/// use oxideav_ape::scalars::{ksum_pivot, KSUM_PIVOT_DIVISOR};
///
/// // max(ksum / 32, 1).
/// assert_eq!(ksum_pivot(0), 1); // floored at 1
/// assert_eq!(ksum_pivot(31), 1); // 31/32 == 0 -> floored at 1
/// assert_eq!(ksum_pivot(32), 1); // 32/32 == 1
/// assert_eq!(ksum_pivot(96), 3);
/// assert_eq!(ksum_pivot(u64::from(KSUM_PIVOT_DIVISOR) * 10), 10);
/// ```
#[inline]
pub const fn ksum_pivot(ksum: u64) -> u64 {
    let quotient = ksum / KSUM_PIVOT_DIVISOR as u64;
    if quotient == 0 {
        1
    } else {
        quotient
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_lookup_returned_the_unreachable_sentinel() {
        // Every constant below was parsed by name; if any name were
        // missing from the CSV the parser returns u32::MAX. None of the
        // documented scalars is u32::MAX, so a sentinel here means a key
        // typo or a CSV row removal.
        assert_ne!(MODEL_ELEMENTS, u32::MAX);
        assert_ne!(RANGE_OVERFLOW_TOTAL_WIDTH, u32::MAX);
        assert_ne!(RANGE_OVERFLOW_SHIFT, u32::MAX);
        assert_ne!(KSUM_PIVOT_DIVISOR, u32::MAX);
        assert_ne!(STAGE1_FILTER_WEIGHT, u32::MAX as i32);
        assert_ne!(STAGE1_FILTER_SHIFT, u32::MAX);
        assert_ne!(PREDICTOR_HISTORY_SEED, u32::MAX as i32);
    }

    #[test]
    fn scalar_values_match_the_documented_table() {
        assert_eq!(MODEL_ELEMENTS, 64);
        assert_eq!(RANGE_OVERFLOW_TOTAL_WIDTH, 65536);
        assert_eq!(RANGE_OVERFLOW_SHIFT, 16);
        assert_eq!(KSUM_PIVOT_DIVISOR, 32);
        assert_eq!(STAGE1_FILTER_WEIGHT, 31);
        assert_eq!(STAGE1_FILTER_SHIFT, 5);
        assert_eq!(PREDICTOR_HISTORY_SEED, 317);
    }

    #[test]
    fn parse_is_independent_of_row_order() {
        // `model_elements` is the first data row and `predictor_history_seed`
        // the last; both resolve, proving the row-walk handles either end.
        assert_eq!(parse_named_value(SCALARS_CSV, "model_elements"), 64);
        assert_eq!(
            parse_named_value(SCALARS_CSV, "predictor_history_seed"),
            317
        );
    }

    #[test]
    fn parse_does_not_prefix_match_a_longer_name() {
        // The key must be followed by the column comma, so a key that is
        // a strict prefix of a real row name must NOT match that row. No
        // such prefix pair exists in the shipped table, so a fabricated
        // prefix key resolves to the unreachable sentinel.
        assert_eq!(parse_named_value(SCALARS_CSV, "model_element"), u32::MAX);
        assert_eq!(parse_named_value(SCALARS_CSV, "stage1_filter"), u32::MAX);
    }

    #[test]
    fn scalars_agree_with_freq_model_module() {
        // The three shared bounds must agree across the two modules that
        // independently sourced them (freq_model derives from the counts
        // table shape; this module from the scalar table).
        assert_eq!(MODEL_ELEMENTS as usize, crate::freq_model::MODEL_ELEMENTS);
        assert_eq!(
            RANGE_OVERFLOW_TOTAL_WIDTH,
            crate::freq_model::RANGE_TOTAL_WIDTH
        );
        assert_eq!(
            RANGE_OVERFLOW_SHIFT,
            crate::freq_model::RANGE_OVERFLOW_SHIFT
        );
    }

    #[test]
    fn stage1_predict_is_weight_times_then_shift() {
        // x * 31 >> 5 for a handful of anchors.
        assert_eq!(stage1_predict(0), 0);
        assert_eq!(stage1_predict(32), (32 * 31) >> 5); // 992 >> 5 = 31.
        assert_eq!(stage1_predict(32), 31);
        assert_eq!(stage1_predict(64), 62); // 1984 >> 5 = 62.
        assert_eq!(stage1_predict(1), 0); // 31 >> 5 = 0.
                                          // Exact agreement with the spelled-out closed form.
        for x in [-1000i32, -33, -1, 0, 1, 7, 31, 32, 1000, 65535] {
            let expected = (i64::from(x) * 31) >> 5;
            assert_eq!(i64::from(stage1_predict(x)), expected);
        }
    }

    #[test]
    fn stage1_predict_uses_arithmetic_shift_rounding_toward_neg_inf() {
        // An arithmetic right shift rounds toward -inf, not toward zero.
        // x = -1: -31 >> 5 = -1 (floor), whereas -31 / 32 (toward zero)
        // would be 0. Anchor the floor semantics explicitly.
        assert_eq!(stage1_predict(-1), -1);
        // x = -2: -62 >> 5 = -2 (floor); toward-zero division gives -1.
        assert_eq!(stage1_predict(-2), -2);
    }

    #[test]
    fn stage1_predict_does_not_overflow_on_i32_extremes() {
        // i32::MIN * 31 overflows i32 but not the i64 intermediate; the
        // const fn must produce a value without panicking in debug.
        let _ = stage1_predict(i32::MIN);
        let _ = stage1_predict(i32::MAX);
        // i32::MAX * 31 >> 5 stays in range: (2147483647*31)>>5.
        assert_eq!(
            i64::from(stage1_predict(i32::MAX)),
            (i64::from(i32::MAX) * 31) >> 5
        );
    }

    #[test]
    fn stage1_predict_is_const_evaluable() {
        const P: i32 = stage1_predict(64);
        assert_eq!(P, 62);
    }

    #[test]
    fn ksum_pivot_matches_the_spelled_out_closed_form() {
        // max(ksum / 32, 1) — exact agreement with the role text's map
        // over a boundary-heavy sweep.
        for ksum in [
            0u64,
            1,
            31,
            32,
            33,
            63,
            64,
            65,
            96,
            1 << 16,
            u64::from(u32::MAX),
            u64::MAX,
        ] {
            let expected = core::cmp::max(ksum / u64::from(KSUM_PIVOT_DIVISOR), 1);
            assert_eq!(ksum_pivot(ksum), expected, "ksum = {ksum}");
        }
    }

    #[test]
    fn ksum_pivot_floors_at_one_below_the_divisor() {
        // Every KSum below the divisor floors at 1 — the property that
        // lets a later phase divide by the pivot unconditionally.
        for ksum in 0..u64::from(KSUM_PIVOT_DIVISOR) {
            assert_eq!(ksum_pivot(ksum), 1);
        }
        // The first non-floored value is exactly the divisor itself.
        assert_eq!(ksum_pivot(u64::from(KSUM_PIVOT_DIVISOR)), 1);
        assert_eq!(ksum_pivot(u64::from(KSUM_PIVOT_DIVISOR) * 2), 2);
    }

    #[test]
    fn ksum_pivot_is_never_zero() {
        for ksum in [0u64, 1, 31, 32, 1000, u64::MAX] {
            assert!(ksum_pivot(ksum) >= 1);
        }
    }

    #[test]
    fn ksum_pivot_is_monotonic_nondecreasing() {
        // A larger accumulated KSum can never yield a smaller pivot.
        let mut prev = ksum_pivot(0);
        for ksum in 1..4096u64 {
            let cur = ksum_pivot(ksum);
            assert!(cur >= prev, "pivot regressed at ksum = {ksum}");
            prev = cur;
        }
    }

    #[test]
    fn ksum_pivot_is_const_evaluable() {
        const P: u64 = ksum_pivot(96);
        assert_eq!(P, 3);
    }
}
