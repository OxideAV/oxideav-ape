//! Stereo channel-decorrelation reconstructor.
//!
//! The wiki snapshot at `docs/audio/ape/wiki/Monkeys_Audio.wiki`
//! §"Channel Correlation" pins exactly one closed-form expression for
//! recovering the original left/right pair `(L, R)` from the encoder's
//! decorrelated pair `(X, Y)`:
//!
//! ```text
//!   R = X - Y / 2
//!   L = R + Y
//! ```
//!
//! That is the entire algebraic recipe the staged docs commit to. The
//! division is by 2 and the encoder operates on integer PCM samples, so
//! the natural in-tree carrier type is `i32`. The staged docs do
//! **not** disambiguate between
//!
//! * "divide and round toward zero" (Rust `Y / 2`) and
//! * "arithmetic right shift" (`Y >> 1`, round toward `-∞` on a
//!   two's-complement machine).
//!
//! The two paths agree for every even `Y` and for every non-negative
//! `Y`, and disagree for odd negative `Y` only.
//!
//! Both are present in the reference-implementation lineage of
//! lossless-audio codecs depending on era; the wiki page is silent.
//! Phase 1 therefore exposes **both** spellings as sibling primitives
//! and lets the caller pick the one the per-version trace eventually
//! pins, rather than locking in a guess that a later trace might have
//! to retract.
//!
//! Both spellings are pure arithmetic with no further dependencies, so
//! the surface here is genuinely the minimum the staged docs commit
//! to. Everything upstream of the `(X, Y)` pair — the IIR predictor
//! cascade, the residual range decoder, the `k`-parameter recurrence
//! — is **out of scope for Phase 1** (the staged docs sketch it but
//! pin no constants).

use crate::error::{Error, Result};

/// Per-sample reconstruction of `(L, R)` from the encoder's
/// decorrelated `(X, Y)` pair using **Rust integer division**
/// (rounds toward zero). The closed form is the one the staged docs
/// pin at §"Channel Correlation":
///
/// ```text
///   R = X - Y / 2
///   L = R + Y
/// ```
///
/// Returns `(left, right)` in left-then-right order, matching the
/// channel order the staged docs use in their narrative.
///
/// This spelling is the one a reader who took the wiki narrative
/// **literally** as Rust source would arrive at. See
/// [`reconstruct_pair_arith_shift`] for the alternative spelling that
/// uses an arithmetic right shift, which differs for odd negative `Y`.
///
/// `const fn` so the reconstructor is usable in `const` contexts (e.g.
/// building a static reference vector for a future black-box check).
///
/// All arithmetic is **wrapping**: for every PCM-range input the sums
/// never wrap, but a hostile decorrelated pair near the `i32` extremes
/// must not be able to panic a debug build, and the wrapping (mod
/// 2^32) algebra is exactly what keeps each spelling a lossless
/// inverse of its decorrelation twin even across a wrap.
#[inline]
pub const fn reconstruct_pair(x: i32, y: i32) -> (i32, i32) {
    let r = x.wrapping_sub(y / 2);
    let l = r.wrapping_add(y);
    (l, r)
}

/// Per-sample reconstruction of `(L, R)` from the encoder's
/// decorrelated `(X, Y)` pair using an **arithmetic right shift** in
/// place of the division.
///
/// ```text
///   R = X - (Y >> 1)         // arithmetic shift; rounds toward -∞
///   L = R + Y
/// ```
///
/// Differs from [`reconstruct_pair`] for odd negative `Y` only: Rust
/// integer division rounds toward zero, an arithmetic right shift
/// rounds toward `-∞`. For example with `Y = -3`:
///
/// * `Y / 2 == -1` (Rust integer division)
/// * `Y >> 1 == -2` (arithmetic right shift)
///
/// The staged wiki snapshot does not pin which spelling the
/// reference encoder uses; both are exposed so a future per-version
/// trace can adopt the one that round-trips its fixtures without
/// retracting an earlier API choice.
///
/// `const fn` so the reconstructor is usable in `const` contexts.
#[inline]
pub const fn reconstruct_pair_arith_shift(x: i32, y: i32) -> (i32, i32) {
    let r = x.wrapping_sub(y >> 1);
    let l = r.wrapping_add(y);
    (l, r)
}

