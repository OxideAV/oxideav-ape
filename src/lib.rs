//! # oxideav-ape
//!
//! **Status:** clean-room build-out — every layer the staged docs pin
//! is implemented, including (since the `format-reference.md` staging)
//! the complete range decoder, the full per-version header/tail
//! extraction, and a vendor frame layer validated **bit-exact against
//! reference-binary-encoded files**. Real `.ape` files parse and their
//! residual arrays decode with full-payload coder consumption; silent
//! frames decode to exact PCM with verified CRCs. Non-silent PCM
//! awaits the predictor narrative (per-version `delta[]` rule, `shift`
//! position, decorrelation orientation).
//!
//! Pure-Rust scaffold for **Monkey's Audio** (`.ape`), the lossless
//! audio codec authored by Matthew T. Ashland and distributed as the
//! reference binary at <http://www.monkeysaudio.com/>. The codec
//! pairs channel decorrelation, a cascade of IIR predictors, and a
//! range-coded residual into a lossless integer-PCM round-trip.
//!
//! Phase 1 ships **only** the 8-byte file-header prefix the staged
//! docs at `docs/audio/ape/wiki/Monkeys_Audio.wiki` pin:
//!
//! - 4-byte `'MAC '` ASCII magic ([`header::MAGIC`]),
//! - 2-byte little-endian `version` field (worked example: `3920` =
//!   v3.92), and
//! - 2-byte little-endian `compression_level` field with the
//!   documented named profiles ([`header::CompressionLevel::Fast`] /
//!   [`Normal`][`header::CompressionLevel::Normal`] /
//!   [`High`][`header::CompressionLevel::High`] /
//!   [`ExtraHigh`][`header::CompressionLevel::ExtraHigh`] /
//!   [`Insane`][`header::CompressionLevel::Insane`]).
//!
//! Phase 1 also exposes the stereo-channel decorrelation
//! reconstructor the wiki §"Channel Correlation" pins
//! ([`decorrelate::reconstruct_pair`] +
//! [`decorrelate::reconstruct_pair_arith_shift`] +
//! [`decorrelate::reconstruct_block`] +
//! [`decorrelate::reconstruct_block_arith_shift`] +
//! [`decorrelate::decorrelate_pair`]). The closed-form recipe
//! `R = X - Y/2`, `L = R + Y` is the **only** algebra the staged
//! docs commit to for the channel-decorrelation layer, so we ship it
//! as a standalone primitive that a future per-version pipeline can
//! plug in unchanged.
//!
//! Phase 1 also exposes the adaptive IIR-predictor **per-value step**
//! the wiki §"IIR Filtering" pins ([`predictor::predict_step`] +
//! [`predictor::predict_step_self_ref`] + [`predictor::predict_dot`] +
//! [`predictor::adapt_sign`]). The snapshot fixes the prediction dot
//! product, the sign-of-input adaptation of the coefficient vector, and
//! `out = in + t`; it explicitly declines to pin the trailing
//! "correct delta[] array - different for many versions" history
//! maintenance, so the step primitive leaves the history window to the
//! caller rather than guessing the unpinned per-version ring update.
//!
//! Phase 2 adds the **range-coder residual frequency model** and the
//! **per-level adaptive-filter cascade configuration** the clean-room
//! tables under `docs/audio/ape-cleanroom/tables/` pin as functional
//! data ([`freq_model`] + [`filter_config`]). The frequency model ships
//! the two version-split cumulative-frequency tables (`< 3990` vs
//! `>= 3990`), the symbol ↔ cumulative-frequency interval lookups the
//! table shape dictates, and the documented scalar bounds
//! (`MODEL_ELEMENTS = 64`, `RANGE_TOTAL_WIDTH = 65536`). It also ships
//! the extractor's **independently-transcribed per-symbol width tables**
//! ([`freq_model::FREQS_LE3980`] / [`freq_model::FREQS_GE3990`] +
//! [`freq_model::freqs_for_version`] + [`freq_model::symbol_width`]),
//! cross-checked at test time against the cumulative tables' successive
//! differences — a provenance guarantee the derived widths could not
//! give. The filter
//! config ships the `(order, shift)` cascade — fast `1000` runs no
//! adaptive filter, insane `5000` runs a three-stage `1280/256/16`
//! cascade.
//!
//! Phase 2 also exposes the **scalar range-coder / predictor constants**
//! the extractor pinned in `tables/scalars.csv` ([`scalars`]) and the two
//! closed forms the scalar `role` text spells out verbatim — the
//! **stage-1 order-1 integer prediction** `x * 31 >> 5`
//! ([`scalars::stage1_predict`]) and the `>= 3990` value-decode **`KSum`
//! pivot** `max(ksum / 32, 1)` ([`scalars::ksum_pivot`]). Both are
//! stateless closed forms distinct from the adaptive cascade recurrence.
//! The `predictor_history_seed` (`317`) scalar is surfaced as a named
//! constant for a later phase, and the recurrences *around* the pivot
//! (how `KSum` accumulates across decoded values; how the pivot splits a
//! value into range-coded parts) are narrative the staged tables do not
//! pin, so no logic is wired around those.
//!
//! Phase 3 assembles the pinned pieces into the decode pipeline shape:
//! the [`cascade`] module walks the pinned per-value recurrence over
//! whole buffers and chains the 1-3 pinned per-level stages (with the
//! unpinned per-version `delta[]` maintenance **injected** as a
//! `policy(residual, filtered)` closure, so encode/decode round-trips
//! exactly for any policy), and the [`pipeline`] module wires the wiki
//! §"General Decoding Process" stage ordering verbatim behind a
//! [`pipeline::DeltaSource`] / [`pipeline::DeltaSink`] entropy
//! boundary. The pipeline round-trips PCM end-to-end in
//! self-consistency across all five pinned level cascades.
//!
//! Phase 4 lands the real-file layers the staged
//! `docs/audio/ape/format-reference.md` pins: the carryless range
//! decoder ([`range_coder`], §2.1–§2.4, with a crate-derived encoder
//! mirror), the per-residual entropy codec for both version paths
//! ([`entropy`], §2.5–§2.9, per-frame init injected as
//! [`entropy::EntropyInit`]), the full per-version header/tail
//! extraction ([`file_header`], §1 — both eras, descriptors, seek
//! tables, derived quantities), the vendor frame layout established
//! black-box against reference-binary fixtures ([`frame`] — CRC/flags
//! prologue, LE-word bit array, per-frame state init, stereo
//! interleave), and the whole-file facade ([`decoder::ApeDecoder`] +
//! [`decoder::FrameDeltaSource`] wiring the entropy layer behind the
//! [`pipeline::DeltaSource`] boundary).
//!
//! Still out of scope: the predictor pass between residual arrays and
//! PCM — the per-version `delta[]` history maintenance, the per-stage
//! `shift` position, the stage-1/adaptive composition, and the
//! decorrelation orientation on real streams — plus 24-bit / ≥ 3
//! channel reassembly and the old-era frame `k` init (unexercisable
//! black-box: the current vendor encoder emits 3990-era streams only).
//!
//! ## Allowed reference material (clean-room wall)
//!
//! Three staged sources were consulted: the format reference at
//! `docs/audio/ape/format-reference.md`, the workspace-local mirror of
//! the multimedia.cx wiki page at
//! `docs/audio/ape/wiki/Monkeys_Audio.wiki`, and the clean-room
//! extractor tables under `docs/audio/ape-cleanroom/tables/`. No
//! external implementation source of any kind, and no online lookups,
//! were used. The reference **binary** (console encoder v13.18) was
//! used strictly black-box — invoked as an opaque tool over engineered
//! PCM inputs to pin the frame-layout facts the format reference marks
//! as GAPs and to produce the committed test fixtures.
//!
//! ## Quick example
//!
//! ```
//! use oxideav_ape::header::{CompressionLevel, HeaderPrefix};
//!
//! // 'MAC ' + 3920 (v3.92, the wiki's worked example) + 2000 (normal).
//! let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
//! let h = HeaderPrefix::parse(&bytes).unwrap();
//! assert_eq!(h.version(), (3, 92));
//! assert_eq!(h.compression_level, CompressionLevel::Normal);
//! assert_eq!(h.header_tail_offset, 8);
//! ```
//!
//! ## Crate features
//!
//! | Feature    | Default | Effect                                                                 |
//! |------------|:-------:|------------------------------------------------------------------------|
//! | `registry` | yes     | Pulls in `oxideav-core` so the crate can declare itself to the framework registry once the decoder lands. |
//!
//! `default-features = false` gives a standalone build that exposes
//! only the file-header parser API surface and the crate-local
//! [`Error`] enum, with no framework dependency tree.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod cascade;
pub mod config;
pub mod decoder;
pub mod decorrelate;
pub mod entropy;
pub mod error;
pub mod file_header;
pub mod filter_config;
pub mod frame;
pub mod freq_model;
pub mod header;
pub mod pipeline;
pub mod predictor;
pub mod range_coder;
pub mod scalars;

