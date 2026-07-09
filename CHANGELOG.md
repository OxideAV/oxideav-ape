# Changelog

All notable changes to this crate are documented in this file. The
format is loosely based on [Keep a Changelog] and the crate adheres to
[Semantic Versioning] from `0.1.0` onward.

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

## [0.0.2](https://github.com/OxideAV/oxideav-ape/compare/v0.0.1...v0.0.2) - 2026-07-07

### Other

- README — StreamConfig dispatch section with worked example
- StreamConfig — one-shot version/level dispatch from the parsed header prefix
- docs refresh — status overview, cascade/pipeline README sections, rewritten out-of-scope list
- hardening sweep — wrapping correlation algebra + exhaustive inverse/header sweeps
- pipeline module — General Decoding Process frame walk over a DeltaSource boundary
- cascade module — buffer stage runner + five-level cascade walk, policy-injected
- residual_step — encoder-direction inverse of the pinned predictor step
- wire ksum_pivot — the second closed form the scalars.csv role text pins
- add CI / crates.io / docs.rs / MIT-license badges
- FilterCascade aggregate view over pinned (order, shift) cascade data
- ship per-symbol freq width tables (freqs_le3980/ge3990) + provenance cross-check
- scalars module — pinned scalar constants + stage-1 order-1 integer prediction
- range-coder frequency model + per-level filter cascade from cleanroom tables
- refresh to current status, drop per-round changelog cruft
- Phase 1: adaptive IIR-predictor per-value step (wiki §"IIR Filtering")
- Phase 1: stereo channel-decorrelation reconstructor (wiki §"Channel Correlation")
- drop release-plz.toml — use release-plz defaults across the workspace
- Phase 1 depth: const-fn accessors + HeaderPrefix::new + Default + FILE_EXTENSION
- Phase 1 depth: Hash + Ord on CompressionLevel, major/minor accessors
- Phase 1 ergonomics: CompressionLevel::ALL + standard conversion traits
- Phase 1 polish: Display impls + single-byte-mutation coverage
- release v0.0.1

### Added

- **`StreamConfig` version/level dispatch** (new [`config`] module) —
  performs the two dispatches the staged material pins, once, from one
  parsed `HeaderPrefix`: the frequency-model version split
  (`counts`/`freqs` pair, `< 3990` vs `>= 3990`) and the per-level
  filter cascade. `from_header` / `from_bytes` constructors,
  `counts()` / `freqs()` / `cascade()` / `uses_ge3990_model()`
  accessors. No new constants and no new control flow beyond composing
  the two existing pinned selectors. 5 unit tests (worked-example
  selection, exact 3990 boundary, all-level cascades, error
  passthrough, counts/freqs pairing invariant; lib suite 130 → 135).

### Changed

- Documentation refresh: README gains a status overview plus sections
  for the cascade runner and frame pipeline; the "Out of scope" list
  is rewritten against the current surface (what is pinned vs. what
  waits on the unauthored cleanroom `spec/`); crate `description` and
  lib-level docs updated to match. No code changes.

### Added

- **Hostile-input hardening + exhaustive sweeps** — the four
  channel-correlation closed forms now use wrapping (mod 2^32)
  arithmetic, so `i32`-extreme decorrelated pairs cannot panic a debug
  build; the wrapping algebra adds and subtracts the identical `Y/2`
  term, making each spelling's decorrelate/reconstruct pair the exact
  identity for **every** input (no parity condition — the
  `decorrelate_pair` doc previously understated this). New tests: full
  `i32`-extreme round-trips both spellings, shift-spelling identity on
  odd negative `Y`, cross-spelling divergence characterisation
  (off-by-one exactly on odd negative `Y`), an exhaustive
  `symbol_for_cum_freq` inverse over all 65536 cumulative-frequency
  values × both tables (validated against an independent linear walk),
  and a `tests/frame_pipeline.rs` integration suite through the public
  re-export surface only: five-level stereo round-trip under a policy
  composed from the crate's own pinned closed forms
  (`stage1_predict` + `ksum_pivot`), cross-frame filter-state carry
  (with a fresh-state divergence counter-check), source-error
  propagation, and an exhaustive all-`u16` compression-level header
  sweep (exactly 5 accepted). Lib suite 126 → 130, +4 integration.

