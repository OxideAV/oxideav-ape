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
    /// A textual compression-level label (e.g. parsed via
    /// [`core::str::FromStr`]) did not match any of the five
    /// documented narrative labels — "fast", "normal", "high",
    /// "extra high", "insane".
    UnknownCompressionLabel,
    /// Phase 1 only parses the 8-byte header prefix. Any operation
    /// that would require the per-version header-tail parser (sound
    /// parameters, frame count, seek table) returns this until the
    /// per-version layouts land under further docs.
    NotImplemented,
    /// The stereo-decorrelation block reconstructor was handed input
    /// and output slices that did not all agree on length. The
    /// reconstructor reads `x[i]` and `y[i]` in lockstep and writes
    /// `left[i]` and `right[i]` per sample, so any of the four slices
    /// disagreeing on `len()` is a caller-side bug.
    ChannelLengthMismatch {
        /// Length of the `X` (decorrelated) input slice.
        x: usize,
        /// Length of the `Y` (decorrelated) input slice.
        y: usize,
        /// Length of the `left` output slice.
        left: usize,
        /// Length of the `right` output slice.
        right: usize,
    },
    /// The adaptive IIR-predictor step was handed a history window and
    /// a parameter vector that did not agree on the predictor order.
    /// The wiki §"IIR Filtering" recurrence iterates `i = 0..order`
    /// over both the prediction window `delta[-order + i]` and the
    /// coefficient vector `par[i]`, so the two must share `len()`.
    PredictorOrderMismatch {
        /// Length of the prediction `history` window.
        history: usize,
        /// Length of the adaptive `par` coefficient vector.
        par: usize,
    },
    /// The range-coded entropy stream drove the coder into a state a
    /// well-formed stream cannot produce (an interval-register
    /// underflow, a zero radix, or an out-of-range working bit count).
    /// The payload names the primitive that tripped.
    CorruptStream(&'static str),
    /// A header, descriptor, or tail structure violated a documented
    /// layout invariant (an undersized descriptor, a non-monotonic
    /// seek table, an out-of-bounds block reference, …). The payload
    /// names the violated invariant.
    Malformed(&'static str),
    /// The file carries `total_frames == 0`, which the documented
    /// derived-quantity rules define as a non-finalised (truncated)
    /// encode and treat as an error.
    NonFinalized,
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
            Error::UnknownCompressionLabel => f.write_str(
                "oxideav-ape: compression label is outside the documented {fast, normal, high, extra high, insane} set",
            ),
            Error::NotImplemented => f.write_str(
                "oxideav-ape: feature not implemented in Phase 1 (header prefix parser only)",
            ),
            Error::ChannelLengthMismatch { x, y, left, right } => write!(
                f,
                "oxideav-ape: channel-decorrelation slice lengths disagree — x={x}, y={y}, left={left}, right={right}"
            ),
            Error::PredictorOrderMismatch { history, par } => write!(
                f,
                "oxideav-ape: IIR-predictor history/par lengths disagree on order — history={history}, par={par}"
            ),
            Error::CorruptStream(what) => {
                write!(f, "oxideav-ape: corrupt entropy stream — {what}")
            }
            Error::Malformed(what) => {
                write!(f, "oxideav-ape: malformed file structure — {what}")
            }
            Error::NonFinalized => f.write_str(
                "oxideav-ape: non-finalised file (total_frames == 0 marks a truncated encode)",
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Crate-local `Result` alias.
pub type Result<T> = core::result::Result<T, Error>;
