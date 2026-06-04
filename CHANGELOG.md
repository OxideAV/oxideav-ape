# Changelog

All notable changes to this crate are documented in this file. The
format is loosely based on [Keep a Changelog] and the crate adheres to
[Semantic Versioning] from `0.1.0` onward.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

### Added

- `Hash` derive on [`header::CompressionLevel`] and
  [`header::HeaderPrefix`] — both types can now index `HashMap` /
  `HashSet`. The compression-level Hash is paired with `Eq` such that
  every profile in `CompressionLevel::ALL` dedups into a distinct
  slot; the header-prefix Hash distinguishes any change to
  `version_raw` or `compression_level`.
- `PartialOrd` / `Ord` for [`header::CompressionLevel`] — orders by
  the raw on-wire `u16`, which is the gradient the staged docs print
  the profiles in (`Fast` 1000 → `Insane` 5000). Lets call sites sort
  the type and express "at or above `High`" predicates without
  committing to a Rust-discriminant accident.
- [`header::HeaderPrefix::major`] / [`header::HeaderPrefix::minor`] —
  one-shot accessors for the major / minor components of the decimal
  -coded version field. Equivalent to `self.version().0` /
  `self.version().1` but available so a call site that only needs one
  component skips the tuple destructure.
- `core::fmt::Display` for [`header::CompressionLevel`] — writes the
  wiki narrative's lowercase label followed by the raw decimal value
  in parentheses (e.g. `"normal (2000)"`), so a single line of
  diagnostic output identifies both the named profile and the stored
  field value.
- `core::fmt::Display` for [`header::HeaderPrefix`] — writes a
  single-line summary in the form
  `"MAC v3.92 (raw=3920) normal (2000)"`. Surfaces the raw
  `version_raw` field verbatim so an encoder that wrote a value the
  staged docs do not pin a worked example for is still
  distinguishable from a documented one with the same decomposition.
- [`header::CompressionLevel::ALL`] — `const [CompressionLevel; 5]`
  listing the documented profiles in the order the staged docs print
  them (fast → normal → high → extra high → insane). Call sites can
  iterate the documented set without committing to a particular
  Rust-side discriminant order.
- [`header::CompressionLevel::iter`] — convenience wrapper over `ALL`
  that hands back a copied iterator.
- `From<CompressionLevel> for u16` — forward conversion via the
  standard `From` trait, equivalent to `as_u16` but available as
  `u16::from(level)` / `.into()`-style coercions.
- `TryFrom<u16> for CompressionLevel` — reverse conversion via the
  standard `TryFrom` trait. Returns the existing
  `Error::UnknownCompressionLevel` for values outside the documented
  `{1000, 2000, 3000, 4000, 5000}` set.
- `core::str::FromStr for CompressionLevel` — parse a profile from
  its narrative label. Case-insensitive on the five documented
  labels — "fast", "normal", "high", "extra high", "insane" — and
  trims ASCII whitespace at both ends.
- `Error::UnknownCompressionLabel` — new variant fired by `FromStr`
  when the textual label falls outside the documented set. The
  binary `parse` path is statically forbidden from emitting this
  variant; the anti-fuzz harness rejects it explicitly.

### Tests

- `CompressionLevel::Ord` mirrors the documented ascending gradient:
  the four `<` comparisons across consecutive profiles all hold, and
  sorting an out-of-order array recovers `CompressionLevel::ALL`.
- `CompressionLevel::Hash` produces no collisions across the five
  documented profiles when inserted into a `HashMap`.
- Exhaustive `version()` decomposition: every value in `0..=u16::MAX`
  is fed through `HeaderPrefix::version()` and asserted equal to the
  arithmetic identity `(raw / 1000, (raw % 1000) / 10)`. The
  standalone `major()` / `minor()` accessors are cross-checked
  against the tuple form on every input.
- `HeaderPrefix::Hash` paired with `Eq`: equal twins dedup; differing
  fields (compression-level OR version-raw) produce distinct
  `HashSet` entries.