- **Frame-decode orchestrator** (new [`pipeline`] module) — wires the
  wiki §"General Decoding Process" stage ordering verbatim:
  `decode_frame` unpacks every channel's delta array (channel 0, then
  1), applies the caller's *"apply all IIR filters"* walk per array,
  then — stereo only — reconstructs `(L, R)` from the filtered
  `(X, Y)` pair. The unpinned entropy layer enters as the
  `DeltaSource` trait boundary (with `DeltaSink` as the encoder
  mirror), so whichever later phase pins the range decoder plugs in
  without reshaping the frame walk. `encode_frame` is the mirror walk
  (correlate → filter → pack); `FrameChannels` (mono/stereo) and
  `CorrelationRounding` (`Y/2` vs `Y>>1`, the documented ambiguity)
  parameterize the shape. Crate-local convention pending spec: array 0
  = `X`, array 1 = `Y`. Also adds `decorrelate_pair_arith_shift` (the
  exact inverse of `reconstruct_pair_arith_shift`, completing the
  spelling pair). 8 unit tests: stage-ordering trail, X/Y convention,
  rounding divergence on odd negative `Y`, error propagation, mono +
  stereo end-to-end round-trips across **all five pinned level
  cascades × both roundings** (lib suite 118 → 126).

- **Buffer-at-a-time stage runner + cascade walk** (new [`cascade`]
  module) — `filter_stage_decode` / `filter_stage_encode` apply the
  pinned per-value recurrence (aliased-window reading) over a whole
  residual buffer, and `cascade_decode` / `cascade_encode` chain the
  1-3 pinned per-level stages ("apply all IIR filters onto values"),
  defined as mutual inverses (encode walks `filter_index` ascending,
  decode descending) because the absolute orientation is unpinned.
  `StageState` owns the sliding window + `par[]` per stage (`zeroed`,
  `for_stage`, `for_cascade`, `with_initial`). The two unpinned parts
  are **injected, not guessed**: the per-version `delta[]` maintenance
  is a `policy(residual, filtered)` closure whose return advances the
  window — both directions hand the policy the identical pair, so
  encode/decode round-trips exactly for any policy — and the staged
  per-stage `shift`'s position in the recurrence is not consumed (the
  wiki recurrence carries no shift). 9 unit tests: order-0 identity,
  manual-loop anchor, PRNG-policy round-trips (orders 1/2/16/32),
  identical policy pairs both directions, full five-level cascade
  round-trip, stage-index dispatch order, fast-level no-op, mismatch,
  geometry (lib suite 109 → 118).

### Changed

- `predict_dot` accumulates with **wrapping** `i64` addition and the
  step's `out = in + t` narrows via `wrapping_add` — a single product
  is `<= 2^62` so realistic magnitudes never wrap, but an adversarial
  full-range buffer at the pinned order-1280 stage could previously
  panic a debug build. Wrapping keeps the forward/inverse steps exact
  mutual inverses across a wrapped accumulator.

- **Encoder-direction predictor-step inverse** ([`predictor`] module) —
  `residual_step` / `residual_step_self_ref`, the algebraic inverse of
  the pinned wiki §"IIR Filtering" per-value recurrence: `in = out - t`
  read against the same pre-adaptation `par[]`, followed by the
  identical sign-of-`in` adaptation. Introduces no new bitstream
  semantics (the pinned recurrence solved for the other variable) and
  gives the step pair an exact round-trip — samples *and* `par[]`
  trajectory — under **any** caller-supplied history policy, including
  across wrapping `i32` narrows. Adds 7 `predictor` unit tests (worked
  example inverse, recovered-sign adaptation, PRNG round-trip over
  orders 1/2/4/16 × 256 steps, wrap-exactness, order-0 identity,
  mismatch, self-ref aliasing; lib suite 102 → 109).

- **`ksum_pivot` closed form** ([`scalars`] module) — wires the second
  (and last) closed form the extractor's `scalars.csv` `role` text
  spells out verbatim: the `>= 3990` value-decode `KSum` pivot
  `max(KSum / KSUM_PIVOT_DIVISOR, 1)` (`max(ksum / 32, 1)`), as a
  `const fn` over a `u64` accumulator. The floor at `1` makes the pivot
  safe to divide by unconditionally. The surrounding recurrence — how
  `KSum` accumulates across decoded values and how the pivot splits a
  value into range-coded parts — remains unpinned narrative awaiting the
  cleanroom `spec/`, so only the pivot map itself is wired. Adds 5
  `scalars` unit tests (closed-form sweep incl. `u64::MAX`, floor
  region, never-zero, monotonicity, const-evaluability; lib suite
  97 → 102).

