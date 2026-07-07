//! Buffer-at-a-time application of the pinned adaptive-predictor step,
//! and its chaining across the pinned per-level filter cascade.
//!
//! The wiki §"General Decoding Process" pins that, per unpacked delta
//! array, the decoder must *"apply all IIR filters onto values"*, and
//! §"General Details" pins that 1-3 filters of different order run per
//! compression level; the extractor's `filter_config.csv` pins exactly
//! which `(order, shift)` stages each level carries
//! ([`crate::filter_config`]). This module supplies the walk that
//! applies the pinned per-value recurrence ([`crate::predictor`]) over
//! a whole buffer, stage after stage.
//!
//! ## What is pinned vs. injected
//!
//! Three things the staged docs commit to are wired here:
//!
//! * the per-value recurrence itself (delegated to
//!   [`predict_step_self_ref`] / [`residual_step_self_ref`]),
//! * the fact that a whole array of values is filtered per frame, and
//! * the per-level stage set (via [`StageState::for_cascade`]).
//!
//! Two things the staged docs decline to pin are **not** guessed:
//!
//! * the *"correct delta[] array - different for many versions"* line —
//!   the per-version rule that advances the history window between
//!   values. The runner takes it as an injected `policy` closure,
//!   called as `policy(residual, filtered)` after every value, whose
//!   return value is pushed into the sliding window. Both directions
//!   hand the policy the identical `(residual, filtered)` pair, so
//!   encode/decode round-trips exactly for **any** policy.
//! * the role of the staged per-stage `shift` inside the recurrence
//!   (the wiki's recurrence carries no shift; where the pinned `shift`
//!   scales the dot product is unpinned narrative). The runner
//!   therefore does not consume [`FilterStage::shift`]; it is carried
//!   in the config for the later phase that pins its position.
//!
//! The stage *orientation* across the cascade is likewise unpinned
//! (which end of the 1-3 stage list a decoder applies first), so the
//! two directions are defined as mutual inverses: [`cascade_encode`]
//! walks stages in `filter_index` order and [`cascade_decode`] walks
//! them in reverse. Whichever absolute orientation a later trace pins,
//! the pair stays a lossless round-trip.

use crate::error::{Error, Result};
use crate::filter_config::{FilterCascade, FilterStage};
use crate::predictor::{predict_step_self_ref, residual_step_self_ref};

/// Live state of one adaptive-filter stage: the sliding history window
/// (`delta[-order .. 0]`, oldest first) and the adaptive coefficient
/// vector `par[]`.
///
/// The two vectors always share the stage order — the constructors
/// enforce it, so a runner over a `StageState` cannot produce a
/// [`Error::PredictorOrderMismatch`] from within.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageState {
    window: Vec<i32>,
    par: Vec<i32>,
}

impl StageState {
    /// A stage of the given order with an all-zero history window and
    /// all-zero coefficients — the neutral starting state a caller uses
    /// until the per-version seeding (see
    /// [`crate::scalars::PREDICTOR_HISTORY_SEED`]) is pinned by further
    /// docs.
    pub fn zeroed(order: usize) -> Self {
        StageState {
            window: vec![0; order],
            par: vec![0; order],
        }
    }

    /// A zeroed state sized for one pinned [`FilterStage`].
    pub fn for_stage(stage: FilterStage) -> Self {
        Self::zeroed(usize::from(stage.order))
    }

    /// Zeroed states for every stage of a pinned per-level cascade, in
    /// `filter_index` order.
    pub fn for_cascade(cascade: &FilterCascade) -> Vec<Self> {
        cascade
            .stages()
            .iter()
            .map(|s| Self::for_stage(*s))
            .collect()
    }

    /// A state over explicit initial window / coefficient vectors, for
    /// callers that seed from a future pinned per-version rule. The two
    /// slices must agree on the stage order.
    pub fn with_initial(window: &[i32], par: &[i32]) -> Result<Self> {
        if window.len() != par.len() {
            return Err(Error::PredictorOrderMismatch {
                history: window.len(),
                par: par.len(),
            });
        }
        Ok(StageState {
            window: window.to_vec(),
            par: par.to_vec(),
        })
    }

