# Changelog

All notable changes to this crate are documented in this file. The
format is loosely based on [Keep a Changelog] and the crate adheres to
[Semantic Versioning] from `0.1.0` onward.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

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