pub use cascade::{
    cascade_decode, cascade_encode, filter_stage_decode, filter_stage_encode, StageState,
};
pub use config::StreamConfig;
pub use decoder::{ApeDecoder, FrameDecode, FrameDeltaSource};
pub use decorrelate::{
    decorrelate_pair, decorrelate_pair_arith_shift, reconstruct_block,
    reconstruct_block_arith_shift, reconstruct_pair, reconstruct_pair_arith_shift,
};
pub use entropy::{
    fold_residual, unfold_residual, EntropyInit, ResidualDecoder, ResidualEncoder,
    K_SUM_MIN_BOUNDARY,
};
pub use error::{Error, Result};
pub use file_header::{ApeDescriptor, FileInfo, FormatFlags};
pub use filter_config::{
    cascade_for_level, FilterCascade, FilterStage, FILTER_STAGES, MAX_CASCADE_DEPTH,
};
pub use frame::{
    crc32, decode_frame_residuals, FrameFlags, FramePrologue, FrameResiduals, FRAME_ENTROPY_INIT,
};
pub use freq_model::{
    counts_for_version, freqs_for_version, symbol_for_cum_freq, symbol_interval, symbol_width,
    COUNTS_GE3990, COUNTS_LE3980, FREQS_GE3990, FREQS_LE3980, FREQ_MODEL_VERSION_SPLIT,
    MODEL_ELEMENTS, POWERS_OF_TWO_MINUS_ONE, RANGE_OVERFLOW_SHIFT, RANGE_TOTAL_WIDTH,
};
pub use header::{CompressionLevel, HeaderPrefix, FILE_EXTENSION, HEADER_PREFIX_LEN, MAGIC};
pub use pipeline::{
    decode_frame, encode_frame, CorrelationRounding, DeltaSink, DeltaSource, FrameChannels,
};
pub use predictor::{
    adapt_sign, predict_dot, predict_step, predict_step_self_ref, residual_step,
    residual_step_self_ref,
};
pub use range_coder::{BitInput, RangeDecoder, RangeEncoder};
pub use scalars::{
    ksum_pivot, stage1_predict, KSUM_PIVOT_DIVISOR, PREDICTOR_HISTORY_SEED, STAGE1_FILTER_SHIFT,
    STAGE1_FILTER_WEIGHT,
};

