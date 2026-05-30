//! Crate-local error type for the Phase 1 file-header parser.
//!
//! The variants surface only the failure modes the 8-byte
//! `MAC ` + version + compression-level prefix parser can produce
//! against synthetic byte buffers built per the documented header
//! layout in `docs/audio/ape/wiki/Monkeys_Audio.wiki` (§"General
//! Details" "Header" table). Later phases will extend the enum as
//! version-specific header-tail parsers (sound parameters, frame
//! count, seek table, embedded WAV header), the residual range
//! decoder, the IIR predictor cascade, and the channel-decorrelation
//! reconstructor are landed under further docs.

/// Errors produced by the Phase 1 Monkey's Audio file-header parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The stream is missing the four-byte `'MAC '` ASCII magic at
    /// offset 0 or is shorter than eight bytes total. Per the
    /// documented header table the magic + version + compression
    /// level occupy file offsets `0x00..=0x07`.
    InvalidMagic,
    /// The stream is shorter than the eight-byte Phase 1 header
    /// prefix (`'MAC '` + `u16` version + `u16` compression level).
    Truncated,
    /// The 16-bit compression-level field carried a value outside
    /// the documented `{1000, 2000, 3000, 4000, 5000}` set the wiki
    /// pins as "fast / normal / high / extra high / insane". A value
    /// outside that set is either stream corruption or an
    /// out-of-scope encoder profile not covered by the staged docs.
    UnknownCompressionLevel(u16),
    /// Phase 1 only parses the 8-byte header prefix. Any operation
    /// that would require the per-version header-tail parser (sound
    /// parameters, frame count, seek table) returns this until the
    /// per-version layouts land under further docs.
    NotImplemented,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::InvalidMagic => {
                f.write_str("oxideav-ape: invalid 'MAC ' magic at offset 0")
            }
            Error::Truncated => f
                .write_str("oxideav-ape: stream truncated inside the 8-byte header prefix"),
            Error::UnknownCompressionLevel(v) => write!(
                f,
                "oxideav-ape: compression level {v} is outside the documented {{1000,2000,3000,4000,5000}} set"
            ),
            Error::NotImplemented => f.write_str(
                "oxideav-ape: feature not implemented in Phase 1 (header prefix parser only)",
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Crate-local `Result` alias.
pub type Result<T> = core::result::Result<T, Error>;
