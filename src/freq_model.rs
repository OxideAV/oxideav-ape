//! Range-coder symbol-frequency model the staged tables pin.
//!
//! The wiki §"Residual Coding"
//! (`docs/audio/ape/wiki/Monkeys_Audio.wiki`) states that each residual
//! is split into a low-bits part and a high-bits part, *"coding each
//! part separately with range coder"*. The high-bits part is range-coded
//! against a fixed **cumulative symbol-frequency table**. The clean-room
//! extractor staged exactly that table — in two version-split variants —
//! under `docs/audio/ape-cleanroom/tables/`:
//!
//! * `counts_le3980.csv` — the model for `file_version < 3990`, and
//! * `counts_ge3990.csv` — the model for `file_version >= 3990`.
//!
//! Both are monotonically increasing `u32` arrays of length 65, running
//! `0 .. 65536`; the 64 successive differences are the per-symbol widths.
//! The extractor *also* staged those per-symbol widths directly, as a
//! second functional-data table transcribed from a different array in the
//! reference (`freqs_le3980.csv` / `freqs_ge3990.csv`). We ship both: the
//! width table is the data the encoder-direction lookup needs without a
//! subtraction, and carrying it independently lets the crate **assert**
//! the two separately-extracted tables agree (`freq[i] ==
//! counts[i + 1] - counts[i]`) — a provenance cross-check the derived
//! form could not provide. The total width `65536 == 1 << 16` and the
//! symbol count `64` are the [`scalars`] bounds the extractor pinned.
//!
//! This module ships the **data + the two interval lookups the table
//! shape itself dictates** — symbol → `[low, width)` interval, and
//! cumulative-frequency → symbol — and nothing more. The range
//! decoder's *renormalisation / byte-input state machine* (how the code
//! value is refilled and how the decoded frequency is mapped back out of
//! the coder's `range`) is **narrative the staged `tables/` do not pin**
//! and the cleanroom `spec/` directory has not yet been authored, so it
//! is deliberately left to a later phase rather than guessed. See the
//! crate README "Out of scope" tail.
//!
//! ## Data provenance (clean-room)
//!
//! The four CSV files under `src/tables/` are byte-for-byte copies of
//! the extractor's functional-data tables in
//! `docs/audio/ape-cleanroom/tables/`. Under *Feist v. Rural* the
//! integer arrays are unprotectable functional facts. The values are
//! loaded with [`include_str!`] + a `const` parser so no numeric literal
//! from the table is retyped into this source.

/// Number of symbols in the residual frequency model (the count table
/// has this many per-symbol widths). From
/// `tables/scalars.csv` (`model_elements`).
pub const MODEL_ELEMENTS: usize = 64;

/// Total cumulative-frequency range — the top of the count table, and
/// the denominator the range coder scales its `range` against. From
/// `tables/scalars.csv` (`range_overflow_total_width`); equals
/// `1 << RANGE_OVERFLOW_SHIFT`.
pub const RANGE_TOTAL_WIDTH: u32 = 1 << RANGE_OVERFLOW_SHIFT;

/// Shift pairing with [`RANGE_TOTAL_WIDTH`] (`2^16 == 65536`). From
/// `tables/scalars.csv` (`range_overflow_shift`).
pub const RANGE_OVERFLOW_SHIFT: u32 = 16;

/// File-version boundary separating the two frequency-model variants.
/// `file_version < 3990` decodes against [`COUNTS_LE3980`];
/// `file_version >= 3990` decodes against [`COUNTS_GE3990`]. The
/// boundary is the one named by the staged table stems
/// (`counts_le3980` / `counts_ge3990`) and recorded in their `.meta`
/// provenance headers.
pub const FREQ_MODEL_VERSION_SPLIT: u16 = 3990;