/// Reconstruct a full block of `(X, Y)` samples in-place / fan-out
/// into pre-allocated left + right buffers, using
/// [`reconstruct_pair`]. The three slices must agree on length; a
/// length mismatch surfaces [`Error::ChannelLengthMismatch`].
///
/// `x[i]` and `y[i]` are read in lockstep; `left[i]` and `right[i]`
/// receive the reconstructed pair per the closed form.
pub fn reconstruct_block(x: &[i32], y: &[i32], left: &mut [i32], right: &mut [i32]) -> Result<()> {
    if x.len() != y.len() || x.len() != left.len() || x.len() != right.len() {
        return Err(Error::ChannelLengthMismatch {
            x: x.len(),
            y: y.len(),
            left: left.len(),
            right: right.len(),
        });
    }
    for i in 0..x.len() {
        let (l, r) = reconstruct_pair(x[i], y[i]);
        left[i] = l;
        right[i] = r;
    }
    Ok(())
}

/// Same as [`reconstruct_block`] but uses
/// [`reconstruct_pair_arith_shift`] per sample. Provided so a future
/// per-version trace can plug the arithmetic-shift spelling in at the
/// block level without forking the call site.
pub fn reconstruct_block_arith_shift(
    x: &[i32],
    y: &[i32],
    left: &mut [i32],
    right: &mut [i32],
) -> Result<()> {
    if x.len() != y.len() || x.len() != left.len() || x.len() != right.len() {
        return Err(Error::ChannelLengthMismatch {
            x: x.len(),
            y: y.len(),
            left: left.len(),
            right: right.len(),
        });
    }
    for i in 0..x.len() {
        let (l, r) = reconstruct_pair_arith_shift(x[i], y[i]);
        left[i] = l;
        right[i] = r;
    }
    Ok(())
}

/// Inverse of [`reconstruct_pair`]: recover the encoder's `(X, Y)`
/// from an `(L, R)` pair using the closed form algebraically
/// re-derived from the staged docs:
///
/// ```text
///   Y = L - R
///   X = R + Y / 2
/// ```
///
/// This is the exact inverse map of [`reconstruct_pair`]: because the
/// **same** `Y / 2` term is added here and subtracted there (and the
/// arithmetic is wrapping, i.e. mod 2^32), both compositions are the
/// identity for **every** input pair — no parity condition. Only
/// cross-pairing the divide spelling with the arithmetic-shift
/// spelling can disagree, and then exactly on odd negative `Y`.
///
/// `const fn` so the inverse is usable in `const` contexts.
#[inline]
pub const fn decorrelate_pair(l: i32, r: i32) -> (i32, i32) {
    let y = l.wrapping_sub(r);
    let x = r.wrapping_add(y / 2);
    (x, y)
}

