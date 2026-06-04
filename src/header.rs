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

/// Canonical lowercase file extension the staged docs pin at the
/// document head (`Extensions: ape`). Surfaced as a `&str` constant
/// so a container demuxer can match against it without re-keying the
/// literal at every call site. The byte string does not carry the
/// leading `.` separator — call sites that need the dotted form can
/// prepend one.
pub const FILE_EXTENSION: &str = "ape";

/// Encoder profile carried in the 16-bit compression-level field.
///
/// The wiki §"Compression levels" pins five named profiles in ascending
/// raw-value order (1000 fast → 5000 insane). We expose each as an
/// enumerator, keep the raw u16 round-trippable via
/// [`CompressionLevel::as_u16`] / [`CompressionLevel::from_u16`], and
/// derive `Hash` + `Ord` so the type is usable as a hash-map key and
/// orderable per the documented gradient.
///
/// The `Ord` / `PartialOrd` impls compare by the raw on-wire value, so
/// `Fast < Normal < High < ExtraHigh < Insane` — the exact order the
/// staged docs list the profiles in. This makes "is the encoder at or
/// above the `High` profile" queries expressible without committing
/// the call site to the inherent Rust-discriminant order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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

impl PartialOrd for CompressionLevel {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CompressionLevel {
    /// Order by the raw 16-bit on-wire value, which is the order the
    /// staged docs list the profiles in (1000 → 5000). Equivalent to
    /// `self.as_u16().cmp(&other.as_u16())` but spelled out so the
    /// derive surface stays explicit about which axis is ordered.
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.as_u16().cmp(&other.as_u16())
    }
}

impl CompressionLevel {
    /// All five documented compression-level profiles, in the order
    /// the staged docs list them (fast → normal → high → extra high
    /// → insane). Exposed as a `const` so call sites can iterate the
    /// documented set without committing to any speculative future
    /// profile a later docs revision might introduce.
    pub const ALL: [CompressionLevel; 5] = [
        CompressionLevel::Fast,
        CompressionLevel::Normal,
        CompressionLevel::High,
        CompressionLevel::ExtraHigh,
        CompressionLevel::Insane,
    ];

    /// Iterate every documented compression-level profile, in the
    /// order the staged docs list them. Convenience wrapper over
    /// [`CompressionLevel::ALL`].
    pub fn iter() -> core::iter::Copied<core::slice::Iter<'static, CompressionLevel>> {
        Self::ALL.iter().copied()
    }

    /// Encode the profile as the raw little-endian u16 the file
    /// carries. `const fn` so the call is usable in `const` contexts
    /// (e.g. building static lookup tables).
    pub const fn as_u16(self) -> u16 {
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
    /// the documented set. `const fn` so the call is usable in `const`
    /// contexts.
    pub const fn from_u16(raw: u16) -> Result<Self> {
        match raw {
            1000 => Ok(CompressionLevel::Fast),
            2000 => Ok(CompressionLevel::Normal),
            3000 => Ok(CompressionLevel::High),
            4000 => Ok(CompressionLevel::ExtraHigh),
            5000 => Ok(CompressionLevel::Insane),
            other => Err(Error::UnknownCompressionLevel(other)),
        }
    }

    /// Human-readable label per the wiki narrative. `const fn` so the
    /// call is usable in `const` contexts.
    pub const fn label(self) -> &'static str {
        match self {
            CompressionLevel::Fast => "fast",
            CompressionLevel::Normal => "normal",
            CompressionLevel::High => "high",
            CompressionLevel::ExtraHigh => "extra high",
            CompressionLevel::Insane => "insane",
        }
    }
}

impl Default for CompressionLevel {
    /// `Normal` (raw value `2000`) — the staged docs list the five
    /// profiles in ascending raw-value order (`Fast` 1000 → `Insane`
    /// 5000), and the profile carrying the unmodified "normal" label
    /// is the natural anchor for a `Default` since it neither under-
    /// nor over-commits the encoder side of the field. The choice
    /// avoids picking either extremum (`Fast` / `Insane`) as a
    /// default, which would bias `..CompressionLevel::default()`
    /// struct-update constructions toward one end of the documented
    /// gradient.
    fn default() -> Self {
        CompressionLevel::Normal
    }
}