    /// The stage order (number of prediction taps).
    pub fn order(&self) -> usize {
        self.par.len()
    }

    /// Read access to the sliding history window, oldest first.
    pub fn window(&self) -> &[i32] {
        &self.window
    }

    /// Read access to the adaptive coefficient vector.
    pub fn par(&self) -> &[i32] {
        &self.par
    }

    fn push(&mut self, novel: i32) {
        if !self.window.is_empty() {
            self.window.rotate_left(1);
            *self.window.last_mut().unwrap() = novel;
        }
    }
}

/// Decode-direction buffer walk of one adaptive stage: rewrite each
/// residual in `values` to its filtered value via the pinned per-value
/// recurrence (`out = in + t`, aliased-window reading), advancing the
/// history window between values with the injected `policy`.
///
/// `policy(residual, filtered)` is called after every value; its return
/// is the element pushed into the sliding window (the wiki's unpinned
/// *"correct delta[] array"* step). An order-0 stage (the fast-level
/// cascade) leaves the buffer untouched apart from the policy calls
/// being skipped entirely (there is no window to advance).
pub fn filter_stage_decode<P>(
    values: &mut [i32],
    state: &mut StageState,
    mut policy: P,
) -> Result<()>
where
    P: FnMut(i32, i32) -> i32,
{
    if state.order() == 0 {
        return Ok(());
    }
    for v in values.iter_mut() {
        let residual = *v;
        let filtered = predict_step_self_ref(residual, &state.window, &mut state.par)?;
        *v = filtered;
        let novel = policy(residual, filtered);
        state.push(novel);
    }
    Ok(())
}

/// Encoder-direction buffer walk of one adaptive stage: rewrite each
/// value in `values` to its residual via the algebraic inverse of the
/// pinned recurrence (`in = out - t`), advancing the history window
/// with the same injected `policy`.
///
/// The policy sees the identical `(residual, filtered)` pair the decode
/// direction sees, so `filter_stage_encode` followed by
/// [`filter_stage_decode`] over equal starting states is an exact
/// identity — buffer, window, and `par[]` trajectory — for any policy.
pub fn filter_stage_encode<P>(
    values: &mut [i32],
    state: &mut StageState,
    mut policy: P,
) -> Result<()>
where
    P: FnMut(i32, i32) -> i32,
{
    if state.order() == 0 {
        return Ok(());
    }
    for v in values.iter_mut() {
        let filtered = *v;
        let residual = residual_step_self_ref(filtered, &state.window, &mut state.par)?;
        *v = residual;
        let novel = policy(residual, filtered);
        state.push(novel);
    }
    Ok(())
}

/// Decode-direction cascade walk — *"apply all IIR filters onto
/// values"* — running every stage in `states` over the whole buffer,
/// in **reverse** `filter_index` order (the mutual-inverse convention;
/// see the module docs on the unpinned absolute orientation).
///
/// `policy(stage_index, residual, filtered)` receives the index of the
/// stage being advanced, so a per-version rule that differs across
/// stages can dispatch on it.
pub fn cascade_decode<P>(values: &mut [i32], states: &mut [StageState], mut policy: P) -> Result<()>
where
    P: FnMut(usize, i32, i32) -> i32,
{
    for (idx, state) in states.iter_mut().enumerate().rev() {
        filter_stage_decode(values, state, |r, f| policy(idx, r, f))?;
    }
    Ok(())
}

