//! Per-stream configuration assembled from the parsed header prefix —
//! the version/level dispatch the staged docs pin.
//!
//! Two dispatch axes are committed to by the staged material:
//!
//! * the **frequency model** splits on the file version — `< 3990`
//!   uses one staged cumulative table, `>= 3990` the other
//!   ([`crate::freq_model::FREQ_MODEL_VERSION_SPLIT`]);
//! * the **filter cascade** is selected by the header's compression
//!   level (the extractor's `filter_config.csv` rows,
//!   [`crate::filter_config::FilterCascade`]).
//!
//! [`StreamConfig`] performs both selections once, from one parsed
//! [`HeaderPrefix`], so downstream stages read a single bundled view
//! instead of re-dispatching per frame. It adds no new constants and
//! no new control flow beyond the two pinned selectors it composes.

use crate::filter_config::FilterCascade;
use crate::freq_model::{
    counts_for_version, freqs_for_version, FREQ_MODEL_VERSION_SPLIT, MODEL_ELEMENTS,
};
use crate::header::HeaderPrefix;

/// Everything the pinned tables let a decoder select up-front from the
/// 8-byte header prefix: the version-split frequency-model pair and
/// the per-level filter cascade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamConfig {
    /// The parsed header prefix the selections were made from.
    pub header: HeaderPrefix,
    counts: &'static [u32; MODEL_ELEMENTS + 1],
    freqs: &'static [u32; MODEL_ELEMENTS],
    cascade: FilterCascade,
}

impl StreamConfig {
    /// Assemble the per-stream configuration from a parsed header
    /// prefix, applying the two pinned dispatches (frequency model by
    /// `version_raw`, cascade by compression level).
    pub fn from_header(header: HeaderPrefix) -> Self {
        StreamConfig {
            counts: counts_for_version(header.version_raw),
            freqs: freqs_for_version(header.version_raw),
            cascade: FilterCascade::for_level(header.compression_level),
            header,
        }
    }

    /// Parse the 8-byte prefix out of `bytes` and assemble the
    /// configuration in one step. Surfaces the header parser's errors
    /// unchanged.
    pub fn from_bytes(bytes: &[u8]) -> crate::error::Result<Self> {
        Ok(Self::from_header(HeaderPrefix::parse(bytes)?))
    }

    /// The cumulative symbol-frequency table this stream's residuals
    /// are range-coded against (version-selected).
    pub fn counts(&self) -> &'static [u32; MODEL_ELEMENTS + 1] {
        self.counts
    }

    /// The per-symbol width table matching [`Self::counts`]
    /// (version-selected).
    pub fn freqs(&self) -> &'static [u32; MODEL_ELEMENTS] {
        self.freqs
    }

    /// The pinned adaptive-filter cascade for this stream's
    /// compression level.
    pub fn cascade(&self) -> &FilterCascade {
        &self.cascade
    }

    /// Whether the stream's version selects the `>= 3990` frequency
    /// model (the newer of the two staged variants).
    pub fn uses_ge3990_model(&self) -> bool {
        self.header.version_raw >= FREQ_MODEL_VERSION_SPLIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cascade::StageState;
    use crate::freq_model::{COUNTS_GE3990, COUNTS_LE3980, FREQS_GE3990, FREQS_LE3980};
    use crate::header::CompressionLevel;

    #[test]
    fn worked_example_selects_older_model_and_normal_cascade() {
        // 'MAC ' + 3920 + normal — the wiki's worked example. 3920 <
        // 3990 selects the older frequency model; normal selects the
        // single order-16 stage.
        let cfg = StreamConfig::from_bytes(b"MAC \x50\x0F\xD0\x07").unwrap();
        assert!(std::ptr::eq(cfg.counts(), &COUNTS_LE3980));
        assert!(std::ptr::eq(cfg.freqs(), &FREQS_LE3980));
        assert!(!cfg.uses_ge3990_model());
        assert_eq!(cfg.header.compression_level, CompressionLevel::Normal);
        assert_eq!(cfg.cascade().len(), 1);
        assert_eq!(cfg.cascade().stage(0).unwrap().order, 16);
    }

    #[test]
    fn version_split_dispatches_at_exactly_3990() {
        let below = StreamConfig::from_header(HeaderPrefix::new(3989, CompressionLevel::Fast));
        let at = StreamConfig::from_header(HeaderPrefix::new(3990, CompressionLevel::Fast));
        assert!(std::ptr::eq(below.counts(), &COUNTS_LE3980));
        assert!(std::ptr::eq(at.counts(), &COUNTS_GE3990));
        assert!(std::ptr::eq(below.freqs(), &FREQS_LE3980));
        assert!(std::ptr::eq(at.freqs(), &FREQS_GE3990));
        assert!(!below.uses_ge3990_model());
        assert!(at.uses_ge3990_model());
    }

    #[test]
    fn every_documented_level_yields_its_pinned_cascade() {
        for level in CompressionLevel::ALL {
            let cfg = StreamConfig::from_header(HeaderPrefix::new(3990, level));
            assert_eq!(cfg.cascade(), &FilterCascade::for_level(level));
            // The config's cascade seeds runnable stage states.
            let states = StageState::for_cascade(cfg.cascade());
            assert_eq!(states.len(), cfg.cascade().len());
        }
    }

    #[test]
    fn parse_errors_pass_through_unchanged() {
        use crate::error::Error;
        assert_eq!(
            StreamConfig::from_bytes(b"MAC \x50\x0F").unwrap_err(),
            Error::Truncated
        );
        assert_eq!(
            StreamConfig::from_bytes(b"OggSxxxx").unwrap_err(),
            Error::InvalidMagic
        );
        assert_eq!(
            StreamConfig::from_bytes(b"MAC \x50\x0F\x39\x30").unwrap_err(),
            Error::UnknownCompressionLevel(u16::from_le_bytes([0x39, 0x30]))
        );
    }

    #[test]
    fn counts_and_freqs_selections_stay_paired() {
        // The two tables must always come from the same version side.
        for version in [0u16, 3920, 3989, 3990, 3999, u16::MAX] {
            let cfg =
                StreamConfig::from_header(HeaderPrefix::new(version, CompressionLevel::Insane));
            let same_side = (std::ptr::eq(cfg.counts(), &COUNTS_LE3980)
                && std::ptr::eq(cfg.freqs(), &FREQS_LE3980))
                || (std::ptr::eq(cfg.counts(), &COUNTS_GE3990)
                    && std::ptr::eq(cfg.freqs(), &FREQS_GE3990));
            assert!(same_side, "version {version}");
        }
    }
}
