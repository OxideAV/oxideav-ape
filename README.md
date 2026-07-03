# oxideav-ape

[![CI](https://github.com/OxideAV/oxideav-ape/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-ape/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-ape.svg)](https://crates.io/crates/oxideav-ape) [![docs.rs](https://docs.rs/oxideav-ape/badge.svg)](https://docs.rs/oxideav-ape) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust, clean-room scaffold for **Monkey's Audio** (`.ape`), the
lossless audio codec by Matthew T. Ashland distributed as the
reference binary at <http://www.monkeysaudio.com/>. Monkey's Audio
pairs channel decorrelation, a cascade of IIR predictors, and a
range-coded residual into a lossless integer-PCM round-trip.

**Phase 1** lands the 8-byte file-header prefix the staged docs at
`docs/audio/ape/wiki/Monkeys_Audio.wiki` pin:

| offset | size | field                                                     |
|-------:|-----:|-----------------------------------------------------------|
| `0x00` |    4 | `'MAC '` ASCII magic                                      |
| `0x04` |    2 | `version` (little-endian `u16`; worked example: `3920` = v3.92) |
| `0x06` |    2 | `compression_level` (little-endian `u16`)                 |

The five documented compression-level profiles (per the wiki
§"Compression levels"):

| raw value | profile      |
|----------:|--------------|
|    `1000` | `Fast`       |
|    `2000` | `Normal`     |
|    `3000` | `High`       |
|    `4000` | `ExtraHigh`  |
|    `5000` | `Insane`     |

## Quick example

```rust
use oxideav_ape::header::{CompressionLevel, HeaderPrefix};

// 'MAC ' + 3920 (v3.92, the wiki's worked example) + 2000 (normal).
let bytes = [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07];
let h = HeaderPrefix::parse(&bytes).unwrap();
assert_eq!(h.version(), (3, 92));
assert_eq!(h.compression_level, CompressionLevel::Normal);
// Phase 2+ picks up at this offset:
assert_eq!(h.header_tail_offset, 8);
// One-line diagnostic via the `Display` impl:
assert_eq!(format!("{h}"), "MAC v3.92 (raw=3920) normal (2000)");
```

The compression-level field also exposes the standard conversion
traits — `u16::from(level)` for forward conversion,
`CompressionLevel::try_from(raw)` for reverse, and
`"normal".parse::<CompressionLevel>()` for the narrative label form.
`CompressionLevel::ALL` is a `const` array carrying every documented
profile in the order the staged docs print them, for call sites that
need to walk the documented set.

`CompressionLevel` derives `Hash` and implements `Ord` / `PartialOrd`
ordered by the raw on-wire `u16` (the gradient the staged docs list
the profiles in: `Fast < Normal < High < ExtraHigh < Insane`). This
makes "is the encoder at or above the `High` profile" queries
expressible at a sort + filter, and lets the type index `HashMap` /
`HashSet`. `HeaderPrefix` derives `Hash` for the same reason. The
parsed prefix also exposes `major()` / `minor()` accessors alongside
the `version()` tuple form so a call site that only needs one
component skips the tuple destructure.

`CompressionLevel` also implements `Default` — yielding `Normal`, the
middle profile of the documented ascending gradient — so
`..Default::default()` struct-update construction lands on a
non-extremal profile.

`CompressionLevel::{as_u16, from_u16, label}` and
`HeaderPrefix::{new, version, major, minor, encode_prefix}` are all
`const fn`, so a call site can build a well-formed prefix at compile
time:

```rust
use oxideav_ape::{CompressionLevel, HeaderPrefix};

const WORKED_EXAMPLE: HeaderPrefix = HeaderPrefix::new(3920, CompressionLevel::Normal);
const ENCODED: [u8; 8] = WORKED_EXAMPLE.encode_prefix();
assert_eq!(ENCODED, [b'M', b'A', b'C', b' ', 0x50, 0x0F, 0xD0, 0x07]);
```

The crate also re-exports `FILE_EXTENSION = "ape"` (the canonical
lowercase extension the staged docs pin, without the leading dot) so
a container demuxer can match on it without re-keying the literal at
every call site.

## Stereo channel-decorrelation reconstructor

The wiki §"Channel Correlation" pins exactly one closed form for
recovering the original `(L, R)` pair from the encoder's decorrelated
`(X, Y)` pair:

```text
  R = X - Y / 2
  L = R + Y
```

The crate exposes this as a standalone primitive (no constants, no
state, no per-version branching), since the closed form is the only
algebra the staged docs commit to for the channel-decorrelation
layer. Two spellings are exposed side-by-side because the staged
docs do not disambiguate between Rust integer division (rounds
toward zero) and an arithmetic right shift (rounds toward `-∞`):

```rust
use oxideav_ape::{reconstruct_pair, reconstruct_pair_arith_shift, decorrelate_pair};

// Closed form with `Y / 2` (Rust integer division).
let (left, right) = reconstruct_pair(10, 4);
assert_eq!((left, right), (12, 8));

// Closed form with `Y >> 1` (arithmetic right shift); the two
// spellings agree for even Y and disagree for odd negative Y only.
let (l, r) = reconstruct_pair_arith_shift(10, 4);
assert_eq!((l, r), (12, 8));

// The inverse map — recover `(X, Y)` from `(L, R)`.
let (x, y) = decorrelate_pair(12, 8);
assert_eq!((x, y), (10, 4));
```

`reconstruct_block` / `reconstruct_block_arith_shift` are the
buffer-at-a-time variants and surface
`Error::ChannelLengthMismatch` if the four slices disagree on length.
All four pair-level entry points are `const fn`.

## Adaptive IIR-predictor step

The wiki §"IIR Filtering" pins a per-value recurrence for the adaptive
integer predictor: a prediction dot product `t` over the last `order`
samples, a sign-of-input adaptation of the coefficient vector `par[]`,
and `out = in + t`:

```rust
use oxideav_ape::{predict_step, predict_dot, adapt_sign};

let history = [1i32, 0, -1];
let adapt_ref = [2i32, 2, 2];
let mut par = [3i32, 4, 5];
// t = 1*3 + 0*4 + (-1)*5 = -2; in = 9 -> out = 7.
let out = predict_step(9, &history, &adapt_ref, &mut par).unwrap();
assert_eq!(out, 7);
// in > 0 -> par += adapt_ref (read AFTER the prediction).
assert_eq!(par, [5, 6, 7]);
```

`predict_step_self_ref` is the reading where the snapshot's `delta[i]`
adaptation window aliases the `delta[-order + i]` prediction window;
`predict_dot` exposes the dot product alone and `adapt_sign` the
`-1 / 0 / +1` branch selector. The prediction is read from `par[]`
**before** the sign adaptation mutates it, matching the snapshot's
statement order. A history/coefficient order disagreement surfaces
`Error::PredictorOrderMismatch`.

The wiki's trailing "correct delta[] array - different for many
versions" line is the one part of the recurrence the staged docs
explicitly decline to pin, so the step leaves the `delta[]` history
window to the caller. The residual range decoder, the `k`-parameter
recurrence, the per-version filter orders / coefficient tables, and the
cascade wiring that chains 1-3 filters remain Phase 2+ inputs (the wiki
sketches them but pins no constants).

## Range-coder residual frequency model

The wiki §"Residual Coding" pins that each residual is split into a low
part and a high part, "coding each part separately with range coder".
The clean-room extractor staged the exact cumulative symbol-frequency
table the high part is range-coded against, under
`docs/audio/ape-cleanroom/tables/`, in two version-split variants. The
crate loads both via `include_str!` + a `const` CSV parser (no numeric
literal retyped) and exposes the lookups the table shape itself
dictates:

```rust
use oxideav_ape::{counts_for_version, symbol_interval, symbol_for_cum_freq,
                  MODEL_ELEMENTS, RANGE_TOTAL_WIDTH};

// file_version < 3990 uses one model, >= 3990 the other.
let counts = counts_for_version(3920);
// Symbol 0 occupies the cumulative-frequency interval [0, 14824).
assert_eq!(symbol_interval(counts, 0), Some((0, 14824)));
// The inverse: a code value in that interval decodes back to symbol 0.
assert_eq!(symbol_for_cum_freq(counts, 14823), 0);
// The model has 64 symbols whose widths sum to the total range 65536.
assert_eq!(MODEL_ELEMENTS, 64);
assert_eq!(RANGE_TOTAL_WIDTH, 65536);
```

`COUNTS_LE3980` / `COUNTS_GE3990` are the two cumulative tables;
`FREQ_MODEL_VERSION_SPLIT` (3990) is the selector boundary; the
`powers_of_two_minus_one` bit-reader mask table is exposed as
`POWERS_OF_TWO_MINUS_ONE`.

The extractor also staged the **per-symbol frequency widths** as a
second, independently-transcribed table (`freqs_le3980.csv` /
`freqs_ge3990.csv`, drawn from a different array in the reference than
the cumulative `counts_*`). The crate ships both — `FREQS_LE3980` /
`FREQS_GE3990`, with `freqs_for_version` and `symbol_width(freqs, s)`
giving the encoder-direction width without a subtraction. Carrying the
two tables independently lets a unit test **assert they agree**
(`freqs[s] == counts[s + 1] - counts[s]` for all 64 symbols, both
version variants), a provenance cross-check the derived widths could
not provide.

The range decoder's renormalisation /
byte-input **state machine** is *not* pinned by the staged tables and
the cleanroom `spec/` narrative has not yet been authored, so it is
deliberately left to a later phase rather than guessed.

## Adaptive-filter cascade configuration

The wiki §"General Details" pins that audio data applies "1-3 IIR
filters of different order". The extractor staged the exact
`(order, shift)` pairs per compression level
(`tables/filter_config.csv`); the crate loads them via `include_str!`
and exposes `cascade_for_level`:

```rust
use oxideav_ape::{cascade_for_level, CompressionLevel};

// Fast runs no adaptive filter (order 0).
assert_eq!(cascade_for_level(CompressionLevel::Fast)[0].order, 0);
// Insane is a three-stage cascade: order 1280 (=1024+256), then 256, 16.
let insane = cascade_for_level(CompressionLevel::Insane);
assert_eq!(insane.len(), 3);
assert_eq!(insane[0].order, 1280);
```

`FilterCascade` is a fixed-capacity, no-alloc view over the same pinned
`(order, shift)` data — the form a decode path uses to read the cascade
and the aggregate quantities derived from it (total tap count, widest
stage) without a heap allocation. It is a pure reshaping of the staged
`filter_config.csv`; it introduces no control flow for *running* the
stages, which the staged tables do not pin.

```rust
use oxideav_ape::{FilterCascade, CompressionLevel, MAX_CASCADE_DEPTH};

let insane = FilterCascade::for_level(CompressionLevel::Insane);
assert_eq!(insane.len(), 3);
// Total prediction taps across all stages: 1280 + 256 + 16.
assert_eq!(insane.total_order(), 1552);
// The widest single stage — the lead 1280-tap filter.
assert_eq!(insane.max_order(), 1280);
// Fast carries one order-0 stage: present, but no adaptive filtering.
assert!(FilterCascade::for_level(CompressionLevel::Fast).is_empty());
assert_eq!(MAX_CASCADE_DEPTH, 3);
```

## Scalar constants + stage-1 order-1 prediction

The clean-room extractor staged a `scalars.csv` table of scalar
functional constants alongside the frequency model
(`docs/audio/ape-cleanroom/tables/scalars.csv`). The crate loads it
byte-for-byte via `include_str!` + a `const` name-keyed reader (no
numeric literal retyped) and exposes all seven pinned scalars as named
constants, plus the one closed form the scalar `role` text spells out
verbatim — the **stage-1 order-1 integer prediction**
`x * stage1_filter_weight >> stage1_filter_shift` (`x * 31 >> 5`):

```rust
use oxideav_ape::{stage1_predict, STAGE1_FILTER_WEIGHT, STAGE1_FILTER_SHIFT};

// 64 * 31 = 1984; 1984 >> 5 = 62.
assert_eq!(stage1_predict(64), 62);
assert_eq!(stage1_predict(64), (64 * STAGE1_FILTER_WEIGHT) >> STAGE1_FILTER_SHIFT);
// Arithmetic (floor) shift: -31 >> 5 = -1, not 0.
assert_eq!(stage1_predict(-1), -1);
```

The cleanroom workspace lists the stage-1 order-1 predictor as
in-scope; it is a stateless closed form — no per-version branching, no
`delta[]` history — distinct from the adaptive cascade recurrence. The
multiply is widened to `i64` so a full-`i32` input cannot overflow the
intermediate before the arithmetic right shift narrows it back, and the
function is `const fn`.

The full scalar set is `MODEL_ELEMENTS = 64`,
`RANGE_OVERFLOW_TOTAL_WIDTH = 65536`, `RANGE_OVERFLOW_SHIFT = 16`,
`KSUM_PIVOT_DIVISOR = 32`, `STAGE1_FILTER_WEIGHT = 31`,
`STAGE1_FILTER_SHIFT = 5`, `PREDICTOR_HISTORY_SEED = 317`. The first
three cross-check (via a unit test) against the `freq_model` module's
independently-sourced copies. `KSUM_PIVOT_DIVISOR` and
`PREDICTOR_HISTORY_SEED` are surfaced as data only — the recurrences
they feed (the `>= 3990` `k`-parameter value decode and the per-version
adaptation-window seeding) are narrative the staged tables do not pin,
so no logic is wired around them.

## Crate features

| Feature    | Default | Effect                                                                 |
|------------|:-------:|------------------------------------------------------------------------|
| `registry` | yes     | Pulls in `oxideav-core` so the crate can declare itself to the framework registry once the decoder lands. |

`default-features = false` gives a standalone build that exposes
only the file-header parser API surface and the crate-local
`Error` enum, with no framework dependency tree.

## Out of scope (Phase 3+)

The staged clean-room `tables/` pin the frequency model and the
filter cascade as functional data, but the narrative `spec/` directory
that would describe the coder's control flow has not yet been authored.
These remain out of scope until it is:

- Per-version header-tail layout (sound parameters, frame count,
  seek table, optional embedded WAV header).
- The per-compression-level **filter orders** are now pinned
  (`cascade_for_level`), but the per-version `delta[]` history
  maintenance ("correct delta[] array - different for many versions")
  that advances the prediction window between steps is not.
- Residual-coding `k`-parameter recurrence and its per-version
  initial-state details (the model the `k` low/high split feeds is
  now pinned; the recurrence that computes `k` is not).
- Range-decoder **renormalisation / byte-input state machine** (the
  frequency-table bounds are now pinned via `freq_model`; the coder's
  refill loop and the `range`-scaling that maps a code value to a
  cumulative frequency are narrative the staged tables do not commit
  to).
- Channel-decorrelation reconstruction outside the documented
  `R = X - Y/2`, `L = R + Y` skeleton.
- `register!` framework wire-up and decoder factory.

These depend on the cleanroom `spec/` (Specifier role) being authored
under `docs/audio/ape-cleanroom/spec/`.

## Clean-room wall

Two clean-room sources were consulted: the workspace-local mirror at
`docs/audio/ape/wiki/Monkeys_Audio.wiki` (a verbatim CC-BY-SA
multimedia.cx behavioural snapshot fetched 2026-05-06), and the
extractor's functional-data tables under
`docs/audio/ape-cleanroom/tables/` (`counts_le3980`, `counts_ge3990`,
`freqs_le3980`, `freqs_ge3990`, `powers_of_two_minus_one`,
`filter_config`, and `scalars`). The seven
CSV tables this crate ships under `src/tables/` are byte-for-byte copies
of those extractor files, loaded via `include_str!` so no numeric
literal is retyped. The crate deliberately
does **not** consult, quote, paraphrase, or cross-check against any
external implementation source, any reverse-engineering writeup beyond
the cited sources, or any other online resource.

Black-box validation against the reference binary remains available as a
future option once enough of the decoder lands to emit comparable PCM
output.

## License

MIT © 2026 Karpelès Lab Inc.