impl From<CompressionLevel> for u16 {
    /// Forward conversion to the raw on-wire little-endian u16 the
    /// file carries. Equivalent to [`CompressionLevel::as_u16`] but
    /// exposed through the standard `From` trait so call sites can
    /// rely on `u16::from(level)` and `.into()`-style coercions.
    fn from(level: CompressionLevel) -> u16 {
        level.as_u16()
    }
}

impl TryFrom<u16> for CompressionLevel {
    type Error = crate::error::Error;

    /// Reverse conversion from the raw 16-bit on-wire field. Returns
    /// [`Error::UnknownCompressionLevel`] for any value outside the
    /// documented `{1000, 2000, 3000, 4000, 5000}` set. Thin wrapper
    /// over [`CompressionLevel::from_u16`] exposed through the
    /// standard `TryFrom` trait so call sites can rely on
    /// `CompressionLevel::try_from(raw)` and `.try_into()`-style
    /// coercions.
    fn try_from(raw: u16) -> Result<Self> {
        Self::from_u16(raw)
    }
}

impl core::str::FromStr for CompressionLevel {
    type Err = crate::error::Error;

    /// Parse a profile from its narrative label. The match is
    /// case-insensitive on the five documented labels — "fast",
    /// "normal", "high", "extra high", "insane" — and ignores ASCII
    /// whitespace at both ends of the input. The inverse of
    /// [`CompressionLevel::label`]. Returns
    /// [`Error::UnknownCompressionLabel`] for any other input.
    fn from_str(s: &str) -> Result<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fast" => Ok(CompressionLevel::Fast),
            "normal" => Ok(CompressionLevel::Normal),
            "high" => Ok(CompressionLevel::High),
            "extra high" => Ok(CompressionLevel::ExtraHigh),
            "insane" => Ok(CompressionLevel::Insane),
            _ => Err(crate::error::Error::UnknownCompressionLabel),
        }
    }
}

impl core::fmt::Display for CompressionLevel {
    /// Writes the wiki narrative's lowercase label
    /// ([`CompressionLevel::label`]) followed by the raw decimal value
    /// in parentheses, e.g. `"normal (2000)"`. Carries both the named
    /// profile and the stored field value so a single line of diagnostic
    /// output identifies the level unambiguously.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{} ({})", self.label(), self.as_u16())
    }
}

