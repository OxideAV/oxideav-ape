//! Adaptive IIR-predictor step the wiki §"IIR Filtering" pins.
//!
//! The wiki snapshot at `docs/audio/ape/wiki/Monkeys_Audio.wiki`
//! §"IIR Filtering" pins exactly one per-value recurrence for the
//! adaptive integer predictor cascade. Reproduced verbatim from the
//! staged snapshot:
//!
//! ```text
//!   for each value
//!     in = delta[0]
//!     t = 0
//!     for i=0..order
//!       t += delta[-order + i] * par[i]
//!     if in < 0
//!       for i=0..order
//!         par[i] -= delta[i]
//!     if in > 0
//!       for i=0..order
//!         par[i] += delta[i]
//!     out = in + t
//!     correct delta[] array - different for many versions
//! ```
//!
//! This module ships **only** the per-value step that the wiki commits
//! to numerically:
//!
//! * the order-`N` prediction dot product `t`,
//! * the sign-of-`in` adaptation of the parameter vector `par[]`, and
//! * the output `out = in + t`,
//!
//! plus the **encoder-direction algebraic inverse** of that same step
//! ([`residual_step`] / [`residual_step_self_ref`]): `in = out - t`
//! with the identical sign-of-`in` adaptation. The inverse introduces
//! no new bitstream semantics — it is the pinned recurrence solved for
//! the other variable — and gives the pair an exact round-trip
//! (samples *and* `par[]` trajectory) under any caller-supplied history
//! policy, which is the self-consistency lever available while the
//! per-version `delta[]` maintenance stays unpinned.
//!
//! The trailing "correct delta[] array - different for many versions"
//! line is the **one** part of the recurrence the staged docs
//! explicitly decline to pin (it varies per file version), so this
//! module deliberately does not maintain the `delta[]` history ring:
//! the caller owns the history window and the per-version shift/decay
//! that advances it. That keeps the primitive faithful to exactly what
//! the snapshot fixes and leaves the unpinned per-version behaviour to
//! a future phase once additional clean-room material is staged.
//!
//! ## Two documented index ambiguities
//!
//! The wiki uses three distinct index forms inside the loop and the
//! snapshot does not annotate how they line up against a single backing
//! buffer:
//!
//! * the prediction reads `delta[-order + i]` for `i = 0..order` — the
//!   `order` samples immediately **preceding** the current one, oldest
//!   first;
//! * the adaptation reads `delta[i]` for `i = 0..order`.
//!
//! Whether the adaptation's `delta[i]` denotes the **same** window the
//! prediction reads (`delta[-order + i]`) or a different slice of the
//! ring is not disambiguated by the snapshot. This module therefore
//! takes the adaptation reference vector as an **explicit** argument
//! ([`predict_step`]), and offers the common reading where the two
//! windows coincide as a convenience wrapper
//! ([`predict_step_self_ref`]). Neither commits the crate to a guess a
//! later per-version trace would have to retract.

use crate::error::{Error, Result};

/// Sign of `value` as the `-1 / 0 / +1` step direction the wiki's
/// `if in < 0` / `if in > 0` branch selects.
///
/// The wiki adapts `par[]` by `-delta` when `in < 0`, by `+delta` when
/// `in > 0`, and leaves `par[]` untouched when `in == 0` (neither
/// branch fires). Exposed as a `const fn` so the branch direction is
/// auditable in isolation.
#[inline]
pub const fn adapt_sign(value: i64) -> i64 {
    if value < 0 {
        -1
    } else if value > 0 {
        1
    } else {
        0
    }
}

/// The order-`N` prediction dot product `t` the wiki pins:
///
/// ```text
///   t = 0
///   for i=0..order
///     t += history[i] * par[i]
/// ```
///
/// `history[i]` is `delta[-order + i]` from the snapshot — the `order`
/// samples immediately preceding the current one, oldest first — and
/// `par[i]` is the matching adaptive coefficient. Both slices must
/// share the predictor order; a length disagreement surfaces
/// [`Error::PredictorOrderMismatch`].
///
/// Accumulates in `i64`, with **wrapping** addition: a single
/// `i32 * i32` product cannot exceed `2^62`, so for every realistic
/// filter magnitude the accumulator never wraps, but an adversarial
/// full-range buffer at the pinned order-1280 stage could — and a
/// hostile stream must not be able to panic a debug build. The wrapping
/// semantics are what keep [`predict_step`] / [`residual_step`] exact
/// mutual inverses even across a wrapped accumulator.
pub fn predict_dot(history: &[i32], par: &[i32]) -> Result<i64> {
    if history.len() != par.len() {
        return Err(Error::PredictorOrderMismatch {
            history: history.len(),
            par: par.len(),
        });
    }
    let mut t: i64 = 0;
    for i in 0..history.len() {
        t = t.wrapping_add(i64::from(history[i]) * i64::from(par[i]));
    }
    Ok(t)
}

