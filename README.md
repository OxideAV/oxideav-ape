# oxideav-ape

[![CI](https://github.com/OxideAV/oxideav-ape/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-ape/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-ape.svg)](https://crates.io/crates/oxideav-ape) [![docs.rs](https://docs.rs/oxideav-ape/badge.svg)](https://docs.rs/oxideav-ape) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust, clean-room scaffold for **Monkey's Audio** (`.ape`), the
lossless audio codec by Matthew T. Ashland distributed as the
reference binary at <http://www.monkeysaudio.com/>. Monkey's Audio
pairs channel decorrelation, a cascade of IIR predictors, and a
range-coded residual into a lossless integer-PCM round-trip.

The crate ships every layer the staged clean-room docs pin — and, as
of the `format-reference.md` staging, that now includes the **complete
range decoder** (both version paths), the **full per-version
header/tail extraction** for both file eras, and a **vendor frame
layer validated bit-exact against reference-binary-encoded files**:
real `.ape` files parse end-to-end and their residual arrays decode
with full-payload coder consumption, verified per-frame CRCs, and
exact PCM for flag-determined silent frames. What still separates
residual arrays from PCM on non-silent frames is the adaptive
predictor pass (per-version `delta[]` maintenance, per-stage `shift`
position, decorrelation orientation) — narrative the staged docs do
not yet pin (see "Out of scope").

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

## Buffer stage runner + cascade walk (policy-injected)

The wiki §"General Decoding Process" pins that the decoder must "apply
all IIR filters onto values" per unpacked array. The `cascade` module
walks the pinned per-value recurrence over a whole buffer
(`filter_stage_decode` / `filter_stage_encode`) and chains the 1-3
pinned per-level stages (`cascade_decode` / `cascade_encode`), with
`StageState` owning each stage's sliding window + `par[]`. The two
parts the staged docs decline to pin are **injected, not guessed**:

- the per-version `delta[]` maintenance is a
  `policy(residual, filtered)` closure whose return advances the
  window — both directions hand the policy the identical pair, so
  encode/decode round-trips exactly (buffer, window, and `par[]`
  trajectory) for *any* policy;
- the staged per-stage `shift`'s position in the recurrence is not
  consumed (the wiki recurrence carries no shift), and the cascade's
  absolute stage orientation is unpinned, so the two directions are
  defined as mutual inverses (encode ascending `filter_index`, decode
  descending).

```rust
use oxideav_ape::{cascade_decode, cascade_encode, CompressionLevel,
                  FilterCascade, StageState};

let cascade = FilterCascade::for_level(CompressionLevel::High);
let mut enc = StageState::for_cascade(&cascade);
let mut dec = StageState::for_cascade(&cascade);
let mut buf = vec![100i32, -50, 25, 0, 7];
let orig = buf.clone();
// Any policy round-trips; the raw residual is the most literal reading.
cascade_encode(&mut buf, &mut enc, |_stage, r, _f| r).unwrap();
cascade_decode(&mut buf, &mut dec, |_stage, r, _f| r).unwrap();
assert_eq!(buf, orig);
```

## Frame pipeline (§"General Decoding Process")

`decode_frame` wires the pinned stage ordering verbatim — unpack every
channel's delta array (channel 0, then 1), apply the caller's filter
walk per array, then (stereo only) reconstruct `(L, R)` from the
filtered `(X, Y)` pair. The unpinned entropy layer (range-decoder
state machine + `k`-parameter recurrence) enters as the `DeltaSource`
trait boundary, with `DeltaSink` + `encode_frame` as the encoder
mirror, so the later phase that pins the coder plugs in without
reshaping the frame walk. `FrameChannels` carries the wiki's
mono/stereo distinction; `CorrelationRounding` carries the documented
`Y/2`-vs-`Y>>1` ambiguity (each spelling pairs losslessly with its own
inverse — `decorrelate_pair_arith_shift` completes the pair). The
crate fixes **array 0 = X, array 1 = Y** as a local convention pending
spec. End-to-end PCM round-trips are tested across all five pinned
level cascades × both roundings, including cross-frame filter-state
carry.

## Stream configuration (version/level dispatch)

`StreamConfig` performs the two dispatches the staged material pins,
once, from one parsed header prefix: the frequency-model version split
(`< 3990` vs `>= 3990`) and the per-level filter cascade.

```rust
use oxideav_ape::StreamConfig;

// 'MAC ' + 3920 (v3.92) + 2000 (normal).
let cfg = StreamConfig::from_bytes(b"MAC \x50\x0F\xD0\x07").unwrap();
assert!(!cfg.uses_ge3990_model()); // 3920 < 3990 -> older model
assert_eq!(cfg.cascade().len(), 1); // normal: one order-16 stage
assert_eq!(cfg.counts()[64], 65536); // version-selected counts table
```

## Range decoder + residual entropy codec

The staged `docs/audio/ape/format-reference.md` §2 pins the carryless
byte-oriented range coder end-to-end, and the `range_coder` +
`entropy` modules implement it: the §2.1 constants
(`TOP_VALUE`/`BOTTOM_VALUE`/`SHIFT_BITS`/`EXTRA_BITS`), the §2.2
word-addressed byte input, the §2.3 renormalisation with the 9-bit
carry window (`(buffer >> 1) & 0xFF`), the §2.4 decode primitives, the
§2.5 overflow-symbol lookup against the version-split frequency
models, the §2.6 `>= 3990` pivot/overflow decode (with the
16-bit-ceiling two-division base split), the §2.7 `< 3990`
adaptive-`k` decode (with the 3910 wide-`k` split and the 5-bit
escape), the §2.8 `KSum` recurrence + `K_SUM_MIN_BOUNDARY` ladder, and
the §2.9 signed unfold. Crate-derived encoder mirrors
(`RangeEncoder` / `ResidualEncoder`) exercise every branch in
round-trip tests across all version boundaries.

```rust
use oxideav_ape::entropy::{EntropyInit, ResidualDecoder, ResidualEncoder};

let init = EntropyInit { k: 10, ksum: 16384 };
let mut enc = ResidualEncoder::new(3990, init);
for r in [0i32, -5, 1234, -100_000] {
    enc.encode_residual(r).unwrap();
}
let bytes = enc.finish();
let mut dec = ResidualDecoder::new(&bytes, 3990, init);
assert_eq!(dec.decode_residual().unwrap(), 0);
assert_eq!(dec.decode_residual().unwrap(), -5);
assert_eq!(dec.decode_residual().unwrap(), 1234);
assert_eq!(dec.decode_residual().unwrap(), -100_000);
```

## Per-version header/tail extraction

`file_header::FileInfo::parse` walks both header eras the staged
reference pins: the `>= 3980` descriptor + header split (alignment
gap, forward-compat length skips, 64-bit frame-data count, MD5, seek
table, stored WAV header blob) and the `< 3980` flat header with the
pinned inline tail order (peak level → seek-element count → WAV blob →
seek byte table → the `<= 3800` seek *bit* table), plus the §1.4
blocks-per-frame derivation, the §1.6 flag-derived bits-per-sample,
and the §1.7 derived quantities. A junk prefix (e.g. a leading ID3v2
tag) is scanned past and recorded.

## Vendor frame layer + whole-file decoder

`frame` + `decoder` turn seek-table entries into decoded frames. The
staged reference marks frame priming and per-frame state reset as GAPs,
so this layer was established **black-box** against reference-binary
(v13.18) encoded files and is validated bit-exact by the committed
fixtures: the whole audio region is one bit array of 32-bit words
loaded little-endian and consumed MSB-first; each frame starts at its
seek offset with a CRC/flags prologue read through that array
(`crc32(frame PCM) >> 1`, bit 31 = flags present; silence +
pseudo-stereo bits), one structural pad byte, then the coder primes
and decodes with per-frame per-channel `KSum = 16384` — stereo frames
interleave the two arrays per sample with independent running states.

```rust,no_run
use oxideav_ape::decoder::{ApeDecoder, FrameDecode};

let data = std::fs::read("music.ape").unwrap();
let dec = ApeDecoder::new(&data).unwrap();
for i in 0..dec.frame_count() {
    match dec.decode_frame(i).unwrap() {
        FrameDecode::Pcm(channels) => { /* exact PCM (silent frames) */ }
        FrameDecode::Residuals(out) => { /* entropy-layer arrays; PCM awaits the predictor docs */ }
    }
}
```

`FrameDeltaSource` adapts a decoded frame onto the pipeline's
`DeltaSource` boundary, so the pinned §"General Decoding Process" walk
runs over real entropy output.

## Scalar constants + pinned closed forms

The clean-room extractor staged a `scalars.csv` table of scalar
functional constants alongside the frequency model
(`docs/audio/ape-cleanroom/tables/scalars.csv`). The crate loads it
byte-for-byte via `include_str!` + a `const` name-keyed reader (no
numeric literal retyped) and exposes all seven pinned scalars as named
constants, plus the two closed forms the scalar `role` text spells out
verbatim — the **stage-1 order-1 integer prediction**
`x * stage1_filter_weight >> stage1_filter_shift` (`x * 31 >> 5`) and
the `>= 3990` value-decode **`KSum` pivot** `max(ksum / 32, 1)`:

```rust
use oxideav_ape::{ksum_pivot, stage1_predict, STAGE1_FILTER_WEIGHT, STAGE1_FILTER_SHIFT};

// 64 * 31 = 1984; 1984 >> 5 = 62.
assert_eq!(stage1_predict(64), 62);
assert_eq!(stage1_predict(64), (64 * STAGE1_FILTER_WEIGHT) >> STAGE1_FILTER_SHIFT);
// Arithmetic (floor) shift: -31 >> 5 = -1, not 0.
assert_eq!(stage1_predict(-1), -1);
// KSum pivot: max(ksum / 32, 1) — floored at 1, so always divisible-by.
assert_eq!(ksum_pivot(0), 1);
assert_eq!(ksum_pivot(96), 3);
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
independently-sourced copies. `PREDICTOR_HISTORY_SEED` is surfaced as
data only — the per-version adaptation-window seeding it feeds is
narrative the staged tables do not pin. The `KSum` recurrence *around*
`ksum_pivot` (accumulation across decoded values; the pivot-driven
split into range-coded parts) is likewise unpinned, so only the pivot's
own closed form is wired.

## Crate features

| Feature    | Default | Effect                                                                 |
|------------|:-------:|------------------------------------------------------------------------|
| `registry` | yes     | Pulls in `oxideav-core` so the crate can declare itself to the framework registry once the decoder lands. |

`default-features = false` gives a standalone build that exposes
only the file-header parser API surface and the crate-local
`Error` enum, with no framework dependency tree.

## Out of scope (pending further staged docs)

The predictor pass between residual arrays and PCM is the remaining
unstaged narrative:

- The per-version `delta[]` history maintenance ("correct delta[]
  array - different for many versions"): the cascade runner injects it
  as a policy closure, but the *actual* per-version rule is unpinned.
- Where the pinned per-stage `shift` enters the recurrence (the wiki
  recurrence carries no shift), the cascade's absolute stage
  orientation, and how the stage-1 `x*31>>5` / `317`-seed predictor
  composes with the adaptive stages.
- The decorrelation orientation on real streams: which coded array is
  the correlation's `X` vs `Y` (empirically the difference-type array
  is coded first), the sign convention, and the divide-vs-shift
  rounding (all carried as parameters).
- 24-bit and ≥ 3-channel sample reassembly (a staged-reference GAP).
- The old-era (`< 3980`) frame-level entropy init: the current vendor
  encoder emits 3990-era streams only, so the `k` init cannot be
  exercised black-box; `FRAME_K_INIT = 10` is the ladder-consistent
  value.
- The `cFileMD5` coverage region (a staged-reference GAP; not needed
  for decode).
- `register!` framework wire-up and decoder factory (lands with
  non-silent real-file PCM).

## Clean-room wall

Three clean-room sources were consulted: the staged format reference
at `docs/audio/ape/format-reference.md` (range coder + header/tail),
the workspace-local mirror at `docs/audio/ape/wiki/Monkeys_Audio.wiki`
(a verbatim CC-BY-SA multimedia.cx behavioural snapshot fetched
2026-05-06), and the extractor's functional-data tables under
`docs/audio/ape-cleanroom/tables/` (`counts_le3980`, `counts_ge3990`,
`freqs_le3980`, `freqs_ge3990`, `powers_of_two_minus_one`,
`filter_config`, and `scalars`). The CSV tables this crate ships under
`src/tables/` are byte-for-byte copies of those extractor files (plus
`ksum_min_boundary.csv`, transcribed from the staged format
reference), loaded via `include_str!` so no numeric literal is
retyped. The crate deliberately does **not** consult, quote,
paraphrase, or cross-check against any external implementation source,
any reverse-engineering writeup beyond the cited sources, or any other
online resource.

Black-box validation against the reference **binary** (console
encoder v13.18, invoked as an opaque tool over engineered PCM inputs)
established the frame-layout facts the staged reference marks as GAPs
and produced the committed `tests/fixtures/*.ape`; the binary's source
was never consulted.

## License

MIT © 2026 Karpelès Lab Inc.