- Public-API integration tests for `Ord` (sort + `>= High` filter)
  and the `major()` / `minor()` accessors at the crate root.
- Single-byte-mutation coverage of the wiki worked example: every
  one-byte perturbation of the well-formed prefix
  `'MAC ' + 3920 + 2000` (8 × 255 = 2040 inputs) is asserted to
  either parse successfully or return one of `InvalidMagic`,
  `UnknownCompressionLevel`. `Truncated` is never reported for an
  8-byte buffer; neither `UnknownCompressionLabel` (that variant
  belongs to `FromStr`) nor `NotImplemented` ever leak out of `parse`
  (Phase 1 reserves the latter for the per-version tail parser the
  staged docs do not pin). Anti-fuzz harness that runs on every CI
  invocation.
- `CompressionLevel::ALL` is asserted to be ordered (1000 → 5000),
  to have length 5, and to be reachable through `iter()` more than
  once per call without state leakage.
- `TryFrom<u16>` and `From<CompressionLevel> for u16` are
  cross-checked against the inherent `from_u16` / `as_u16` pair on
  every documented profile and on a representative unknown value.
- `FromStr` parses every documented label, accepts mixed case and
  leading/trailing whitespace, rejects undocumented strings (incl.
  the empty string, a whitespace-only string, and the numeric form),
  and round-trips through `Display`'s `"label (raw)"` form.

## [0.0.1](https://github.com/OxideAV/oxideav-ape/releases/tag/v0.0.1) - 2026-05-30

### Other

- Phase 1 bootstrap: 'MAC ' magic + version + compression-level prefix parser
- Initial commit — MIT LICENSE (Karpelès Lab Inc.)

### Added

- **Phase 1 bootstrap** — clean-room scaffold for **Monkey's Audio**
  (`.ape`), per the staged docs at
  `docs/audio/ape/wiki/Monkeys_Audio.wiki`.
- 8-byte file-header prefix parser
  ([`header::HeaderPrefix::parse`]): the `'MAC '` ASCII magic at
  offset 0, the little-endian `u16` version field at offset 4, and
  the little-endian `u16` compression-level field at offset 6.
- [`header::CompressionLevel`] enum covering the five documented
  named profiles (`1000` fast / `2000` normal / `3000` high /
  `4000` extra high / `5000` insane), with `as_u16` / `from_u16` /
  `label` accessors.
- [`header::HeaderPrefix::version`] decomposes the raw decimal-coded
  version into the spec major/minor pair (worked example: `3920`
  → `(3, 92)`).
- [`header::HeaderPrefix::encode_prefix`] emits the 8-byte
  little-endian buffer that round-trips through `parse`.
- [`is_ape_magic`] — O(1) magic probe a container demuxer can use to
  route to this crate without committing to a full prefix parse.
- Crate-local `Error` / `Result` types covering the four Phase 1
  failure modes (`InvalidMagic`, `Truncated`,
  `UnknownCompressionLevel`, `NotImplemented`).
- `registry` cargo feature (default on) gating the future
  `oxideav-core` framework wire-up. `default-features = false`
  yields a standalone build with no framework dependency tree.
- 8 unit tests + 6 integration tests covering the wiki worked
  example, every documented compression level round-trip, truncation
  rejection, wrong-magic rejection, and unknown-level rejection.

### Out of scope for Phase 1

The staged docs describe the codec at the algorithm-sketch level but
do not pin the constants or per-version layouts needed for a
sample-exact decoder. The following are documented Phase 2+ inputs:

- Per-version header-tail layout (sound parameters, frame count,
  seek table, optional embedded WAV header).
- IIR-predictor coefficient tables / per-compression-level filter
  orders.
- Residual-coding `k`-parameter recurrence and its per-version
  initial-state details.
- Range-decoder frequency-table bounds and renormalisation rules.
- Channel-decorrelation reconstruction outside the documented
  `R = X - Y/2`, `L = R + Y` skeleton.
- `register!` framework wire-up and decoder factory.
