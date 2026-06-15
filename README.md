# oxideav-ape

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

## Crate features

| Feature    | Default | Effect                                                                 |
|------------|:-------:|------------------------------------------------------------------------|
| `registry` | yes     | Pulls in `oxideav-core` so the crate can declare itself to the framework registry once the decoder lands. |

`default-features = false` gives a standalone build that exposes
only the file-header parser API surface and the crate-local
`Error` enum, with no framework dependency tree.

## Out of scope for Phase 1

The wiki snapshot describes the codec at the algorithm-sketch level
but does not pin the constants or per-version layouts a sample-exact
decoder needs:

- Per-version header-tail layout (sound parameters, frame count,
  seek table, optional embedded WAV header).
- IIR-predictor coefficient tables, per-compression-level filter
  orders, and the per-version `delta[]` history maintenance ("correct
  delta[] array - different for many versions") that advances the
  prediction window between steps.
- Residual-coding `k`-parameter recurrence and its per-version
  initial-state details.
- Range-decoder frequency-table bounds and renormalisation rules.
- Channel-decorrelation reconstruction outside the documented
  `R = X - Y/2`, `L = R + Y` skeleton.
- `register!` framework wire-up and decoder factory.

These are documented Phase 2+ inputs that depend on additional
clean-room reference material being staged under `docs/audio/ape/`.

## Clean-room wall

Only the workspace-local mirror at
`docs/audio/ape/wiki/Monkeys_Audio.wiki` (a verbatim CC-BY-SA
multimedia.cx snapshot fetched 2026-05-06) was consulted. The crate
deliberately does **not** consult, quote, paraphrase, or cross-check
against any reference codec source, any third-party reimplementation,
any `.ape` reverse-engineering writeup beyond the cited wiki snapshot,
or any other online resource.

Black-box validation against the reference binary remains available as a
future option once enough of the decoder lands to emit comparable PCM
output.

## License

MIT © 2026 Karpelès Lab Inc.
