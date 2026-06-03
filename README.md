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
- IIR-predictor coefficient tables and per-compression-level filter
  orders.
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
against:

- the Monkey's Audio reference source (`MAC.exe` distribution),
- FFmpeg / `libav*`,
- any third-party `.ape` reverse-engineering writeup beyond the cited
  wiki snapshot, or
- any online resource of any kind.

Black-box validation against the reference `mac` binary remains
available as a future-round option once enough of the decoder lands
to emit comparable PCM output.

## License

MIT © 2026 Karpelès Lab Inc.