/// The parsed 8-byte Monkey's Audio header prefix.
///
/// `header_tail_offset` is always `HEADER_PREFIX_LEN` (8). Phase 1
/// surfaces it as a named field so the call site reads the boundary
/// explicitly when handing the remainder to a future per-version
/// header-tail parser.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
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
    /// Construct a `HeaderPrefix` from a raw decimal-coded version and
    /// a typed [`CompressionLevel`]. `header_tail_offset` is fixed to
    /// [`HEADER_PREFIX_LEN`] (8) — the only value Phase 1 ever
    /// publishes for the boundary. `const fn` so the constructor is
    /// usable in `const` contexts (e.g. building a static lookup of
    /// well-known prefixes).
    pub const fn new(version_raw: u16, compression_level: CompressionLevel) -> Self {
        HeaderPrefix {
            version_raw,
            compression_level,
            header_tail_offset: HEADER_PREFIX_LEN,
        }
    }

    /// Return the documented decimal-coded major/minor pair.
    ///
    /// Encoders write the version as `major * 1000 + minor * 10`,
    /// which the wiki worked example (v3.92 → 3920) confirms. The
    /// decode is `(raw / 1000, (raw % 1000) / 10)`. Spec versions
    /// below v3 are pre-history and not covered by the staged docs;
    /// the helper still returns the arithmetic decomposition without
    /// gating on a minimum. `const fn` so the helper is usable in
    /// `const` contexts.
    pub const fn version(&self) -> (u16, u16) {
        (self.major(), self.minor())
    }

    /// Major component of the decimal-coded version field
    /// (`version_raw / 1000`). For the wiki worked example
    /// (`version_raw = 3920`) the major is `3`. Equivalent to
    /// `self.version().0` but available as a one-shot accessor so call
    /// sites that only need the major component skip the tuple destructure.
    /// `const fn` so the accessor is usable in `const` contexts.
    pub const fn major(&self) -> u16 {
        self.version_raw / 1000
    }

    /// Minor component of the decimal-coded version field
    /// (`(version_raw % 1000) / 10`). For the wiki worked example
    /// (`version_raw = 3920`) the minor is `92`. Equivalent to
    /// `self.version().1` but available as a one-shot accessor so call
    /// sites that only need the minor component skip the tuple destructure.
    /// `const fn` so the accessor is usable in `const` contexts.
    pub const fn minor(&self) -> u16 {
        (self.version_raw % 1000) / 10
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
    /// round-trips through [`HeaderPrefix::parse`]. `const fn` so the
    /// encoder is usable in `const` contexts (e.g. building a static
    /// well-formed prefix at compile time).
    pub const fn encode_prefix(&self) -> [u8; HEADER_PREFIX_LEN] {
        let raw_level = self.compression_level.as_u16().to_le_bytes();
        let raw_version = self.version_raw.to_le_bytes();
        [
            MAGIC[0],
            MAGIC[1],
            MAGIC[2],
            MAGIC[3],
            raw_version[0],
            raw_version[1],
            raw_level[0],
            raw_level[1],
        ]
    }
}

impl core::fmt::Display for HeaderPrefix {
    /// Writes a single-line summary in the form
    /// `"MAC v3.92 (raw=3920) normal (2000)"`. Combines the
    /// [`HeaderPrefix::version`] decode and the
    /// [`CompressionLevel`] `Display` form, with the raw
    /// `version_raw` field shown verbatim so the diagnostic stays
    /// faithful to the on-wire bytes even when the encoder writes a
    /// raw value the staged docs do not pin a worked example for.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let (major, minor) = self.version();
        write!(
            f,
            "MAC v{}.{:02} (raw={}) {}",
            major, minor, self.version_raw, self.compression_level
        )
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

    #[test]
    fn compression_level_display_pairs_label_and_raw() {
        // Display couples the wiki narrative's lowercase label with the
        // raw u16 stored on the wire. Anchors the diagnostic format the
        // crate's `Display` impl promises so downstream call sites can
        // depend on it in error messages.
        assert_eq!(format!("{}", CompressionLevel::Fast), "fast (1000)");
        assert_eq!(format!("{}", CompressionLevel::Normal), "normal (2000)");
        assert_eq!(format!("{}", CompressionLevel::High), "high (3000)");
        assert_eq!(
            format!("{}", CompressionLevel::ExtraHigh),
            "extra high (4000)"
        );
        assert_eq!(format!("{}", CompressionLevel::Insane), "insane (5000)");
    }