/// Compile-time CSV column extractor: returns the `[u32; N]` from the
/// second column of a `header\nk,v\n…` CSV, ignoring the header row.
///
/// `const`-evaluated so the table arrays are baked into the binary with
/// no runtime parse and — critically for the clean-room wall — no
/// numeric literal retyped from the source CSV into this file.
const fn parse_u32_col<const N: usize>(csv: &str) -> [u32; N] {
    let bytes = csv.as_bytes();
    let mut out = [0u32; N];
    let mut i = 0usize;
    // Skip the header line.
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i += 1;
    let mut row = 0usize;
    while i < bytes.len() && row < N {
        // Skip the first column up to and including the comma.
        while i < bytes.len() && bytes[i] != b',' {
            i += 1;
        }
        i += 1; // step past the comma
                // Parse the second column up to newline / EOF.
        let mut v: u32 = 0;
        while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'9' {
            v = v * 10 + (bytes[i] - b'0') as u32;
            i += 1;
        }
        out[row] = v;
        row += 1;
        // Advance to the next line.
        while i < bytes.len() && bytes[i] != b'\n' {
            i += 1;
        }
        i += 1;
    }
    out
}

/// Cumulative symbol-frequency table for `file_version < 3990`.
/// Monotonic `0 .. 65536`, length `MODEL_ELEMENTS + 1`. Loaded from
/// `src/tables/counts_le3980.csv`.
pub static COUNTS_LE3980: [u32; MODEL_ELEMENTS + 1] =
    parse_u32_col(include_str!("tables/counts_le3980.csv"));

/// Cumulative symbol-frequency table for `file_version >= 3990`.
/// Monotonic `0 .. 65536`, length `MODEL_ELEMENTS + 1`. Loaded from
/// `src/tables/counts_ge3990.csv`.
pub static COUNTS_GE3990: [u32; MODEL_ELEMENTS + 1] =
    parse_u32_col(include_str!("tables/counts_ge3990.csv"));

/// Per-symbol frequency widths for `file_version < 3990`, length
/// `MODEL_ELEMENTS`. Each entry is the half-open width of the symbol's
/// cumulative-frequency interval; the entries sum to
/// [`RANGE_TOTAL_WIDTH`]. This is the extractor's directly-transcribed
/// width table (`freqs_le3980.csv`), distinct from the cumulative
/// [`COUNTS_LE3980`] — the two are cross-checked for agreement in this
/// module's tests. Loaded from `src/tables/freqs_le3980.csv`.
pub static FREQS_LE3980: [u32; MODEL_ELEMENTS] =
    parse_u32_col(include_str!("tables/freqs_le3980.csv"));

/// Per-symbol frequency widths for `file_version >= 3990`, length
/// `MODEL_ELEMENTS`. Companion width table to the cumulative
/// [`COUNTS_GE3990`]; see [`FREQS_LE3980`]. Loaded from
/// `src/tables/freqs_ge3990.csv`.
pub static FREQS_GE3990: [u32; MODEL_ELEMENTS] =
    parse_u32_col(include_str!("tables/freqs_ge3990.csv"));

/// Bit-reader mask table `value[n] = (2^n) - 1`, `n = 0..=32`
/// (`value[32]` saturates at `u32::MAX`). Loaded from
/// `src/tables/powers_of_two_minus_one.csv`.
pub static POWERS_OF_TWO_MINUS_ONE: [u32; 33] =
    parse_u32_col(include_str!("tables/powers_of_two_minus_one.csv"));

/// Select the cumulative-frequency table for a given `file_version`.
///
/// `< 3990` → [`COUNTS_LE3980`]; `>= 3990` → [`COUNTS_GE3990`]. The
/// boundary is [`FREQ_MODEL_VERSION_SPLIT`].
#[inline]
pub fn counts_for_version(file_version: u16) -> &'static [u32; MODEL_ELEMENTS + 1] {
    if file_version >= FREQ_MODEL_VERSION_SPLIT {
        &COUNTS_GE3990
    } else {
        &COUNTS_LE3980
    }
}