/// Crate identifier used by the future `oxideav-core` registry entry.
pub const CRATE_NAME: &str = "oxideav-ape";

/// Identify whether `bytes` opens with the `'MAC '` magic. Cheap
/// O(1) probe a container demuxer can use to route to this crate
/// without committing to a full prefix parse.
pub fn is_ape_magic(bytes: &[u8]) -> bool {
    bytes.len() >= header::MAGIC.len() && bytes[..header::MAGIC.len()] == header::MAGIC
}

/// `oxideav-core` framework hook.
///
/// Phase 1 publishes only the crate name so the umbrella's
/// `make_codec_list` audit logs a stable identifier for the
/// scaffold. The full `register!` wire-up (decoder factory,
/// container tag) lands once Phase 2 pins a per-version header tail
/// and Phase 3 supplies enough of the range-decoder + IIR predictor
/// to emit PCM samples.
#[cfg(feature = "registry")]
pub fn registry_name() -> &'static str {
    CRATE_NAME
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_name_constant_is_stable() {
        assert_eq!(CRATE_NAME, "oxideav-ape");
    }

    #[test]
    fn magic_probe_accepts_well_formed_prefix() {
        assert!(is_ape_magic(b"MAC \x50\x0F\xD0\x07"));
    }

    #[test]
    fn magic_probe_rejects_short_buffer() {
        assert!(!is_ape_magic(b"MAC"));
        assert!(!is_ape_magic(b""));
    }

    #[test]
    fn magic_probe_rejects_wrong_magic() {
        assert!(!is_ape_magic(b"OggS\x00\x00\x00\x00"));
        // 'MAC!' is the most plausible single-byte typo.
        assert!(!is_ape_magic(b"MAC!\x50\x0F\xD0\x07"));
    }

    #[test]
    fn header_prefix_reexport_round_trips() {
        let h = HeaderPrefix {
            version_raw: 3920,
            compression_level: CompressionLevel::Normal,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        let bytes = h.encode_prefix();
        assert!(is_ape_magic(&bytes));
        let parsed = HeaderPrefix::parse(&bytes).unwrap();
        assert_eq!(parsed, h);
    }

    #[cfg(feature = "registry")]
    #[test]
    fn registry_name_matches_crate_name_constant() {
        assert_eq!(registry_name(), CRATE_NAME);
    }
}
