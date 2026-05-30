//! Phase 1 file-header prefix parser.
//!
//! Implements the 8-byte header prefix the staged docs at
//! `docs/audio/ape/wiki/Monkeys_Audio.wiki` §"General Details" pin:
//!
//! ```text
//!   offset  size  field
//!   0x00       4  'MAC ' ASCII magic
//!   0x04       2  version (little-endian u16; e.g. 3920 = v3.92)
//!   0x06       2  compression level (little-endian u16;
//!                 1000 fast / 2000 normal / 3000 high /
//!                 4000 extra high / 5000 insane)
//! ```
//!
//! Everything after offset `0x08` is version-dependent (sound
//! parameters, frame count, seek table, optional embedded WAV
//! header). The wiki narrative says "the rest of header data depends
//! on file version" but does not enumerate the per-version layouts;
//! that's a documented Phase 2 input. Phase 1 therefore stops at
//! offset 8 and reports the boundary so a caller can hand the
//! remainder to a future per-version parser.

use crate::error::{Error, Result};

/// Four-byte ASCII magic that opens every Monkey's Audio file.
pub const MAGIC: [u8; 4] = *b"MAC ";

/// Length of the Phase 1 header prefix in bytes.
pub const HEADER_PREFIX_LEN: usize = 8;

/// Encoder profile carried in the 16-bit compression-level field.
///
/// The wiki §"Compression levels" pins five named profiles. We expose
/// each as an enumerator and keep the raw u16 round-trippable via
/// [`CompressionLevel::as_u16`] / [`CompressionLevel::from_u16`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionLevel {
    /// 1000 — "fast".
    Fast,
    /// 2000 — "normal".
    Normal,
    /// 3000 — "high".
    High,
    /// 4000 — "extra high".
    ExtraHigh,
    /// 5000 — "insane".
    Insane,
}

impl CompressionLevel {
    /// Encode the profile as the raw little-endian u16 the file
    /// carries.
    pub fn as_u16(self) -> u16 {
        match self {
            CompressionLevel::Fast => 1000,
            CompressionLevel::Normal => 2000,
            CompressionLevel::High => 3000,
            CompressionLevel::ExtraHigh => 4000,
            CompressionLevel::Insane => 5000,
        }
    }

    /// Map a raw 16-bit field value to the named profile, or return
    /// [`Error::UnknownCompressionLevel`] if the value falls outside
    /// the documented set.
    pub fn from_u16(raw: u16) -> Result<Self> {
        match raw {
            1000 => Ok(CompressionLevel::Fast),
            2000 => Ok(CompressionLevel::Normal),
            3000 => Ok(CompressionLevel::High),
            4000 => Ok(CompressionLevel::ExtraHigh),
            5000 => Ok(CompressionLevel::Insane),
            other => Err(Error::UnknownCompressionLevel(other)),
        }
    }

    /// Human-readable label per the wiki narrative.
    pub fn label(self) -> &'static str {
        match self {
            CompressionLevel::Fast => "fast",
            CompressionLevel::Normal => "normal",
            CompressionLevel::High => "high",
            CompressionLevel::ExtraHigh => "extra high",
            CompressionLevel::Insane => "insane",
        }
    }
}

/// The parsed 8-byte Monkey's Audio header prefix.
///
/// `header_tail_offset` is always `HEADER_PREFIX_LEN` (8). Phase 1
/// surfaces it as a named field so the call site reads the boundary
/// explicitly when handing the remainder to a future per-version
/// header-tail parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderPrefix {
    /// Raw 16-bit version field. Encoders write the spec major and
    /// minor as `major * 1000 + minor * 10`; e.g. v3.92 is 3920 and
    /// v3.97 is 3970. The wiki narrative gives 3920 as the worked
    /// example; further per-version layouts are not pinned in the
    /// staged docs.
    pub version_raw: u16,
    /// The decoded compression level.
    pub compression_level: CompressionLevel,
    /// Byte offset at which the version-dependent header tail starts.
    /// Always [`HEADER_PREFIX_LEN`] in Phase 1; carried as a field so
    /// downstream parsers can pick up from a documented index.
    pub header_tail_offset: usize,
}

impl HeaderPrefix {
    /// Return the documented decimal-coded major/minor pair.
    ///
    /// Encoders write the version as `major * 1000 + minor * 10`,
    /// which the wiki worked example (v3.92 → 3920) confirms. The
    /// decode is `(raw / 1000, (raw % 1000) / 10)`. Spec versions
    /// below v3 are pre-history and not covered by the staged docs;
    /// the helper still returns the arithmetic decomposition without
    /// gating on a minimum.
    pub fn version(&self) -> (u16, u16) {
        let major = self.version_raw / 1000;
        let minor = (self.version_raw % 1000) / 10;
        (major, minor)
    }

