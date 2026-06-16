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
//! This module ships the **data + the one closed form the scalar `role`
//! text fully spells out** — the stage-1 order-1 integer prediction
//! `x * stage1_filter_weight >> stage1_filter_shift` — and nothing more.
//! The cleanroom README lists *"stage-1 integer order-1 predictor +
//! channel decorrelation"* as in-scope-from-the-wiki, and the `role`
//! column states the operation verbatim as `x*weight>>shift`. That makes
//! the stage a pinned, stateless closed form, distinct from the adaptive
//! predictor cascade recurrence (whose `delta[]` history maintenance the
//! wiki declines to pin — see [`crate::predictor`]).
//!
//! [`KSUM_PIVOT_DIVISOR`] and [`PREDICTOR_HISTORY_SEED`] are exposed as
//! named constants because the extractor pinned them as functional data,
//! but the recurrences they feed (the `>= 3990` `k`-parameter value
//! decode and the per-version adaptation-window seeding) are **narrative
//! the staged `tables/` do not commit to** and the cleanroom `spec/`
//! directory has not been authored. This module therefore wires **no
//! logic** around those two — it surfaces the constants for a later phase
//! and refuses to guess the control flow.
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
/// `ksum_pivot_divisor`). The recurrence this divisor feeds — the
/// per-value `k`-parameter computation — is **not** pinned by the staged
/// tables, so the constant is exposed for a later phase but no logic is
/// wired around it here.
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
}