- **`FilterCascade` aggregate view** ([`filter_config`] module) — a
  fixed-capacity, no-alloc view over the same pinned `(order, shift)`
  cascade data `cascade_for_level` returns as a `Vec`. `for_level`
  gathers the 1-3 stages in `filter_index` application order into an
  inline buffer sized by the new `MAX_CASCADE_DEPTH = 3` constant; the
  view exposes `level()`, `stages()`, `len()`, `is_empty()` (true only
  for the fast / `1000` no-filter level's single `order == 0` stage),
  `stage(i)`, `total_order()` (the summed prediction-tap count a decode
  path uses to size its combined history window — `1280 + 256 + 16 =
  1552` for insane), and `max_order()` (the widest single stage). It is
  a pure reshaping of the Extractor-staged `filter_config.csv`: it
  introduces no control flow for *running* the stages over a residual
  buffer, which the staged tables do not pin. Adds 6 `filter_config`
  unit tests, cross-checking the inline view against the allocating
  `cascade_for_level` form (lib suite 91 → 97).
- **Per-symbol frequency width tables + cross-check** ([`freq_model`]
  module) — ships the clean-room extractor's two independently-staged
  per-symbol width tables (`freqs_le3980.csv` for `file_version < 3990`,
  `freqs_ge3990.csv` for `>= 3990`), copied byte-for-byte under
  `src/tables/` and parsed by the existing `const` CSV reader so no
  numeric literal is retyped. Exposes `FREQS_LE3980` / `FREQS_GE3990`,
  the `freqs_for_version` selector, and `symbol_width(freqs, s)` — the
  encoder-direction width lookup without a subtraction. These widths are
  transcribed from a different reference array than the cumulative
  `counts_*` tables, so the module now **asserts the two
  independently-extracted tables agree** (`freqs[s] == counts[s + 1] -
  counts[s]` for all 64 symbols, both version variants), and that each
  width table sums to `RANGE_TOTAL_WIDTH = 65536` — a provenance
  cross-check the previously-derived widths could not provide. Adds 7
  `freq_model` unit tests (lib suite 84 → 91). The range decoder's
  renormalisation / byte-input state machine remains out of scope: it is
  narrative the staged `tables/` do not pin and the cleanroom `spec/`
  directory has not yet been authored.
- **Scalar constants + stage-1 order-1 integer prediction**
  ([`scalars`] module) — loads the extractor's `scalars.csv` table
  (`docs/audio/ape-cleanroom/tables/`) byte-for-byte under `src/tables/`
  via `include_str!` + a `const` name-keyed CSV reader, so no numeric
  literal is retyped. Exposes all seven pinned scalars as named
  constants — `MODEL_ELEMENTS = 64`, `RANGE_OVERFLOW_TOTAL_WIDTH =
  65536`, `RANGE_OVERFLOW_SHIFT = 16`, `KSUM_PIVOT_DIVISOR = 32`,
  `STAGE1_FILTER_WEIGHT = 31`, `STAGE1_FILTER_SHIFT = 5`,
  `PREDICTOR_HISTORY_SEED = 317` — and ships the one closed form the
  scalar `role` text spells out verbatim: the stage-1 order-1 integer
  prediction `stage1_predict(x) = x * 31 >> 5` (arithmetic shift, `i64`
  intermediate, `const fn`). The cleanroom README lists the stage-1
  order-1 predictor as in-scope; it is a stateless closed form, distinct
  from the adaptive cascade recurrence. `KSUM_PIVOT_DIVISOR` and
  `PREDICTOR_HISTORY_SEED` are surfaced as data only — the recurrences
  they feed (the `>= 3990` `k`-parameter value decode and the
  per-version adaptation-window seeding) are narrative the staged tables
  do not pin, so no logic is wired around them. A cross-check test
  asserts the three shared bounds agree with the [`freq_model`] module's
  independently-sourced copies.
- **Range-coder residual frequency model** ([`freq_model`] module) —
  loads the two version-split cumulative symbol-frequency tables the
  clean-room extractor staged under `docs/audio/ape-cleanroom/tables/`
  (`counts_le3980` for `file_version < 3990`, `counts_ge3990` for
  `>= 3990`) plus
  the `powers_of_two_minus_one` bit-reader mask table. The four CSVs
  ship under `src/tables/` (byte-for-byte copies of the extractor
  files) and are parsed at compile time by a `const` CSV reader, so no
  numeric literal is retyped. Exposes `COUNTS_LE3980` / `COUNTS_GE3990`,
  the `counts_for_version` selector (boundary `FREQ_MODEL_VERSION_SPLIT
  = 3990`), the `symbol_interval` (symbol → `[low, width)`) and
  `symbol_for_cum_freq` (cumulative-frequency → symbol, binary search)
  lookups the table shape dictates, and the scalar bounds
  `MODEL_ELEMENTS = 64`, `RANGE_TOTAL_WIDTH = 65536`,
  `RANGE_OVERFLOW_SHIFT = 16`. The range decoder's renormalisation /
  byte-input state machine is *not* shipped — it is narrative the
  staged tables do not pin and the cleanroom `spec/` is not yet
  authored.
- **Adaptive-filter cascade configuration** ([`filter_config`] module)
  — loads the per-compression-level `(order, shift)` cascade from
  `tables/filter_config.csv` (also `include_str!`-loaded under
  `src/tables/`). Exposes `FILTER_STAGES`, the `FilterStage` struct, and
  `cascade_for_level`, which returns the 1-3 stages for a level in
  application order: fast `1000` runs a single `order == 0` (no-filter)
  stage; insane `5000` runs a three-stage `1280 (=1024+256) / 256 / 16`
  cascade.
- **Adaptive IIR-predictor per-value step** ([`predictor`] module) —
  transcribes the per-value recurrence the staged wiki §"IIR Filtering"
  pins: the order-`N` prediction dot product `t`, the sign-of-input
  adaptation of the coefficient vector `par[]`, and `out = in + t`.
  Exposes [`predictor::predict_step`] (explicit adaptation-reference
  window), [`predictor::predict_step_self_ref`] (adaptation window
  aliased to the prediction history — the reading where the snapshot's
  `delta[i]` and `delta[-order + i]` denote the same samples),
  [`predictor::predict_dot`] (the dot product alone), and
  [`predictor::adapt_sign`] (the `-1 / 0 / +1` branch selector). The
  step reads the prediction from `par[]` **before** the sign adaptation
  mutates it, matching the snapshot's statement order. All four are
  re-exported at the crate root. The trailing "correct delta[] array -
  different for many versions" history-maintenance line is the one part
  of the recurrence the staged docs decline to pin, so the step leaves
  the `delta[]` window to the caller rather than guessing the unpinned
  per-version ring update. Arithmetic is `i64`-accumulated with a
  wrapping narrow / wrapping `par[]` update so full-`i32` extremes do
  not panic in debug builds.
- `Error::PredictorOrderMismatch { history, par }` — surfaces a
  caller-side bug where the IIR-predictor step's history window and
  coefficient vector disagree on the predictor order. The binary
  `parse` path is statically forbidden from emitting this variant; the
  header anti-fuzz harness rejects it explicitly.
- **Stereo channel-decorrelation reconstructor**
  ([`decorrelate`] module) — implements the closed form the staged
  wiki §"Channel Correlation" pins:
  ```text
    R = X - Y / 2
    L = R + Y
  ```
  Exposes [`decorrelate::reconstruct_pair`] (Rust integer division)
  and [`decorrelate::reconstruct_pair_arith_shift`] (arithmetic right
  shift) side-by-side because the staged docs do not disambiguate
  which rounding the reference encoder uses; the two spellings agree
  for even `Y` and disagree for odd negative `Y` only. The inverse
  map [`decorrelate::decorrelate_pair`] recovers `(X, Y)` from
  `(L, R)`. Buffer-at-a-time helpers
  [`decorrelate::reconstruct_block`] /
  [`decorrelate::reconstruct_block_arith_shift`] take pre-allocated
  output slices and surface
  [`Error::ChannelLengthMismatch`] on input/output length disagreement.
  All five entry points are re-exported at the crate root; the four
  pair-level forms are `const fn`.
- `Error::ChannelLengthMismatch { x, y, left, right }` — surfaces a
  caller-side bug where the four slices handed to the block
  reconstructor disagree on length. Carries every length in the
  display message so the diagnostic identifies the mismatch
  unambiguously. The binary `parse` path is statically forbidden from
  emitting this variant; the anti-fuzz harness rejects it explicitly.
- [`header::HeaderPrefix::new`] — `const fn` constructor taking a
  raw decimal-coded version and a typed [`header::CompressionLevel`].
  Pins `header_tail_offset` to [`header::HEADER_PREFIX_LEN`] (8) so
  call sites that build a prefix through the constructor cannot
  accidentally drift the Phase 2 boundary.
- [`header::FILE_EXTENSION`] — `&str` constant carrying the canonical
  lowercase file extension the staged docs pin (`"ape"`, no leading
  dot). Re-exported at the crate root.
- `Default for CompressionLevel` — yields
  [`CompressionLevel::Normal`], the middle profile of the documented
  ascending raw-value gradient. Lets `..Default::default()`
  struct-update construction land on a non-extremal profile.
- [`header::CompressionLevel::as_u16`],
  [`header::CompressionLevel::from_u16`], and
  [`header::CompressionLevel::label`] are now `const fn` — usable in
  `const` contexts (e.g. building static lookup tables).
- [`header::HeaderPrefix::version`],
  [`header::HeaderPrefix::major`], [`header::HeaderPrefix::minor`],
  and [`header::HeaderPrefix::encode_prefix`] are now `const fn` —
  usable in `const` contexts.
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