/// Select the per-symbol width table for a given `file_version`.
///
/// `< 3990` → [`FREQS_LE3980`]; `>= 3990` → [`FREQS_GE3990`]. The
/// boundary is [`FREQ_MODEL_VERSION_SPLIT`]; mirrors
/// [`counts_for_version`].
#[inline]
pub fn freqs_for_version(file_version: u16) -> &'static [u32; MODEL_ELEMENTS] {
    if file_version >= FREQ_MODEL_VERSION_SPLIT {
        &FREQS_GE3990
    } else {
        &FREQS_LE3980
    }
}

/// The half-open frequency width of `symbol` in a per-symbol width
/// `freqs` table — `freqs[symbol]`, or `None` if `symbol >=
/// MODEL_ELEMENTS`. This reads the directly-staged width table; it
/// equals the [`symbol_interval`] width derived from the cumulative
/// table (asserted in this module's tests).
#[inline]
pub fn symbol_width(freqs: &[u32; MODEL_ELEMENTS], symbol: usize) -> Option<u32> {
    if symbol >= MODEL_ELEMENTS {
        return None;
    }
    Some(freqs[symbol])
}

/// The `[low, width)` cumulative-frequency interval of `symbol` in a
/// cumulative-frequency `counts` table.
///
/// `low = counts[symbol]` and `width = counts[symbol + 1] - low`. Returns
/// `None` if `symbol >= MODEL_ELEMENTS` (no width slot follows the last
/// cumulative entry). This is the encoder-direction lookup: given the
/// symbol, recover the sub-range the range coder narrows into.
#[inline]
pub fn symbol_interval(counts: &[u32; MODEL_ELEMENTS + 1], symbol: usize) -> Option<(u32, u32)> {
    if symbol >= MODEL_ELEMENTS {
        return None;
    }
    let low = counts[symbol];
    let width = counts[symbol + 1] - low;
    Some((low, width))
}

