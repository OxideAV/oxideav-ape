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

/// The documented maximum cascade depth: the staged `filter_config.csv`
/// pins at most three stages for any one compression level (the insane /
/// `5000` profile). Surfaced as a named constant so a fixed-capacity
/// caller can size a backing array without hard-coding `3`.
pub const MAX_CASCADE_DEPTH: usize = 3;

/// A fixed-capacity, no-alloc view of one compression level's adaptive-
/// filter cascade.
///
/// [`cascade_for_level`] allocates a `Vec`; this is the same pinned
/// `(order, shift)` data carried inline so a decode path can read the
/// cascade — and the aggregate quantities it derives from it — without an
/// allocation. It is a pure reshaping of the Extractor-staged
/// `filter_config.csv` data: every method below reads the staged stages
/// or sums their pinned `order` fields. No control-flow narrative is
/// introduced — how the stages are *run* over a residual buffer is
/// downstream work the staged tables do not pin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FilterCascade {
    /// The compression level this cascade belongs to.
    level: CompressionLevel,
    /// The cascade stages in application order, filled `0..len`.
    stages: [FilterStage; MAX_CASCADE_DEPTH],
    /// Number of populated entries in `stages` (`1..=MAX_CASCADE_DEPTH`).
    len: usize,
}

impl FilterCascade {
    /// Build the cascade view for `level` from the staged stage table.
    ///
    /// The stages are gathered in `filter_index` order, exactly as
    /// [`cascade_for_level`] returns them, but into an inline fixed-size
    /// buffer rather than a heap `Vec`. Every documented level populates
    /// between one and [`MAX_CASCADE_DEPTH`] stages.
    pub fn for_level(level: CompressionLevel) -> Self {
        let code = level.as_u16();
        let mut stages = [FilterStage {
            level_code: code,
            filter_index: 0,
            order: 0,
            shift: 0,
        }; MAX_CASCADE_DEPTH];
        let mut len = 0usize;
        // Walk the staged table in filter_index order. The table has at
        // most MAX_CASCADE_DEPTH rows per level, so a single ascending
        // pass over the filter_index keyspace fills `stages` in order
        // without a sort or an allocation.
        for idx in 0..MAX_CASCADE_DEPTH as u8 {
            for s in FILTER_STAGES.iter() {
                if s.level_code == code && s.filter_index == idx {
                    stages[len] = *s;
                    len += 1;
                }
            }
        }
        FilterCascade { level, stages, len }
    }

    /// The compression level this cascade belongs to.
    #[inline]
    pub const fn level(&self) -> CompressionLevel {
        self.level
    }

    /// The cascade stages in application order (`filter_index` ascending).
    #[inline]
    pub fn stages(&self) -> &[FilterStage] {
        &self.stages[..self.len]
    }

    /// Number of stages in the cascade (`1..=MAX_CASCADE_DEPTH`).
    #[inline]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Whether the cascade applies no adaptive filtering — true exactly
    /// when the single stage has `order == 0` (the fast / `1000` level).
    /// The cascade is never empty (every level has at least one staged
    /// row), so this is distinct from a zero-length collection.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.stages().iter().all(|s| s.order == 0)
    }

    /// The stage at application position `index` (`0` runs first), or
    /// `None` if `index >= len`.
    #[inline]
    pub fn stage(&self, index: usize) -> Option<FilterStage> {
        self.stages().get(index).copied()
    }

    /// The sum of every stage's IIR filter `order`.
    ///
    /// This is the total number of prediction taps the cascade reads
    /// across all stages — the quantity a decode path uses to size the
    /// combined history window. Computed by summing the pinned `order`
    /// fields; widened to `usize` so the `1280 + 256 + 16` insane total
    /// cannot overflow the `u16` per-stage field type.
    #[inline]
    pub fn total_order(&self) -> usize {
        self.stages().iter().map(|s| s.order as usize).sum()
    }

    /// The largest single-stage IIR filter `order` in the cascade — the
    /// widest history window any one stage reads. For the insane profile
    /// this is the `1280`-tap lead stage.
    #[inline]
    pub fn max_order(&self) -> u16 {
        self.stages().iter().map(|s| s.order).max().unwrap_or(0)
    }
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
        assert_eq!(MAX_CASCADE_DEPTH, 3);
    }

    #[test]
    fn cascade_view_stages_match_the_vec_form() {
        // The inline FilterCascade view must carry exactly the same
        // stages, in the same application order, as the Vec-returning
        // cascade_for_level — it is the same staged data, reshaped.
        for level in CompressionLevel::ALL {
            let view = FilterCascade::for_level(level);
            assert_eq!(view.level(), level);
            assert_eq!(view.stages(), cascade_for_level(level).as_slice());
            assert_eq!(view.len(), cascade_for_level(level).len());
        }
    }

    #[test]
    fn cascade_view_stage_accessor_indexes_in_application_order() {
        // 4000 -> stage0 (256,13), stage1 (32,10), no stage2.
        let view = FilterCascade::for_level(CompressionLevel::ExtraHigh);
        assert_eq!(view.stage(0).map(|s| (s.order, s.shift)), Some((256, 13)));
        assert_eq!(view.stage(1).map(|s| (s.order, s.shift)), Some((32, 10)));
        assert_eq!(view.stage(2), None);
    }

    #[test]
    fn cascade_view_total_order_sums_every_stage() {
        // fast: order 0 -> total 0. normal: 16. high: 64.
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::Fast).total_order(),
            0
        );
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::Normal).total_order(),
            16
        );
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::High).total_order(),
            64
        );
        // extra high: 256 + 32 = 288.
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::ExtraHigh).total_order(),
            256 + 32
        );
        // insane: 1280 + 256 + 16 = 1552.
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::Insane).total_order(),
            1280 + 256 + 16
        );
    }

    #[test]
    fn cascade_view_max_order_is_the_widest_stage() {
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::Fast).max_order(),
            0
        );
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::ExtraHigh).max_order(),
            256
        );
        // Insane's widest stage is the 1280-tap lead.
        assert_eq!(
            FilterCascade::for_level(CompressionLevel::Insane).max_order(),
            1280
        );
    }

    #[test]
    fn cascade_view_is_empty_only_for_the_no_filter_fast_level() {
        // Fast is the single order-0 stage: no adaptive filtering.
        assert!(FilterCascade::for_level(CompressionLevel::Fast).is_empty());
        // Every other documented level runs at least one real filter.
        for level in [
            CompressionLevel::Normal,
            CompressionLevel::High,
            CompressionLevel::ExtraHigh,
            CompressionLevel::Insane,
        ] {
            let view = FilterCascade::for_level(level);
            assert!(!view.is_empty());
            // The view is never zero-length: a non-filtering level still
            // carries its one order-0 stage row.
            assert!(view.len() >= MAX_CASCADE_DEPTH.min(1));
            assert!(!view.stages().is_empty());
        }
    }

    #[test]
    fn cascade_view_total_order_agrees_with_summing_the_vec() {
        // Cross-check the inline total against an independent sum over the
        // allocating form, for every level.
        for level in CompressionLevel::ALL {
            let independent: usize = cascade_for_level(level)
                .iter()
                .map(|s| s.order as usize)
                .sum();
            assert_eq!(FilterCascade::for_level(level).total_order(), independent);
        }
    }
}
