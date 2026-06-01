//! End-to-end roundtrips of the Phase 1 8-byte header prefix as the
//! public API exposes it. Covers each documented compression level,
//! the wiki worked example (v3.92 / level 2000), and the public
//! crate-root re-exports.

use oxideav_ape::{is_ape_magic, CompressionLevel, Error, HeaderPrefix, HEADER_PREFIX_LEN, MAGIC};

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