/// One adaptive-predictor step over an explicit history + adaptation
/// window, transcribing the wiki §"IIR Filtering" per-value recurrence
/// (minus the unpinned `delta[]` maintenance line).
///
/// * `input` is `in = delta[0]`, the current value entering the filter.
/// * `history` is `delta[-order .. 0]`, oldest first, feeding the
///   prediction dot product `t`.
/// * `adapt_ref` is the `delta[i]` vector the snapshot adds to / subtracts
///   from `par[]` based on `sign(input)`. It is taken explicitly because
///   the snapshot does not pin whether it aliases `history` (see the
///   module-level note); pass `history` for the common self-referential
///   reading, or use [`predict_step_self_ref`].
/// * `par` is the adaptive coefficient vector, updated **in place** by
///   `sign(input) * adapt_ref[i]` after the prediction is read.
///
/// Returns `out = input + t`. The update order matches the snapshot:
/// `t` is computed from `par[]` **before** the sign adaptation mutates
/// it, then `out` is formed from the pre-adaptation prediction.
///
/// All three slices must share the predictor order; any disagreement
/// surfaces [`Error::PredictorOrderMismatch`] (reported against the
/// `history`/`par` pair, the dot product's own contract).
pub fn predict_step(
    input: i32,
    history: &[i32],
    adapt_ref: &[i32],
    par: &mut [i32],
) -> Result<i32> {
    if history.len() != par.len() || adapt_ref.len() != par.len() {
        return Err(Error::PredictorOrderMismatch {
            history: history.len(),
            par: par.len(),
        });
    }
    // Prediction is read from `par[]` BEFORE the sign adaptation, per
    // the snapshot's statement order.
    let t = predict_dot(history, par)?;
    let sign = adapt_sign(i64::from(input));
    if sign > 0 {
        // `in > 0` -> par[i] += adapt_ref[i].
        for i in 0..par.len() {
            par[i] = par[i].wrapping_add(adapt_ref[i]);
        }
    } else if sign < 0 {
        // `in < 0` -> par[i] -= adapt_ref[i]. Wrapping subtraction so a
        // `adapt_ref[i] == i32::MIN` does not panic in debug builds.
        for i in 0..par.len() {
            par[i] = par[i].wrapping_sub(adapt_ref[i]);
        }
    }
    // `out = in + t`; the accumulator is `i64`, the output is the
    // pipeline's `i32` sample, formed by a wrapping narrow so an
    // out-of-range intermediate does not panic in release builds.
    Ok(i64::from(input).wrapping_add(t) as i32)
}

/// [`predict_step`] with the adaptation reference window aliased to the
/// prediction `history` window — the reading where the snapshot's
/// `delta[i]` and `delta[-order + i]` denote the same `order` samples.
///
/// This is the most direct reading of the snapshot when `delta[]` is a
/// single sliding window and both index forms walk it. It is offered
/// alongside the explicit-reference [`predict_step`] so a future
/// per-version trace can adopt either without forcing a call-site fork.
pub fn predict_step_self_ref(input: i32, history: &[i32], par: &mut [i32]) -> Result<i32> {
    if history.len() != par.len() {
        return Err(Error::PredictorOrderMismatch {
            history: history.len(),
            par: par.len(),
        });
    }
    let t = predict_dot(history, par)?;
    let sign = adapt_sign(i64::from(input));
    if sign > 0 {
        for i in 0..par.len() {
            par[i] = par[i].wrapping_add(history[i]);
        }
    } else if sign < 0 {
        for i in 0..par.len() {
            par[i] = par[i].wrapping_sub(history[i]);
        }
    }
    Ok(i64::from(input).wrapping_add(t) as i32)
}