    #[test]
    fn header_prefix_display_renders_wiki_worked_example() {
        // The wiki narrative pins exactly one worked example
        // (v3.92 / level 2000). Anchor the Display output against it
        // so the format string survives future refactors.
        let h = HeaderPrefix {
            version_raw: 3920,
            compression_level: CompressionLevel::Normal,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        assert_eq!(format!("{h}"), "MAC v3.92 (raw=3920) normal (2000)");
    }

    #[test]
    fn header_prefix_display_keeps_raw_field_verbatim() {
        // The decimal-coded version helper rounds (raw / 1000,
        // (raw % 1000) / 10); the Display impl additionally surfaces
        // the raw u16 so an encoder that wrote a value the docs do
        // not pin a worked example for is still distinguishable from
        // a documented one with the same decomposition.
        let h = HeaderPrefix {
            version_raw: 3925, // decomposes to (3, 92) just like 3920.
            compression_level: CompressionLevel::Fast,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        assert_eq!(format!("{h}"), "MAC v3.92 (raw=3925) fast (1000)");
    }

    #[test]
    fn compression_level_all_lists_the_five_documented_profiles_in_doc_order() {
        // The wiki §"Compression levels" lists the five profiles in
        // ascending raw-value order (1000 → 5000). `ALL` must mirror
        // that order so call sites that iterate it walk the
        // documented sequence rather than a Rust-source-declaration
        // accident.
        let raws: Vec<u16> = CompressionLevel::ALL.iter().map(|l| l.as_u16()).collect();
        assert_eq!(raws, vec![1000, 2000, 3000, 4000, 5000]);
        assert_eq!(CompressionLevel::ALL.len(), 5);
    }

    #[test]
    fn compression_level_iter_walks_the_documented_set_once() {
        let collected: Vec<CompressionLevel> = CompressionLevel::iter().collect();
        assert_eq!(collected.as_slice(), &CompressionLevel::ALL[..]);
        // The iterator is one-shot per call — a second call yields
        // the same sequence afresh (rather than picking up where the
        // first left off), which is the std-iter contract for fresh
        // `iter()` calls on a slice.
        let twice: usize = CompressionLevel::iter().count() + CompressionLevel::iter().count();
        assert_eq!(twice, 10);
    }

    #[test]
    fn try_from_u16_mirrors_from_u16() {
        for raw in [1000u16, 2000, 3000, 4000, 5000] {
            let via_trait = <CompressionLevel as TryFrom<u16>>::try_from(raw).unwrap();
            let via_inherent = CompressionLevel::from_u16(raw).unwrap();
            assert_eq!(via_trait, via_inherent);
        }
        // Unknown values surface the same Error variant either way.
        assert_eq!(
            <CompressionLevel as TryFrom<u16>>::try_from(1234).unwrap_err(),
            Error::UnknownCompressionLevel(1234)
        );
    }

    #[test]
    fn from_compression_level_into_u16_round_trips() {
        for level in CompressionLevel::ALL {
            let raw: u16 = u16::from(level);
            assert_eq!(raw, level.as_u16());
            let back: CompressionLevel = TryFrom::try_from(raw).unwrap();
            assert_eq!(back, level);
        }
    }

    #[test]
    fn from_str_parses_every_documented_label() {
        use core::str::FromStr;
        for level in CompressionLevel::ALL {
            let parsed = CompressionLevel::from_str(level.label()).unwrap();
            assert_eq!(parsed, level);
        }
    }

    #[test]
    fn from_str_is_case_insensitive_and_trims_whitespace() {
        use core::str::FromStr;
        // Case variations.
        assert_eq!(
            CompressionLevel::from_str("FAST").unwrap(),
            CompressionLevel::Fast
        );
        assert_eq!(
            CompressionLevel::from_str("Normal").unwrap(),
            CompressionLevel::Normal
        );
        assert_eq!(
            CompressionLevel::from_str("Extra High").unwrap(),
            CompressionLevel::ExtraHigh
        );
        // Leading/trailing whitespace stripped.
        assert_eq!(
            CompressionLevel::from_str("  insane\n").unwrap(),
            CompressionLevel::Insane
        );
    }

    #[test]
    fn from_str_rejects_undocumented_label() {
        use core::str::FromStr;
        assert_eq!(
            CompressionLevel::from_str("turbo").unwrap_err(),
            Error::UnknownCompressionLabel
        );
        // The numeric form is not a documented narrative label —
        // callers should use `from_u16` / `TryFrom<u16>` for that.
        assert_eq!(
            CompressionLevel::from_str("2000").unwrap_err(),
            Error::UnknownCompressionLabel
        );
        // Empty string rejects.
        assert_eq!(
            CompressionLevel::from_str("").unwrap_err(),
            Error::UnknownCompressionLabel
        );
        // Whitespace-only rejects.
        assert_eq!(
            CompressionLevel::from_str("   ").unwrap_err(),
            Error::UnknownCompressionLabel
        );
    }

    #[test]
    fn compression_level_ord_matches_documented_ascending_gradient() {
        // The wiki §"Compression levels" lists profiles in ascending
        // raw-value order: 1000 fast → 2000 normal → 3000 high →
        // 4000 extra high → 5000 insane. The `Ord` impl is required
        // to mirror that gradient (i.e. order by the raw on-wire
        // u16), so "is this level at or above `High`" queries are
        // expressible without committing to a Rust-discriminant
        // accident.
        assert!(CompressionLevel::Fast < CompressionLevel::Normal);
        assert!(CompressionLevel::Normal < CompressionLevel::High);
        assert!(CompressionLevel::High < CompressionLevel::ExtraHigh);
        assert!(CompressionLevel::ExtraHigh < CompressionLevel::Insane);

        // The induced sort order across the full set matches the
        // declared `ALL` order verbatim.
        let mut shuffled = [
            CompressionLevel::Insane,
            CompressionLevel::Fast,
            CompressionLevel::ExtraHigh,
            CompressionLevel::Normal,
            CompressionLevel::High,
        ];
        shuffled.sort();
        assert_eq!(shuffled, CompressionLevel::ALL);
    }

    #[test]
    fn compression_level_hash_is_distinct_across_documented_set() {
        // `Hash` lets the type be used as a `HashMap` key. A trivial
        // smoke-test inserts every documented profile into a
        // `HashMap` and asserts each round-trips with a distinct
        // payload — i.e. no two profiles collide under `Hash + Eq`.
        use std::collections::HashMap;
        let mut by_level: HashMap<CompressionLevel, u16> = HashMap::new();
        for level in CompressionLevel::ALL {
            by_level.insert(level, level.as_u16());
        }
        assert_eq!(by_level.len(), CompressionLevel::ALL.len());
        for level in CompressionLevel::ALL {
            assert_eq!(by_level.get(&level), Some(&level.as_u16()));
        }
    }

    #[test]
    fn version_arithmetic_decomposes_every_u16_consistently() {
        // Anti-fuzz: the `version()` decomposition is pure arithmetic
        // (`(raw / 1000, (raw % 1000) / 10)`) and must satisfy that
        // identity for every possible raw value. Also asserts the
        // standalone `major()` / `minor()` accessors agree with the
        // tuple form, so the three entry points stay in lockstep.
        for raw in 0u16..=u16::MAX {
            let h = HeaderPrefix {
                version_raw: raw,
                compression_level: CompressionLevel::Normal,
                header_tail_offset: HEADER_PREFIX_LEN,
            };
            let (major, minor) = h.version();
            assert_eq!(major, raw / 1000);
            assert_eq!(minor, (raw % 1000) / 10);
            assert_eq!(h.major(), major);
            assert_eq!(h.minor(), minor);
        }
    }

    #[test]
    fn header_prefix_hash_pairs_with_eq() {
        // `HeaderPrefix` derives `Hash + Eq` so it can index a
        // `HashSet`. Two equal prefixes must hash identically and
        // dedup to one slot; a difference in any field must produce
        // distinct entries.
        use std::collections::HashSet;
        let baseline = HeaderPrefix {
            version_raw: 3920,
            compression_level: CompressionLevel::Normal,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        let twin = baseline;
        let mut set: HashSet<HeaderPrefix> = HashSet::new();
        set.insert(baseline);
        set.insert(twin);
        assert_eq!(set.len(), 1, "Eq twins must dedup");

        let differs_in_level = HeaderPrefix {
            compression_level: CompressionLevel::High,
            ..baseline
        };
        let differs_in_version = HeaderPrefix {
            version_raw: 3970,
            ..baseline
        };
        set.insert(differs_in_level);
        set.insert(differs_in_version);
        assert_eq!(set.len(), 3);
    }

    #[test]
    fn version_helpers_match_worked_example() {
        // The wiki narrative pins exactly one worked example
        // (v3.92 → raw 3920). Anchor the standalone `major()` /
        // `minor()` accessors against it so the helper signatures
        // survive future refactors.
        let h = HeaderPrefix {
            version_raw: 3920,
            compression_level: CompressionLevel::Normal,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        assert_eq!(h.major(), 3);
        assert_eq!(h.minor(), 92);
        assert_eq!((h.major(), h.minor()), h.version());
    }

    #[test]
    fn from_str_label_to_display_round_trips() {
        // For every documented profile, `Display`'s "label (raw)" form
        // can be split on " (" to recover the label, and the label
        // round-trips through `FromStr`.
        use core::str::FromStr;
        for level in CompressionLevel::ALL {
            let displayed = format!("{level}");
            let (label, rest) = displayed.split_once(" (").expect("space-paren split");
            assert!(rest.ends_with(')'));
            let parsed = CompressionLevel::from_str(label).unwrap();
            assert_eq!(parsed, level);
        }
    }

    #[test]
    fn compression_level_default_is_normal() {
        // `Normal` is the documented middle profile of the ascending
        // raw-value gradient. Anchor the choice at the unit-test
        // boundary so an accidental change to the `Default` impl gets
        // a hard failure.
        assert_eq!(CompressionLevel::default(), CompressionLevel::Normal);
        // Struct-update construction picks the default up by name.
        let h = HeaderPrefix {
            version_raw: 3920,
            compression_level: CompressionLevel::default(),
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        assert_eq!(h.compression_level, CompressionLevel::Normal);
    }

    #[test]
    fn file_extension_is_canonical_lowercase_ape() {
        // The staged docs pin `Extensions: ape` at the document head.
        // The constant must mirror it byte-for-byte without the
        // leading dot.
        assert_eq!(FILE_EXTENSION, "ape");
        assert!(!FILE_EXTENSION.starts_with('.'));
    }

    #[test]
    fn compression_level_accessors_are_const_eval_capable() {
        // `as_u16`, `from_u16`, and `label` are `const fn`, so each one
        // can be invoked at compile time. The assertions below exist
        // primarily so the compiler is forced to const-evaluate every
        // call — a regression that demoted any accessor from `const fn`
        // would surface here as a "non-const expression in const
        // context" build error long before the runtime check fires.
        const FAST_RAW: u16 = CompressionLevel::Fast.as_u16();
        const NORMAL_LABEL: &str = CompressionLevel::Normal.label();
        const INSANE_FROM_RAW: Result<CompressionLevel> = CompressionLevel::from_u16(5000);
        const REJECTED: Result<CompressionLevel> = CompressionLevel::from_u16(1234);

        assert_eq!(FAST_RAW, 1000);
        assert_eq!(NORMAL_LABEL, "normal");
        assert_eq!(INSANE_FROM_RAW, Ok(CompressionLevel::Insane));
        assert_eq!(REJECTED, Err(Error::UnknownCompressionLevel(1234)));
    }

    #[test]
    fn header_prefix_new_constructor_fixes_tail_offset_to_eight() {
        // The Phase 1 constructor is required to pin
        // `header_tail_offset` to `HEADER_PREFIX_LEN` (8). No other
        // value is reachable through the constructor surface, so call
        // sites that build `HeaderPrefix` through `new` cannot accidentally
        // drift the boundary the per-version tail parser will pick up at.
        let h = HeaderPrefix::new(3920, CompressionLevel::Normal);
        assert_eq!(h.version_raw, 3920);
        assert_eq!(h.compression_level, CompressionLevel::Normal);
        assert_eq!(h.header_tail_offset, HEADER_PREFIX_LEN);

        // Const-eval the constructor at compile time so a future demotion
        // surfaces as a build error.
        const PREFIX: HeaderPrefix = HeaderPrefix::new(3970, CompressionLevel::High);
        assert_eq!(PREFIX.version_raw, 3970);
        assert_eq!(PREFIX.compression_level, CompressionLevel::High);
    }

    #[test]
    fn header_prefix_accessors_are_const_eval_capable() {
        // `version`, `major`, `minor`, and `encode_prefix` are all
        // `const fn` so they can be invoked at compile time. As with
        // the `CompressionLevel` accessor test above, the const
        // contexts here exist to force the compiler to const-evaluate
        // each call — any regression that demoted a method to non-const
        // would surface as a build error.
        const PREFIX: HeaderPrefix = HeaderPrefix::new(3920, CompressionLevel::Normal);
        const VERSION_PAIR: (u16, u16) = PREFIX.version();
        const MAJOR: u16 = PREFIX.major();
        const MINOR: u16 = PREFIX.minor();
        const ENCODED: [u8; HEADER_PREFIX_LEN] = PREFIX.encode_prefix();

        assert_eq!(VERSION_PAIR, (3, 92));
        assert_eq!(MAJOR, 3);
        assert_eq!(MINOR, 92);
        // The wiki worked example pairs v3.92 + level 2000 with the
        // little-endian bytes `0x50 0x0F 0xD0 0x07` past the magic.
        assert_eq!(ENCODED, [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07]);
    }

    #[test]
    fn header_prefix_new_round_trips_through_parse() {
        // Whatever the constructor builds must survive an `encode_prefix`
        // round-trip back through `parse`. Covers every documented
        // compression level so the constructor is interchangeable with
        // direct struct-literal construction at the encoder side.
        for level in CompressionLevel::ALL {
            let built = HeaderPrefix::new(3970, level);
            let bytes = built.encode_prefix();
            let parsed = HeaderPrefix::parse(&bytes).expect("constructor round-trips");
            assert_eq!(parsed, built);
        }
    }

    #[test]
    fn single_byte_mutation_of_worked_example_is_always_well_defined() {
        // Anti-fuzz: every 1-byte mutation of the wiki worked example
        // must either parse successfully (still a valid prefix) or
        // return one of the documented `Error` variants. No panic, no
        // `NotImplemented` leakage out of `parse` (Phase 1 reserves
        // `NotImplemented` for the per-version tail parser the staged
        // docs do not pin).
        let baseline = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
        for offset in 0..HEADER_PREFIX_LEN {
            for delta in 1u16..=255 {
                let mut mutated = baseline;
                mutated[offset] = baseline[offset].wrapping_add(delta as u8);
                match HeaderPrefix::parse(&mutated) {
                    Ok(_) => {} // a different-but-valid prefix.
                    Err(Error::InvalidMagic) => {
                        // Mutations inside offsets 0..4 can flip the
                        // magic; that's the only place this variant
                        // can fire on an 8-byte buffer.
                        assert!(offset < 4, "InvalidMagic at offset {offset}");
                    }
                    Err(Error::UnknownCompressionLevel(_)) => {
                        // Mutations inside offsets 6..8 can flip the
                        // compression-level field off the documented
                        // set; offsets 4..6 only touch `version_raw`,
                        // which `parse` does not gate.
                        assert!(offset >= 6, "UnknownCompressionLevel at offset {offset}");
                    }
                    Err(Error::Truncated) => panic!(
                        "Truncated reported for an 8-byte buffer at offset {offset}"
                    ),
                    Err(Error::UnknownCompressionLabel) => panic!(
                        "UnknownCompressionLabel leaked out of parse() at offset {offset}; that variant belongs to FromStr, not the binary prefix parser"
                    ),
                    Err(Error::NotImplemented) => panic!(
                        "NotImplemented leaked out of parse() at offset {offset}; Phase 1 reserves this for the per-version tail parser"
                    ),
                }
            }
        }
    }
}