/// The symbol whose cumulative-frequency interval contains `cum_freq`.
///
/// Finds the unique `s` with `counts[s] <= cum_freq < counts[s + 1]`.
/// This is the decoder-direction lookup: given a scaled code value in
/// `0 .. RANGE_TOTAL_WIDTH`, recover the symbol it decodes to. A
/// `cum_freq >= RANGE_TOTAL_WIDTH` (out of the table's total range)
/// clamps to the last symbol `MODEL_ELEMENTS - 1`.
///
/// The table is monotonic, so this is a binary search; for the 64-entry
/// model the difference against a linear scan is immaterial, but the
/// search keeps the lookup branch-predictable and total-range-correct.
#[inline]
pub fn symbol_for_cum_freq(counts: &[u32; MODEL_ELEMENTS + 1], cum_freq: u32) -> usize {
    // Largest `s` such that counts[s] <= cum_freq, clamped to the model.
    let mut lo = 0usize;
    let mut hi = MODEL_ELEMENTS; // exclusive upper symbol bound
    while lo + 1 < hi {
        let mid = (lo + hi) / 2;
        if counts[mid] <= cum_freq {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_have_documented_shape() {
        assert_eq!(COUNTS_LE3980.len(), MODEL_ELEMENTS + 1);
        assert_eq!(COUNTS_GE3990.len(), MODEL_ELEMENTS + 1);
        assert_eq!(POWERS_OF_TWO_MINUS_ONE.len(), 33);
    }

    #[test]
    fn cumulative_tables_run_zero_to_total_and_are_monotonic() {
        for counts in [&COUNTS_LE3980, &COUNTS_GE3990] {
            assert_eq!(counts[0], 0);
            assert_eq!(counts[MODEL_ELEMENTS], RANGE_TOTAL_WIDTH);
            for w in counts.windows(2) {
                // Strictly non-decreasing; every per-symbol width >= 1.
                assert!(w[1] > w[0], "table must be strictly increasing");
            }
        }
    }

    #[test]
    fn total_width_matches_scalar_bounds() {
        assert_eq!(RANGE_TOTAL_WIDTH, 65536);
        assert_eq!(RANGE_TOTAL_WIDTH, 1 << RANGE_OVERFLOW_SHIFT);
        assert_eq!(MODEL_ELEMENTS, 64);
    }

    #[test]
    fn first_widths_anchor_the_le3980_table() {
        // counts_le3980 begins 0, 14824, 28224 — so symbol 0 occupies
        // [0, 14824) and symbol 1 occupies [14824, 28224 - 14824).
        assert_eq!(symbol_interval(&COUNTS_LE3980, 0), Some((0, 14824)));
        assert_eq!(
            symbol_interval(&COUNTS_LE3980, 1),
            Some((14824, 28224 - 14824))
        );
    }

    #[test]
    fn first_widths_anchor_the_ge3990_table() {
        // counts_ge3990 begins 0, 19578, 36160.
        assert_eq!(symbol_interval(&COUNTS_GE3990, 0), Some((0, 19578)));
        assert_eq!(
            symbol_interval(&COUNTS_GE3990, 1),
            Some((19578, 36160 - 19578))
        );
    }

    #[test]
    fn symbol_interval_rejects_out_of_range_symbol() {
        assert_eq!(symbol_interval(&COUNTS_LE3980, MODEL_ELEMENTS), None);
        assert_eq!(symbol_interval(&COUNTS_LE3980, MODEL_ELEMENTS + 5), None);
    }

    #[test]
    fn version_selector_splits_at_3990() {
        assert_eq!(FREQ_MODEL_VERSION_SPLIT, 3990);
        // The worked-example header version 3920 takes the < 3990 table.
        assert!(std::ptr::eq(counts_for_version(3920), &COUNTS_LE3980));
        assert!(std::ptr::eq(counts_for_version(3980), &COUNTS_LE3980));
        // 3989 is the last version on the older model.
        assert!(std::ptr::eq(counts_for_version(3989), &COUNTS_LE3980));
        // 3990 and up take the newer model.
        assert!(std::ptr::eq(counts_for_version(3990), &COUNTS_GE3990));
        assert!(std::ptr::eq(counts_for_version(3999), &COUNTS_GE3990));
    }

    #[test]
    fn cum_freq_lookup_inverts_symbol_interval() {
        // For every symbol, every cumulative-frequency value inside its
        // half-open interval must decode back to that symbol.
        for counts in [&COUNTS_LE3980, &COUNTS_GE3990] {
            for symbol in 0..MODEL_ELEMENTS {
                let (low, width) = symbol_interval(counts, symbol).unwrap();
                // Low edge decodes to the symbol.
                assert_eq!(symbol_for_cum_freq(counts, low), symbol);
                // High edge (last value still inside) decodes to symbol.
                assert_eq!(symbol_for_cum_freq(counts, low + width - 1), symbol);
            }
        }
    }

    #[test]
    fn cum_freq_lookup_clamps_at_total_width() {
        // A value at or past the table total clamps to the last symbol
        // rather than indexing off the end.
        for counts in [&COUNTS_LE3980, &COUNTS_GE3990] {
            assert_eq!(
                symbol_for_cum_freq(counts, RANGE_TOTAL_WIDTH),
                MODEL_ELEMENTS - 1
            );
            assert_eq!(symbol_for_cum_freq(counts, u32::MAX), MODEL_ELEMENTS - 1);
        }
    }

    #[test]
    fn widths_are_successive_differences_summing_to_total() {
        // Cross-check the derived per-symbol widths sum to the total
        // range — the defining property of the cumulative table.
        for counts in [&COUNTS_LE3980, &COUNTS_GE3990] {
            let mut sum = 0u32;
            for s in 0..MODEL_ELEMENTS {
                sum += symbol_interval(counts, s).unwrap().1;
            }
            assert_eq!(sum, RANGE_TOTAL_WIDTH);
        }
    }

    #[test]
    fn width_tables_have_documented_shape() {
        assert_eq!(FREQS_LE3980.len(), MODEL_ELEMENTS);
        assert_eq!(FREQS_GE3990.len(), MODEL_ELEMENTS);
    }

    #[test]
    fn staged_width_table_matches_cumulative_differences() {
        // The two tables are extracted independently from different
        // arrays in the reference; the width table must equal the
        // successive differences of the cumulative table. This is the
        // provenance cross-check carrying the second copy buys us.
        for (counts, freqs) in [
            (&COUNTS_LE3980, &FREQS_LE3980),
            (&COUNTS_GE3990, &FREQS_GE3990),
        ] {
            for s in 0..MODEL_ELEMENTS {
                assert_eq!(
                    freqs[s],
                    counts[s + 1] - counts[s],
                    "width[{s}] disagrees with cumulative difference"
                );
                // And it must equal the symbol_interval width.
                assert_eq!(symbol_interval(counts, s).unwrap().1, freqs[s]);
                assert_eq!(symbol_width(freqs, s), Some(freqs[s]));
            }
        }
    }

    #[test]
    fn widths_sum_to_total_range() {
        for freqs in [&FREQS_LE3980, &FREQS_GE3990] {
            let sum: u32 = freqs.iter().sum();
            assert_eq!(sum, RANGE_TOTAL_WIDTH);
            // Every symbol has a non-zero width.
            assert!(freqs.iter().all(|&w| w >= 1));
        }
    }

    #[test]
    fn width_selector_splits_at_3990() {
        assert!(std::ptr::eq(freqs_for_version(3920), &FREQS_LE3980));
        assert!(std::ptr::eq(freqs_for_version(3989), &FREQS_LE3980));
        assert!(std::ptr::eq(freqs_for_version(3990), &FREQS_GE3990));
        assert!(std::ptr::eq(freqs_for_version(3999), &FREQS_GE3990));
    }

    #[test]
    fn symbol_width_rejects_out_of_range_symbol() {
        assert_eq!(symbol_width(&FREQS_LE3980, MODEL_ELEMENTS), None);
        assert_eq!(symbol_width(&FREQS_GE3990, MODEL_ELEMENTS + 3), None);
    }

    #[test]
    fn first_widths_anchor_the_width_tables() {
        // freqs_le3980 begins 14824, 13400; freqs_ge3990 begins 19578.
        assert_eq!(symbol_width(&FREQS_LE3980, 0), Some(14824));
        assert_eq!(symbol_width(&FREQS_LE3980, 1), Some(13400));
        assert_eq!(symbol_width(&FREQS_GE3990, 0), Some(19578));
    }

    #[test]
    fn exhaustive_cum_freq_inverse_over_the_full_range() {
        // Every cumulative-frequency value in [0, 65536) must decode —
        // via the binary search — to the unique symbol whose half-open
        // interval contains it. Walk the table linearly alongside so
        // the check is independent of the search implementation.
        for counts in [&COUNTS_LE3980, &COUNTS_GE3990] {
            let mut symbol = 0usize;
            for cf in 0..RANGE_TOTAL_WIDTH {
                while counts[symbol + 1] <= cf {
                    symbol += 1;
                }
                assert_eq!(
                    symbol_for_cum_freq(counts, cf),
                    symbol,
                    "cum_freq {cf} must decode to symbol {symbol}"
                );
            }
            assert_eq!(symbol, MODEL_ELEMENTS - 1);
        }
    }

    #[test]
    fn powers_of_two_minus_one_is_mask_table() {
        for (n, &v) in POWERS_OF_TWO_MINUS_ONE.iter().enumerate() {
            if n < 32 {
                assert_eq!(v, (1u32 << n) - 1, "value[{n}] must be (2^{n})-1");
            } else {
                // value[32] saturates at u32::MAX.
                assert_eq!(v, u32::MAX);
            }
        }
    }
}