/// Inverse of [`reconstruct_pair_arith_shift`]: recover `(X, Y)` from
/// `(L, R)` with the arithmetic-shift spelling:
///
/// ```text
///   Y = L - R
///   X = R + (Y >> 1)
/// ```
///
/// Because the same `Y >> 1` term is added here and subtracted by
/// [`reconstruct_pair_arith_shift`], the composition is the identity
/// for **every** `(L, R)` — exactly as [`decorrelate_pair`] composes
/// with [`reconstruct_pair`] under the divide spelling. Each spelling
/// pairs losslessly with its own inverse; only cross-pairing the two
/// spellings can disagree (odd negative `Y`).
///
/// `const fn` so the inverse is usable in `const` contexts.
#[inline]
pub const fn decorrelate_pair_arith_shift(l: i32, r: i32) -> (i32, i32) {
    let y = l.wrapping_sub(r);
    let x = r.wrapping_add(y >> 1);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wiki_worked_zero_pair_is_identity() {
        // The single closed form the wiki pins evaluates `(0, 0)` to
        // `(0, 0)` regardless of which divide-vs-shift spelling the
        // reference encoder uses, so this is the one shared anchor
        // both sibling primitives must hit.
        assert_eq!(reconstruct_pair(0, 0), (0, 0));
        assert_eq!(reconstruct_pair_arith_shift(0, 0), (0, 0));
    }

    #[test]
    fn reconstruct_matches_inverse_for_even_y() {
        // For any `Y` even, `Y / 2 == Y >> 1`, so both spellings agree
        // and `reconstruct_pair . decorrelate_pair` is the identity
        // on every `(L, R)`.
        for l in [-1024i32, -1, 0, 1, 1024, i32::MIN / 4, i32::MAX / 4] {
            for r in [-1024i32, -1, 0, 1, 1024, i32::MIN / 4, i32::MAX / 4] {
                let y = l - r;
                if y % 2 != 0 {
                    continue;
                }
                let (x, y2) = decorrelate_pair(l, r);
                assert_eq!(y2, y);
                let (l2, r2) = reconstruct_pair(x, y);
                assert_eq!((l2, r2), (l, r));
                let (l3, r3) = reconstruct_pair_arith_shift(x, y);
                assert_eq!((l3, r3), (l, r));
            }
        }
    }

    #[test]
    fn divide_vs_shift_differ_for_odd_negative_y_only() {
        // The wiki narrative does not disambiguate between Rust integer
        // division (toward zero) and an arithmetic right shift (toward
        // -infinity). Anchor the exact set of inputs where the two
        // spellings disagree so a future per-version trace can pick
        // the one its fixtures pin.
        for x in -16i32..=16 {
            for y in -16i32..=16 {
                let (l_div, r_div) = reconstruct_pair(x, y);
                let (l_shift, r_shift) = reconstruct_pair_arith_shift(x, y);
                let differ = (l_div, r_div) != (l_shift, r_shift);
                let expected_diff = y < 0 && y % 2 != 0;
                assert_eq!(
                    differ, expected_diff,
                    "x={x} y={y}: divide=({l_div},{r_div}) shift=({l_shift},{r_shift})"
                );
            }
        }
    }

    #[test]
    fn block_reconstructor_fans_out_correctly() {
        let x = [1i32, 2, 3, 4];
        let y = [0i32, 4, -2, 8];
        let mut left = [0i32; 4];
        let mut right = [0i32; 4];
        reconstruct_block(&x, &y, &mut left, &mut right).unwrap();
        for i in 0..x.len() {
            let (l, r) = reconstruct_pair(x[i], y[i]);
            assert_eq!(left[i], l);
            assert_eq!(right[i], r);
        }
    }

    #[test]
    fn block_reconstructor_arith_shift_fans_out_correctly() {
        let x = [1i32, 2, 3, 4];
        let y = [0i32, 4, -3, 8];
        let mut left = [0i32; 4];
        let mut right = [0i32; 4];
        reconstruct_block_arith_shift(&x, &y, &mut left, &mut right).unwrap();
        for i in 0..x.len() {
            let (l, r) = reconstruct_pair_arith_shift(x[i], y[i]);
            assert_eq!(left[i], l);
            assert_eq!(right[i], r);
        }
    }

    #[test]
    fn block_reconstructor_rejects_length_mismatch() {
        let x = [1i32, 2, 3];
        let y = [0i32, 4];
        let mut left = [0i32; 3];
        let mut right = [0i32; 3];
        let err = reconstruct_block(&x, &y, &mut left, &mut right).unwrap_err();
        assert_eq!(
            err,
            Error::ChannelLengthMismatch {
                x: 3,
                y: 2,
                left: 3,
                right: 3
            }
        );
    }

    #[test]
    fn block_reconstructor_handles_empty_input() {
        let x: [i32; 0] = [];
        let y: [i32; 0] = [];
        let mut left: [i32; 0] = [];
        let mut right: [i32; 0] = [];
        reconstruct_block(&x, &y, &mut left, &mut right).unwrap();
        reconstruct_block_arith_shift(&x, &y, &mut left, &mut right).unwrap();
    }

    #[test]
    fn reconstructor_overflow_is_wrapping_on_documented_extreme() {
        // The wiki narrative does not enumerate the encoder's sample
        // range, so the reconstructor signature carries a full `i32`.
        // Confirm the closed form does not panic on the extreme inputs
        // a 32-bit pipeline can reach (arithmetic wraps under release
        // mode; the test is a smoke check, not a saturating claim).
        // We use values within `i32::MIN / 4 ..= i32::MAX / 4` so the
        // intermediate addition cannot overflow.
        let (l, r) = reconstruct_pair(i32::MAX / 4, i32::MAX / 4);
        let (l2, r2) = reconstruct_pair_arith_shift(i32::MAX / 4, i32::MAX / 4);
        let _ = (l, r, l2, r2);
        let (l, r) = reconstruct_pair(i32::MIN / 4, i32::MIN / 4);
        let _ = (l, r);
    }

    #[test]
    fn reconstruct_pair_is_const_eval_capable() {
        // The reconstructor and its arithmetic-shift sibling are both
        // `const fn`. Forcing the compiler to const-evaluate every
        // documented entry point here catches a future demotion as a
        // build error rather than a runtime regression.
        const ZERO: (i32, i32) = reconstruct_pair(0, 0);
        const ZERO_SHIFT: (i32, i32) = reconstruct_pair_arith_shift(0, 0);
        const INVERSE: (i32, i32) = decorrelate_pair(0, 0);
        assert_eq!(ZERO, (0, 0));
        assert_eq!(ZERO_SHIFT, (0, 0));
        assert_eq!(INVERSE, (0, 0));
    }

    #[test]
    fn exhaustive_small_input_roundtrip_matches_closed_form() {
        // Anti-fuzz: walk every `(L, R)` pair in a small box and
        // assert `reconstruct_pair(decorrelate_pair(l, r))` is the
        // identity when `Y == L - R` is even, and recovers `(L, R)`
        // exactly when `Y` is odd because the encoder writes `Y` with
        // the same rounding the decoder reads it with. Anchors the
        // closed-form symmetry the staged docs commit to.
        for l in -32i32..=32 {
            for r in -32i32..=32 {
                let (x, y) = decorrelate_pair(l, r);
                let (l2, r2) = reconstruct_pair(x, y);
                assert_eq!(
                    (l2, r2),
                    (l, r),
                    "round-trip failed for (l, r) = ({l}, {r}) via (x, y) = ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn full_extreme_inputs_round_trip_without_panicking() {
        // Hostile-input hardening: every (L, R) pair drawn from the
        // i32 extremes must round-trip exactly through BOTH spelling
        // pairs — the wrapping (mod 2^32) algebra adds and subtracts
        // the identical Y/2 term, so no parity or range condition
        // applies — and must never panic a debug build.
        let extremes = [i32::MIN, i32::MIN + 1, -1, 0, 1, i32::MAX - 1, i32::MAX];
        for &l in &extremes {
            for &r in &extremes {
                let (x, y) = decorrelate_pair(l, r);
                assert_eq!(reconstruct_pair(x, y), (l, r), "div spelling ({l}, {r})");
                let (xs, ys) = decorrelate_pair_arith_shift(l, r);
                assert_eq!(
                    reconstruct_pair_arith_shift(xs, ys),
                    (l, r),
                    "shift spelling ({l}, {r})"
                );
            }
        }
    }

    #[test]
    fn arith_shift_spelling_pair_is_identity_on_odd_negative_y() {
        // The shift spelling pairs losslessly with its own inverse on
        // exactly the inputs where the two spellings diverge.
        for (l, r) in [(0i32, 3i32), (-5, -2), (2, 5), (-1, 2)] {
            let y = l.wrapping_sub(r);
            assert!(y % 2 != 0);
            let (x, y2) = decorrelate_pair_arith_shift(l, r);
            assert_eq!(y2, y);
            assert_eq!(reconstruct_pair_arith_shift(x, y2), (l, r));
        }
    }

    #[test]
    fn cross_pairing_the_spellings_diverges_only_on_odd_negative_y() {
        // decorrelate with div, reconstruct with shift: identical
        // whenever Y is even or non-negative, off by the rounding
        // difference exactly when Y is odd and negative.
        for l in -16i32..=16 {
            for r in -16i32..=16 {
                let (x, y) = decorrelate_pair(l, r);
                let (l2, r2) = reconstruct_pair_arith_shift(x, y);
                if y % 2 == 0 || y > 0 {
                    assert_eq!((l2, r2), (l, r), "({l}, {r})");
                } else {
                    // Y odd negative: Y/2 and Y>>1 differ by exactly 1.
                    assert_eq!((l2, r2), (l + 1, r + 1), "({l}, {r})");
                }
            }
        }
    }
}