/// The **encoder-direction algebraic inverse** of [`predict_step`]:
/// recover `in` (the residual) from `out` (the filtered value) against
/// the *same* history / adaptation-reference / coefficient state.
///
/// [`predict_step`] transcribes the wiki's decode-direction recurrence
/// `out = in + t` with `t` read from the pre-adaptation `par[]`. Because
/// `t` depends only on `history` and the pre-adaptation `par[]` — never
/// on `in` itself — the map `in -> out` is a bijection for fixed state,
/// and its inverse is pure algebra: `in = out - t`, followed by the
/// **identical** sign-of-`in` adaptation of `par[]` (the sign is taken
/// of the recovered residual, exactly the quantity the decode direction
/// branches on). No new bitstream semantics are introduced: this is the
/// same pinned recurrence, solved for the other variable.
///
/// Running `residual_step` then [`predict_step`] (or vice versa) over
/// the same evolving state is therefore an exact round-trip — including
/// the `par[]` trajectory — for **any** caller-supplied history policy,
/// which is what makes the pair self-validating while the per-version
/// `delta[]` maintenance remains unpinned.
///
/// The subtraction happens in `i64` and narrows with the same wrapping
/// semantics as the forward step, so the round-trip is exact even when
/// the forward `out = in + t` wrapped.
pub fn residual_step(
    output: i32,
    history: &[i32],
    adapt_ref: &[i32],
    par: &mut [i32],
) -> Result<i32> {
    if history.len() != par.len() || adapt_ref.len() != par.len() {
        return Err(Error::PredictorOrderMismatch {
            history: history.len(),
            par: par.len(),
        });
    }
    // Same pre-adaptation prediction the forward step reads.
    let t = predict_dot(history, par)?;
    // in = out - t, with the same wrapping narrow as the forward
    // (in + t) so the two are exact inverses even across wraps.
    let input = (i64::from(output).wrapping_sub(t)) as i32;
    let sign = adapt_sign(i64::from(input));
    if sign > 0 {
        for i in 0..par.len() {
            par[i] = par[i].wrapping_add(adapt_ref[i]);
        }
    } else if sign < 0 {
        for i in 0..par.len() {
            par[i] = par[i].wrapping_sub(adapt_ref[i]);
        }
    }
    Ok(input)
}

