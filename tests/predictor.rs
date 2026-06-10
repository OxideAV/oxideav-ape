//! Integration coverage for the adaptive IIR-predictor per-value step
//! the wiki §"IIR Filtering" pins. Walks every public-API entry point
//! the crate root re-exports.

use oxideav_ape::{adapt_sign, predict_dot, predict_step, predict_step_self_ref, Error};

#[test]
fn dot_product_anchors_at_public_api() {
    // t = sum history[i] * par[i].
    let history = [2i32, -3, 5];
    let par = [7i32, 11, 13];
    // 2*7 + (-3)*11 + 5*13 = 14 - 33 + 65 = 46.
    assert_eq!(predict_dot(&history, &par).unwrap(), 46);
}

#[test]
fn adapt_sign_anchors_at_public_api() {
    assert_eq!(adapt_sign(-3), -1);
    assert_eq!(adapt_sign(0), 0);
    assert_eq!(adapt_sign(3), 1);
}

#[test]
fn step_recurrence_anchors_at_public_api() {
    // out = in + t, with t read from the pre-adaptation par[], and the
    // sign-of-input adaptation applied afterward.
    let history = [1i32, 0, -1];
    let adapt_ref = [2i32, 2, 2];
    let mut par = [3i32, 4, 5];
    // t = 1*3 + 0*4 + (-1)*5 = -2; in = 9 -> out = 7.
    let out = predict_step(9, &history, &adapt_ref, &mut par).unwrap();
    assert_eq!(out, 7);
    // in > 0 -> par += adapt_ref.
    assert_eq!(par, [5, 6, 7]);
}

#[test]
fn self_ref_step_matches_explicit_with_history_window() {
    let history = [4i32, -2, 6];
    let mut par_a = [1i32, 2, 3];
    let mut par_b = [1i32, 2, 3];
    let a = predict_step(-8, &history, &history, &mut par_a).unwrap();
    let b = predict_step_self_ref(-8, &history, &mut par_b).unwrap();
    assert_eq!(a, b);
    assert_eq!(par_a, par_b);
}

#[test]
fn order_mismatch_surfaces_at_public_api() {
    let history = [1i32, 2, 3];
    let par = [1i32, 2];
    let err = predict_dot(&history, &par).unwrap_err();
    assert_eq!(err, Error::PredictorOrderMismatch { history: 3, par: 2 });
}

#[test]
fn dc_signal_drives_par_monotonically() {
    // A constant-positive input run adapts par[] upward by adapt_ref on
    // every step (no zero-input steps to stall it), exercising the
    // step primitive across a short sequence the way a frame loop would
    // call it. This anchors the sign-LMS direction the wiki pins, not a
    // numeric oracle for any particular file.
    let history = [1i32, 1, 1];
    let adapt_ref = [1i32, 1, 1];
    let mut par = [0i32, 0, 0];
    for _ in 0..10 {
        let _ = predict_step(1, &history, &adapt_ref, &mut par).unwrap();
    }
    assert_eq!(par, [10, 10, 10]);
}