/// Encoder-direction cascade walk: every stage in `states` over the
/// whole buffer, in `filter_index` order. Exact inverse of
/// [`cascade_decode`] over equal starting states and the same policy.
pub fn cascade_encode<P>(values: &mut [i32], states: &mut [StageState], mut policy: P) -> Result<()>
where
    P: FnMut(usize, i32, i32) -> i32,
{
    for (idx, state) in states.iter_mut().enumerate() {
        filter_stage_encode(values, state, |r, f| policy(idx, r, f))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter_config::MAX_CASCADE_DEPTH;
    use crate::header::CompressionLevel;
    use crate::predictor::predict_step_self_ref as step;

    fn xorshift(state: &mut u64) -> u64 {
        let mut x = *state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *state = x;
        x
    }

    /// The most literal reading of the unpinned maintenance line: push
    /// the raw residual into the window. Used here only as *a* policy;
    /// the runner must round-trip under any.
    fn raw_residual_policy(residual: i32, _filtered: i32) -> i32 {
        residual
    }

    #[test]
    fn order_zero_stage_is_identity() {
        // The fast (1000) level's single order-0 stage must leave the
        // buffer untouched in both directions.
        let mut state_d = StageState::zeroed(0);
        let mut state_e = StageState::zeroed(0);
        let mut buf = [5i32, -3, 0, i32::MAX, i32::MIN];
        let orig = buf;
        filter_stage_decode(&mut buf, &mut state_d, raw_residual_policy).unwrap();
        assert_eq!(buf, orig);
        filter_stage_encode(&mut buf, &mut state_e, raw_residual_policy).unwrap();
        assert_eq!(buf, orig);
    }

    #[test]
    fn decode_walk_matches_a_manual_step_loop() {
        // The runner must be exactly "the pinned per-value step in a
        // loop, plus the policy push" — anchor it against a hand-rolled
        // equivalent.
        let mut state = StageState::with_initial(&[1, 2], &[3, 4]).unwrap();
        let mut buf = [10i32, -20, 30];
        filter_stage_decode(&mut buf, &mut state, raw_residual_policy).unwrap();

        let mut window = vec![1i32, 2];
        let mut par = vec![3i32, 4];
        let mut expect = Vec::new();
        for &residual in &[10i32, -20, 30] {
            let filtered = step(residual, &window, &mut par).unwrap();
            expect.push(filtered);
            window.rotate_left(1);
            *window.last_mut().unwrap() = residual;
        }
        assert_eq!(buf.as_slice(), expect.as_slice());
        assert_eq!(state.window(), window.as_slice());
        assert_eq!(state.par(), par.as_slice());
    }

    #[test]
    fn stage_round_trips_under_prng_policies() {
        // encode -> decode over equal starting states must be an exact
        // identity for ANY policy. Drive a PRNG-valued policy so the
        // window contents are far from either natural reading.
        for order in [1usize, 2, 16, 32] {
            let mut rng = 0x9E37_79B9_7F4A_7C15u64 ^ (order as u64);
            let mut init_window = vec![0i32; order];
            let mut init_par = vec![0i32; order];
            for v in init_window.iter_mut().chain(init_par.iter_mut()) {
                *v = (xorshift(&mut rng) as i32) % 512;
            }
            let mut enc_state = StageState::with_initial(&init_window, &init_par).unwrap();
            let mut dec_state = enc_state.clone();

            let mut buf: Vec<i32> = (0..200).map(|_| xorshift(&mut rng) as i32).collect();
            let orig = buf.clone();

            // The policy must see the same (residual, filtered) pairs in
            // both directions; give each side its own PRNG stream seeded
            // identically so the pushed values agree iff the pairs do.
            let mut rng_e = 0xDEAD_BEEF_u64 ^ (order as u64);
            filter_stage_encode(&mut buf, &mut enc_state, |r, f| {
                r.wrapping_add(f)
                    .wrapping_add((xorshift(&mut rng_e) as i32) % 16)
            })
            .unwrap();
            assert_ne!(buf, orig, "order {order}: encode should transform");

            let mut rng_d = 0xDEAD_BEEF_u64 ^ (order as u64);
            filter_stage_decode(&mut buf, &mut dec_state, |r, f| {
                r.wrapping_add(f)
                    .wrapping_add((xorshift(&mut rng_d) as i32) % 16)
            })
            .unwrap();
            assert_eq!(buf, orig, "order {order}: round trip");
            assert_eq!(enc_state, dec_state, "order {order}: state trajectory");
        }
    }

    #[test]
    fn policy_sees_identical_pairs_in_both_directions() {
        let mut enc_state = StageState::with_initial(&[7, -2, 9], &[1, 0, -3]).unwrap();
        let mut dec_state = enc_state.clone();
        let mut buf = [100i32, -50, 0, 25, -8000];
        let orig = buf;

        let mut enc_pairs = Vec::new();
        filter_stage_encode(&mut buf, &mut enc_state, |r, f| {
            enc_pairs.push((r, f));
            r
        })
        .unwrap();

        let mut dec_pairs = Vec::new();
        filter_stage_decode(&mut buf, &mut dec_state, |r, f| {
            dec_pairs.push((r, f));
            r
        })
        .unwrap();

        assert_eq!(buf, orig);
        assert_eq!(enc_pairs, dec_pairs);
        // The filtered member of each pair is the original sample.
        for (pair, &sample) in enc_pairs.iter().zip(orig.iter()) {
            assert_eq!(pair.1, sample);
        }
    }

    #[test]
    fn every_documented_level_cascade_round_trips() {
        // Chain the full pinned cascade for each of the five documented
        // levels over a PRNG buffer; cascade_encode then cascade_decode
        // must restore it exactly, including all per-stage state.
        for level in CompressionLevel::ALL {
            let cascade = FilterCascade::for_level(level);
            let mut enc_states = StageState::for_cascade(&cascade);
            assert!(enc_states.len() <= MAX_CASCADE_DEPTH);
            let mut dec_states = enc_states.clone();

            let mut rng = 0x1234_5678_9ABC_DEF0u64 ^ u64::from(u16::from(level));
            let mut buf: Vec<i32> = (0..64)
                .map(|_| (xorshift(&mut rng) as i32) % 65536)
                .collect();
            let orig = buf.clone();

            cascade_encode(&mut buf, &mut enc_states, |_i, r, _f| r).unwrap();
            cascade_decode(&mut buf, &mut dec_states, |_i, r, _f| r).unwrap();
            assert_eq!(buf, orig, "level {level:?}");
            assert_eq!(enc_states, dec_states, "level {level:?}");
        }
    }

    #[test]
    fn cascade_policy_receives_stage_indices() {
        // Insane runs three stages; the per-stage policy must be handed
        // each stage's filter_index, encode in ascending order and
        // decode in the mutual-inverse (descending) order.
        let cascade = FilterCascade::for_level(CompressionLevel::Insane);
        let mut states = StageState::for_cascade(&cascade);
        let mut buf = [1i32, 2, 3];

        let mut seen = Vec::new();
        cascade_encode(&mut buf, &mut states, |i, r, _f| {
            if seen.last() != Some(&i) {
                seen.push(i);
            }
            r
        })
        .unwrap();
        assert_eq!(seen, vec![0, 1, 2]);

        let mut seen_dec = Vec::new();
        let mut dec_states = StageState::for_cascade(&cascade);
        cascade_decode(&mut buf, &mut dec_states, |i, r, _f| {
            if seen_dec.last() != Some(&i) {
                seen_dec.push(i);
            }
            r
        })
        .unwrap();
        assert_eq!(seen_dec, vec![2, 1, 0]);
    }

    #[test]
    fn fast_level_cascade_is_a_no_op() {
        // Fast (1000) pins a single order-0 stage: the cascade walk must
        // leave any buffer bit-identical in both directions.
        let cascade = FilterCascade::for_level(CompressionLevel::Fast);
        let mut states = StageState::for_cascade(&cascade);
        let mut buf = [i32::MIN, -1, 0, 1, i32::MAX];
        let orig = buf;
        cascade_encode(&mut buf, &mut states, |_i, r, _f| r).unwrap();
        assert_eq!(buf, orig);
        cascade_decode(&mut buf, &mut states, |_i, r, _f| r).unwrap();
        assert_eq!(buf, orig);
    }

    #[test]
    fn with_initial_rejects_order_mismatch() {
        let err = StageState::with_initial(&[1, 2, 3], &[1, 2]).unwrap_err();
        assert_eq!(err, Error::PredictorOrderMismatch { history: 3, par: 2 });
    }

    #[test]
    fn zeroed_state_geometry() {
        let s = StageState::zeroed(16);
        assert_eq!(s.order(), 16);
        assert_eq!(s.window(), &[0i32; 16]);
        assert_eq!(s.par(), &[0i32; 16]);
        assert_eq!(StageState::zeroed(0).order(), 0);
    }
}