/// [`residual_step`] with the adaptation reference window aliased to the
/// prediction `history` window — the inverse of
/// [`predict_step_self_ref`], mirroring the same snapshot-reading pair
/// the forward direction offers.
pub fn residual_step_self_ref(output: i32, history: &[i32], par: &mut [i32]) -> Result<i32> {
    residual_step(output, history, history, par)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapt_sign_matches_wiki_branches() {
        // `if in < 0` -> -1, `if in > 0` -> +1, neither -> 0.
        assert_eq!(adapt_sign(-5), -1);
        assert_eq!(adapt_sign(-1), -1);
        assert_eq!(adapt_sign(0), 0);
        assert_eq!(adapt_sign(1), 1);
        assert_eq!(adapt_sign(5), 1);
        assert_eq!(adapt_sign(i64::MIN), -1);
        assert_eq!(adapt_sign(i64::MAX), 1);
    }

    #[test]
    fn dot_product_anchors_small_window() {
        // t = sum history[i] * par[i].
        let history = [1i32, 2, 3];
        let par = [10i32, 20, 30];
        // 1*10 + 2*20 + 3*30 = 10 + 40 + 90 = 140.
        assert_eq!(predict_dot(&history, &par).unwrap(), 140);
    }

    #[test]
    fn dot_product_empty_order_is_zero() {
        // Order-0 predictor: no history, t == 0, out == in.
        let history: [i32; 0] = [];
        let par: [i32; 0] = [];
        assert_eq!(predict_dot(&history, &par).unwrap(), 0);
    }

    #[test]
    fn dot_product_rejects_order_mismatch() {
        let history = [1i32, 2, 3];
        let par = [10i32, 20];
        let err = predict_dot(&history, &par).unwrap_err();
        assert_eq!(err, Error::PredictorOrderMismatch { history: 3, par: 2 });
    }

    #[test]
    fn step_output_is_input_plus_prediction() {
        // out = in + t, with t read from the pre-adaptation par[].
        let history = [1i32, 2, 3];
        let mut par = [10i32, 20, 30];
        // t = 140, in = 7 -> out = 147.
        let out = predict_step(7, &history, &history, &mut par).unwrap();
        assert_eq!(out, 147);
    }

    #[test]
    fn step_adapts_par_up_on_positive_input() {
        // in > 0 -> par[i] += adapt_ref[i].
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par = [10i32, 20, 30];
        let out = predict_step(7, &history, &adapt_ref, &mut par).unwrap();
        // Prediction read from pre-adaptation par: 140, so out = 147.
        assert_eq!(out, 147);
        // par updated by +adapt_ref.
        assert_eq!(par, [14, 25, 36]);
    }

    #[test]
    fn step_adapts_par_down_on_negative_input() {
        // in < 0 -> par[i] -= adapt_ref[i].
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par = [10i32, 20, 30];
        let out = predict_step(-7, &history, &adapt_ref, &mut par).unwrap();
        // t = 140 from pre-adaptation par; out = -7 + 140 = 133.
        assert_eq!(out, 133);
        assert_eq!(par, [6, 15, 24]);
    }

    #[test]
    fn step_leaves_par_untouched_on_zero_input() {
        // in == 0 -> neither branch fires; par is unchanged.
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par = [10i32, 20, 30];
        let out = predict_step(0, &history, &adapt_ref, &mut par).unwrap();
        // out = 0 + 140 = 140; par unchanged.
        assert_eq!(out, 140);
        assert_eq!(par, [10, 20, 30]);
    }

    #[test]
    fn step_reads_prediction_before_adapting() {
        // The snapshot computes t from par[] BEFORE the sign branch
        // mutates par[]. If we adapted first, the prediction would use
        // the post-adaptation coefficients and out would differ. Anchor
        // that ordering explicitly.
        let history = [1i32];
        let adapt_ref = [1000i32];
        let mut par = [2i32];
        // Pre-adaptation t = 1*2 = 2; out = 5 + 2 = 7.
        // Post-adaptation par would be 2 + 1000 = 1002 -> t = 1002,
        // which we must NOT observe in the output.
        let out = predict_step(5, &history, &adapt_ref, &mut par).unwrap();
        assert_eq!(out, 7);
        assert_eq!(par, [1002]);
    }

    #[test]
    fn self_ref_step_aliases_history_as_adaptation_window() {
        // predict_step_self_ref must equal predict_step with adapt_ref
        // == history.
        let history = [1i32, 2, 3];
        let mut par_a = [10i32, 20, 30];
        let mut par_b = [10i32, 20, 30];
        let out_a = predict_step(7, &history, &history, &mut par_a).unwrap();
        let out_b = predict_step_self_ref(7, &history, &mut par_b).unwrap();
        assert_eq!(out_a, out_b);
        assert_eq!(par_a, par_b);
        assert_eq!(par_a, [11, 22, 33]);
    }

    #[test]
    fn step_rejects_order_mismatch_on_any_slice() {
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par = [10i32, 20];
        let err = predict_step(7, &history, &adapt_ref, &mut par).unwrap_err();
        assert_eq!(err, Error::PredictorOrderMismatch { history: 3, par: 2 });

        // Adaptation-reference length disagreeing also rejects.
        let adapt_short = [4i32, 5];
        let mut par3 = [10i32, 20, 30];
        let err = predict_step(7, &history, &adapt_short, &mut par3).unwrap_err();
        assert_eq!(err, Error::PredictorOrderMismatch { history: 3, par: 3 });
    }

    /// Tiny xorshift PRNG so property tests need no external dep.
    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    #[test]
    fn residual_step_inverts_the_worked_example() {
        // Forward: in = 7, t = 140 -> out = 147, par += adapt_ref.
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par_fwd = [10i32, 20, 30];
        let out = predict_step(7, &history, &adapt_ref, &mut par_fwd).unwrap();
        assert_eq!(out, 147);

        // Inverse from the same PRE-step state: recovers in = 7 and the
        // identical par[] trajectory.
        let mut par_inv = [10i32, 20, 30];
        let input = residual_step(out, &history, &adapt_ref, &mut par_inv).unwrap();
        assert_eq!(input, 7);
        assert_eq!(par_inv, par_fwd);
    }

    #[test]
    fn residual_step_adapts_by_the_recovered_residual_sign() {
        // out = 133 recovers in = -7 (t = 140); the adaptation must
        // follow sign(in) = -1, matching the forward step's branch.
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par = [10i32, 20, 30];
        let input = residual_step(133, &history, &adapt_ref, &mut par).unwrap();
        assert_eq!(input, -7);
        assert_eq!(par, [6, 15, 24]);

        // out = t exactly recovers in = 0: neither branch fires.
        let mut par0 = [10i32, 20, 30];
        let input = residual_step(140, &history, &adapt_ref, &mut par0).unwrap();
        assert_eq!(input, 0);
        assert_eq!(par0, [10, 20, 30]);
    }

    #[test]
    fn residual_step_rejects_order_mismatch() {
        let history = [1i32, 2, 3];
        let adapt_ref = [4i32, 5, 6];
        let mut par = [10i32, 20];
        let err = residual_step(7, &history, &adapt_ref, &mut par).unwrap_err();
        assert_eq!(err, Error::PredictorOrderMismatch { history: 3, par: 2 });
    }

    #[test]
    fn self_ref_inverse_matches_explicit_aliasing() {
        let history = [1i32, 2, 3];
        let mut par_a = [10i32, 20, 30];
        let mut par_b = [10i32, 20, 30];
        let a = residual_step(147, &history, &history, &mut par_a).unwrap();
        let b = residual_step_self_ref(147, &history, &mut par_b).unwrap();
        assert_eq!(a, b);
        assert_eq!(par_a, par_b);
    }

    #[test]
    fn round_trip_holds_over_prng_state_evolution() {
        // Encode-then-decode over an evolving sliding window must be an
        // exact identity — samples AND par[] trajectory — for any
        // caller-supplied history policy. Use a PRNG-driven policy so the
        // property is exercised well away from the anchors.
        for order in [1usize, 2, 4, 16] {
            let mut rng = 0x243F_6A88_85A3_08D3u64 ^ (order as u64);
            let mut history = vec![0i32; order];
            let mut adapt_ref = vec![0i32; order];
            for v in history.iter_mut().chain(adapt_ref.iter_mut()) {
                *v = (xorshift(&mut rng) as i32) % 1024;
            }
            let mut par_enc = vec![0i32; order];
            let mut par_dec = vec![0i32; order];
            for v in par_enc.iter_mut() {
                *v = (xorshift(&mut rng) as i32) % 64;
            }
            par_dec.copy_from_slice(&par_enc);

            let mut hist_enc = history.clone();
            let mut hist_dec = history;
            for step in 0..256 {
                let sample = xorshift(&mut rng) as i32;
                // Encoder direction: sample -> residual.
                let residual = residual_step(sample, &hist_enc, &adapt_ref, &mut par_enc).unwrap();
                // Decoder direction: residual -> sample.
                let decoded = predict_step(residual, &hist_dec, &adapt_ref, &mut par_dec).unwrap();
                assert_eq!(decoded, sample, "order {order}, step {step}");
                assert_eq!(par_enc, par_dec, "order {order}, step {step}");
                // Shared (arbitrary) history policy: push the decoded
                // sample, mixed with the PRNG, into the window. Both
                // sides advance identically — the policy itself is the
                // unpinned per-version part.
                let novel = sample.wrapping_add((xorshift(&mut rng) as i32) % 8);
                hist_enc.rotate_left(1);
                *hist_enc.last_mut().unwrap() = novel;
                hist_dec.rotate_left(1);
                *hist_dec.last_mut().unwrap() = novel;
            }
        }
    }

    #[test]
    fn round_trip_is_exact_across_wrapping_narrows() {
        // Force out = in + t to wrap the i32 narrow; the inverse must
        // still recover the exact residual and par[] state.
        let history = [i32::MAX, i32::MAX];
        let adapt_ref = [3i32, 5];
        let mut par_fwd = [i32::MAX, 7];
        let mut par_inv = [i32::MAX, 7];
        let input = i32::MAX;
        let out = predict_step(input, &history, &adapt_ref, &mut par_fwd).unwrap();
        let back = residual_step(out, &history, &adapt_ref, &mut par_inv).unwrap();
        assert_eq!(back, input);
        assert_eq!(par_inv, par_fwd);
    }

    #[test]
    fn residual_step_order_zero_is_identity() {
        // Order-0: t == 0, so the residual IS the sample and par[] is
        // untouched (there is nothing to adapt).
        let history: [i32; 0] = [];
        let adapt_ref: [i32; 0] = [];
        let mut par: [i32; 0] = [];
        assert_eq!(
            residual_step(42, &history, &adapt_ref, &mut par).unwrap(),
            42
        );
        assert_eq!(
            residual_step(-42, &history, &adapt_ref, &mut par).unwrap(),
            -42
        );
    }

    #[test]
    fn step_does_not_panic_on_extreme_inputs() {
        // The snapshot does not bound the sample range; the i64
        // accumulator + wrapping narrow must not panic on full-i32
        // extremes in release-mode arithmetic.
        let history = [i32::MAX, i32::MIN];
        let adapt_ref = [i32::MAX, i32::MIN];
        let mut par = [i32::MAX, i32::MIN];
        let _ = predict_step(i32::MIN, &history, &adapt_ref, &mut par);
        let _ = predict_step(i32::MAX, &history, &adapt_ref, &mut par);
    }
}