    /// Parse the 8-byte header prefix from `bytes`.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() < HEADER_PREFIX_LEN {
            return Err(Error::Truncated);
        }
        if bytes[0..4] != MAGIC {
            return Err(Error::InvalidMagic);
        }
        let version_raw = u16::from_le_bytes([bytes[4], bytes[5]]);
        let raw_level = u16::from_le_bytes([bytes[6], bytes[7]]);
        let compression_level = CompressionLevel::from_u16(raw_level)?;
        Ok(HeaderPrefix {
            version_raw,
            compression_level,
            header_tail_offset: HEADER_PREFIX_LEN,
        })
    }

    /// Encode the prefix into an 8-byte little-endian buffer that
    /// round-trips through [`HeaderPrefix::parse`].
    pub fn encode_prefix(&self) -> [u8; HEADER_PREFIX_LEN] {
        let mut out = [0u8; HEADER_PREFIX_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4..6].copy_from_slice(&self.version_raw.to_le_bytes());
        out[6..8].copy_from_slice(&self.compression_level.as_u16().to_le_bytes());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_is_ascii_mac_space() {
        assert_eq!(&MAGIC, b"MAC ");
    }

    #[test]
    fn parses_v392_normal_per_wiki_worked_example() {
        // 'MAC ' + 3920 (v3.92 from the wiki) + 2000 (normal).
        let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
        let h = HeaderPrefix::parse(&bytes).expect("well-formed prefix");
        assert_eq!(h.version_raw, 3920);
        assert_eq!(h.version(), (3, 92));
        assert_eq!(h.compression_level, CompressionLevel::Normal);
        assert_eq!(h.header_tail_offset, 8);
    }

    #[test]
    fn all_documented_compression_levels_roundtrip() {
        for (raw, expected, label) in [
            (1000u16, CompressionLevel::Fast, "fast"),
            (2000, CompressionLevel::Normal, "normal"),
            (3000, CompressionLevel::High, "high"),
            (4000, CompressionLevel::ExtraHigh, "extra high"),
            (5000, CompressionLevel::Insane, "insane"),
        ] {
            let parsed = CompressionLevel::from_u16(raw).expect("documented level");
            assert_eq!(parsed, expected);
            assert_eq!(parsed.as_u16(), raw);
            assert_eq!(parsed.label(), label);
        }
    }

    #[test]
    fn rejects_unknown_compression_level() {
        // 'MAC ' + 3920 + 1234 (not in documented set).
        let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD2, 0x04];
        assert_eq!(
            HeaderPrefix::parse(&bytes).unwrap_err(),
            Error::UnknownCompressionLevel(1234)
        );
    }

    #[test]
    fn rejects_missing_magic() {
        let bytes = [b'M', b'A', b'C', b'!', 0x50, 0x0F, 0xD0, 0x07];
        assert_eq!(
            HeaderPrefix::parse(&bytes).unwrap_err(),
            Error::InvalidMagic
        );
    }

    #[test]
    fn rejects_truncated_prefix() {
        // Any length below 8 fails before the magic check.
        for len in 0..HEADER_PREFIX_LEN {
            let buf = vec![0u8; len];
            assert_eq!(HeaderPrefix::parse(&buf).unwrap_err(), Error::Truncated);
        }
    }

    #[test]
    fn encode_prefix_roundtrips_through_parse() {
        let original = HeaderPrefix {
            version_raw: 3970,
            compression_level: CompressionLevel::High,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        let bytes = original.encode_prefix();
        let parsed = HeaderPrefix::parse(&bytes).expect("encoded prefix round-trips");
        assert_eq!(parsed, original);
        assert_eq!(parsed.version(), (3, 97));
    }

    #[test]
    fn extra_bytes_after_prefix_are_ignored_phase1() {
        // 8-byte well-formed prefix + 16 bytes of opaque tail.
        let mut bytes = vec![b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
        bytes.extend(std::iter::repeat(0xAA).take(16));
        let h = HeaderPrefix::parse(&bytes).expect("trailing bytes ignored");
        assert_eq!(h.header_tail_offset, 8);
        // The tail bytes are the caller's responsibility — Phase 1
        // explicitly stops at offset 8.
        assert_eq!(
            &bytes[h.header_tail_offset..h.header_tail_offset + 4],
            &[0xAA; 4]
        );
    }
}
