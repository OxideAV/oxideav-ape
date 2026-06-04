//! # oxideav-ape
//!
//! **Status:** clean-room Phase 1 bootstrap.
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
//! Everything past offset 8 — version-specific sound-parameters,
//! frame count, seek table, optional embedded WAV header, the range
//! decoder, the IIR-predictor cascade, and the channel-decorrelation
//! reconstructor — is **out of scope for Phase 1**. The staged docs
//! enumerate those layers only at the algorithm-sketch level and do
//! not pin the per-version header tail formats, the IIR coefficient
//! tables, the residual-coding `k`-parameter recurrence, the range
//! coder's frequency-table bounds, or the v3.97-vs-v3.98 layout
//! delta. Filling those in is a documented Phase 2 input.
//!
//! ## Allowed reference material (clean-room wall)
//!
//! Only the workspace-local mirror of the multimedia.cx wiki page at
//! `docs/audio/ape/wiki/Monkeys_Audio.wiki` was consulted for this
//! crate. No Monkey's Audio reference source (the C++ MAC binary
//! distribution), no FFmpeg `libav*` source, no third-party
//! reverse-engineering writeups beyond the cited wiki snapshot, and
//! no online lookups of any kind were used. Black-box validation
//! against the reference `mac` binary is a future-round option once
//! enough of the decoder lands to produce comparable PCM output.
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

pub mod error;
pub mod header;

pub use error::{Error, Result};
pub use header::{CompressionLevel, HeaderPrefix, FILE_EXTENSION, HEADER_PREFIX_LEN, MAGIC};

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
