//! Per-compression-level adaptive-filter cascade configuration.
//!
//! The wiki §"General Details" states audio data is packed *"applying
//! 1-3 IIR filters of different order"*. The clean-room extractor staged
//! the exact `(order, shift)` pairs each compression level uses, under
//! `docs/audio/ape-cleanroom/tables/filter_config.csv`. Each row is
//! `(level_code, filter_index, order, shift)`: the cascade for a given
//! level is the set of rows sharing its `level_code`, ordered by
//! `filter_index`. `order == 0` (fast / `1000`) means no adaptive
//! filter runs.
//!
//! From the staged `filter_config.meta`: this is the cascade for the
//! `>= 3950` predictor; the same `(order, shift)` pairs recur in the
//! 3930-3950 predictor for the levels it supports, and the insane
//! filter-A order `1280 == 1024 + 256`.
//!
//! The data is loaded from `src/tables/filter_config.csv` via
//! [`include_str!`] + a `const` parser so no numeric literal from the
//! table is retyped into this source.

use crate::header::CompressionLevel;

/// One stage of the adaptive-filter cascade: an IIR filter of the given
/// `order` whose adaptation is scaled by a right-shift of `shift` bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FilterStage {
    /// The on-wire compression-level code (`1000`..=`5000`) this stage
    /// belongs to.
    pub level_code: u16,
    /// Position of this stage within its level's cascade (`0` is applied
    /// first). Levels run 1-3 stages.
    pub filter_index: u8,
    /// IIR filter order (number of taps). `0` means no adaptive filter.
    pub order: u16,
    /// Adaptation right-shift paired with `order`.
    pub shift: u8,
}

/// Number of rows in the staged cascade table.
const ROWS: usize = 8;

/// Compile-time CSV parser for the 4-column `filter_config.csv`
/// (`level_code,filter_index,order,shift`), skipping the header row. No
/// numeric literal from the source CSV is retyped into this file.
const fn parse_stages(csv: &str) -> [FilterStage; ROWS] {
    let bytes = csv.as_bytes();
    let mut out = [FilterStage {
        level_code: 0,
        filter_index: 0,
        order: 0,
        shift: 0,
    }; ROWS];
    let mut i = 0usize;
    // Skip the header line (tolerating CRLF or LF endings).
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i += 1;
    let mut row = 0usize;
    while i < bytes.len() && row < ROWS {
        let mut field = 0usize;
        let mut vals = [0u32; 4];
        while field < 4 {
            let mut v: u32 = 0;
            while i < bytes.len() && bytes[i] >= b'0' && bytes[i] <= b'9' {
                v = v * 10 + (bytes[i] - b'0') as u32;
                i += 1;
            }
            vals[field] = v;
            field += 1;
            // Step past any run of field separators — comma, carriage
            // return, newline — so the parser is agnostic to CRLF vs LF.
            while i < bytes.len() && (bytes[i] == b',' || bytes[i] == b'\r' || bytes[i] == b'\n') {
                i += 1;
            }
        }
        out[row] = FilterStage {
            level_code: vals[0] as u16,
            filter_index: vals[1] as u8,
            order: vals[2] as u16,
            shift: vals[3] as u8,
        };
        row += 1;
    }
    out
}

/// The full adaptive-filter cascade table, every stage of every
/// documented compression level, loaded from
/// `src/tables/filter_config.csv`.
pub static FILTER_STAGES: [FilterStage; ROWS] =
    parse_stages(include_str!("tables/filter_config.csv"));

/// The cascade stages for `level`, in application order (`filter_index`
/// ascending). The fast (`1000`) level returns a single `order == 0`
/// stage, meaning no adaptive filter runs.
///
/// Returns up to three stages — the documented maximum cascade depth.
pub fn cascade_for_level(level: CompressionLevel) -> Vec<FilterStage> {
    let code = level.as_u16();
    let mut stages: Vec<FilterStage> = FILTER_STAGES
        .iter()
        .copied()
        .filter(|s| s.level_code == code)
        .collect();
    stages.sort_by_key(|s| s.filter_index);
    stages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fast_level_has_a_single_no_filter_stage() {
        let c = cascade_for_level(CompressionLevel::Fast);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].order, 0);
        assert_eq!(c[0].shift, 0);
        assert_eq!(c[0].level_code, 1000);
    }

    #[test]
    fn normal_high_extrahigh_orders_anchor_the_table() {
        // From filter_config.csv: 2000 -> (16,11); 3000 -> (64,11).
        let normal = cascade_for_level(CompressionLevel::Normal);
        assert_eq!(normal.len(), 1);
        assert_eq!((normal[0].order, normal[0].shift), (16, 11));

        let high = cascade_for_level(CompressionLevel::High);
        assert_eq!(high.len(), 1);
        assert_eq!((high[0].order, high[0].shift), (64, 11));
    }

    #[test]
    fn extrahigh_is_a_two_stage_cascade() {
        // 4000 -> stage0 (256,13), stage1 (32,10).
        let c = cascade_for_level(CompressionLevel::ExtraHigh);
        assert_eq!(c.len(), 2);
        assert_eq!((c[0].order, c[0].shift), (256, 13));
        assert_eq!((c[1].order, c[1].shift), (32, 10));
        assert_eq!(c[0].filter_index, 0);
        assert_eq!(c[1].filter_index, 1);
    }

    #[test]
    fn insane_is_a_three_stage_cascade_with_1280_lead() {
        // 5000 -> stage0 (1280,15), stage1 (256,13), stage2 (16,11).
        let c = cascade_for_level(CompressionLevel::Insane);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].order, c[0].shift), (1280, 15));
        assert_eq!((c[1].order, c[1].shift), (256, 13));
        assert_eq!((c[2].order, c[2].shift), (16, 11));
        // Documented decomposition: 1280 == 1024 + 256.
        assert_eq!(c[0].order, 1024 + 256);
    }

    #[test]
    fn every_stage_belongs_to_a_documented_level() {
        for s in FILTER_STAGES.iter() {
            assert!(
                CompressionLevel::try_from(s.level_code).is_ok(),
                "level_code {} must be a documented profile",
                s.level_code
            );
        }
    }

    #[test]
    fn cascade_depth_never_exceeds_three() {
        for level in CompressionLevel::ALL {
            assert!(cascade_for_level(level).len() <= 3);
        }
    }
}
