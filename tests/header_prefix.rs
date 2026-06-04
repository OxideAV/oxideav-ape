//! End-to-end roundtrips of the Phase 1 8-byte header prefix as the
//! public API exposes it. Covers each documented compression level,
//! the wiki worked example (v3.92 / level 2000), and the public
//! crate-root re-exports.

use oxideav_ape::{
    is_ape_magic, CompressionLevel, Error, HeaderPrefix, FILE_EXTENSION, HEADER_PREFIX_LEN, MAGIC,
};

#[test]
fn magic_constant_is_ascii_mac_space() {
    assert_eq!(&MAGIC, b"MAC ");
    assert_eq!(HEADER_PREFIX_LEN, 8);
}

#[test]
fn wiki_worked_example_parses() {
    // 'MAC ' + version 3920 (= v3.92, the wiki worked example) +
    // compression level 2000 ("normal").
    let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
    assert!(is_ape_magic(&bytes));
    let h = HeaderPrefix::parse(&bytes).expect("well-formed prefix");
    assert_eq!(h.version_raw, 3920);
    assert_eq!(h.version(), (3, 92));
    assert_eq!(h.compression_level, CompressionLevel::Normal);
    assert_eq!(h.header_tail_offset, HEADER_PREFIX_LEN);
}

#[test]
fn every_documented_level_roundtrips_through_encode_parse() {
    for level in [
        CompressionLevel::Fast,
        CompressionLevel::Normal,
        CompressionLevel::High,
        CompressionLevel::ExtraHigh,
        CompressionLevel::Insane,
    ] {
        let original = HeaderPrefix {
            version_raw: 3920,
            compression_level: level,
            header_tail_offset: HEADER_PREFIX_LEN,
        };
        let bytes = original.encode_prefix();
        let parsed = HeaderPrefix::parse(&bytes).expect("documented level round-trips");
        assert_eq!(parsed, original);
    }
}

#[test]
fn truncated_buffer_is_rejected_before_magic_check() {
    let buf = [b'M', b'A', b'C', b' ', 0x50, 0x0F]; // 6 bytes < 8.
    assert_eq!(HeaderPrefix::parse(&buf).unwrap_err(), Error::Truncated);
}

#[test]
fn wrong_magic_at_offset_zero_is_rejected() {
    let buf = [b'X', b'X', b'X', b'X', 0x50, 0x0F, 0xD0, 0x07];
    assert_eq!(HeaderPrefix::parse(&buf).unwrap_err(), Error::InvalidMagic);
}

#[test]
fn unknown_compression_level_reports_raw_value() {
    let buf = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0x05, 0x00]; // level 5.
    assert_eq!(
        HeaderPrefix::parse(&buf).unwrap_err(),
        Error::UnknownCompressionLevel(5)
    );
}

#[test]
fn header_prefix_display_matches_phase1_format() {
    // Mirror of the unit-test anchor at the integration boundary so
    // a downstream caller can rely on the documented one-line
    // Display form for diagnostics.
    let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
    let h = HeaderPrefix::parse(&bytes).expect("well-formed prefix");
    assert_eq!(format!("{h}"), "MAC v3.92 (raw=3920) normal (2000)");
}

#[test]
fn compression_level_display_couples_label_with_raw_value() {
    assert_eq!(
        format!("{}", CompressionLevel::ExtraHigh),
        "extra high (4000)"
    );
}

#[test]
fn compression_level_all_constant_is_visible_through_public_api() {
    // The const array publishes the documented set so a caller can
    // iterate it without committing to a particular Rust-side
    // discriminant order.
    let raws: Vec<u16> = CompressionLevel::ALL.iter().map(|l| l.as_u16()).collect();
    assert_eq!(raws, vec![1000, 2000, 3000, 4000, 5000]);
}

#[test]
fn standard_conversion_traits_are_in_scope() {
    // `From<CompressionLevel> for u16` — forward conversion via the
    // standard trait, equivalent to `as_u16` but available as
    // `u16::from(level)` / `.into()`.
    let raw: u16 = u16::from(CompressionLevel::High);
    assert_eq!(raw, 3000);

    // `TryFrom<u16> for CompressionLevel` — reverse conversion via
    // the standard trait, equivalent to `from_u16`.
    let parsed = CompressionLevel::try_from(4000u16).unwrap();
    assert_eq!(parsed, CompressionLevel::ExtraHigh);
    assert_eq!(
        CompressionLevel::try_from(99u16).unwrap_err(),
        Error::UnknownCompressionLevel(99)
    );
}

#[test]
fn compression_level_orders_per_documented_gradient() {
    // The wiki §"Compression levels" lists profiles in ascending
    // raw-value order (1000 → 5000). The crate's `Ord` impl is required
    // to mirror that gradient, so a public-API caller can sort the type
    // and rely on the documented ordering at the integration boundary.
    let mut levels = vec![
        CompressionLevel::Insane,
        CompressionLevel::Fast,
        CompressionLevel::ExtraHigh,
        CompressionLevel::Normal,
        CompressionLevel::High,
    ];
    levels.sort();
    assert_eq!(levels, CompressionLevel::ALL.to_vec());

    // The "at or above `High`" predicate is the canonical use case the
    // ordering exists for.
    let high_or_above: Vec<CompressionLevel> = CompressionLevel::ALL
        .iter()
        .copied()
        .filter(|l| *l >= CompressionLevel::High)
        .collect();
    assert_eq!(
        high_or_above,
        vec![
            CompressionLevel::High,
            CompressionLevel::ExtraHigh,
            CompressionLevel::Insane,
        ]
    );
}

#[test]
fn header_prefix_version_helpers_are_in_scope() {
    // The standalone `major()` / `minor()` accessors must agree with
    // the tuple form at the public-API boundary. Anchored against the
    // wiki worked example (v3.92 → raw 3920).
    let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
    let h = HeaderPrefix::parse(&bytes).expect("well-formed prefix");
    assert_eq!(h.major(), 3);
    assert_eq!(h.minor(), 92);
    assert_eq!((h.major(), h.minor()), h.version());
}

#[test]
fn from_str_recovers_every_documented_label_from_its_display_form() {
    use core::str::FromStr;
    for level in CompressionLevel::ALL {
        // Round-trip through the narrative label exactly as
        // `Display` / `label()` emit it.
        let parsed = CompressionLevel::from_str(level.label()).unwrap();
        assert_eq!(parsed, level);
    }
    // Unknown labels surface the dedicated variant.
    assert_eq!(
        CompressionLevel::from_str("turbo").unwrap_err(),
        Error::UnknownCompressionLabel
    );
}

#[test]
fn file_extension_is_re_exported_at_crate_root() {
    // The staged docs pin `Extensions: ape` at the document head;
    // surface the constant at the crate-root re-export boundary so a
    // container demuxer matching on extensions reaches it directly.
    assert_eq!(FILE_EXTENSION, "ape");
}

#[test]
fn header_prefix_new_constructor_at_public_api() {
    // `HeaderPrefix::new(version_raw, level)` is the const-fn
    // constructor: it pins `header_tail_offset` to `HEADER_PREFIX_LEN`,
    // round-trips through `encode_prefix` + `parse`, and is invokable
    // in a `const` context at the public-API boundary.
    const PREFIX: HeaderPrefix = HeaderPrefix::new(3920, CompressionLevel::Normal);
    let bytes = PREFIX.encode_prefix();
    let parsed = HeaderPrefix::parse(&bytes).expect("const-built prefix round-trips");
    assert_eq!(parsed, PREFIX);
    assert_eq!(PREFIX.header_tail_offset, HEADER_PREFIX_LEN);
}

#[test]
fn compression_level_default_at_public_api_is_normal() {
    // The crate's `Default for CompressionLevel` impl anchors on the
    // middle profile of the documented ascending gradient. Surface the
    // choice at the integration boundary so a downstream caller using
    // `..Default::default()` struct-update form lands on `Normal`.
    let level: CompressionLevel = Default::default();
    assert_eq!(level, CompressionLevel::Normal);
}
