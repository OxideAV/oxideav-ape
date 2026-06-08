//! Integration coverage for the stereo channel-decorrelation
//! reconstructor the wiki §"Channel Correlation" pins. Walks every
//! one of the public-API entry points the crate root re-exports.

use oxideav_ape::{
    decorrelate_pair, reconstruct_block, reconstruct_block_arith_shift, reconstruct_pair,
    reconstruct_pair_arith_shift, Error,
};

#[test]
fn closed_form_anchors_at_public_api() {
    // The wiki §"Channel Correlation" pins exactly:
    //   R = X - Y / 2
    //   L = R + Y
    // Anchor a handful of representative inputs at the integration
    // boundary so a downstream caller can rely on the closed form
    // without re-reading the crate internals.
    let (l, r) = reconstruct_pair(0, 0);
    assert_eq!((l, r), (0, 0));

    let (l, r) = reconstruct_pair(10, 4);
    // R = 10 - 4/2 = 8; L = 8 + 4 = 12.
    assert_eq!((l, r), (12, 8));

    // Arithmetic-shift spelling agrees with division for even Y.
    let (l, r) = reconstruct_pair_arith_shift(10, 4);
    assert_eq!((l, r), (12, 8));
}

#[test]
fn divide_vs_shift_disagree_on_odd_negative_y() {
    // The wiki narrative does not pin which rounding the reference
    // encoder uses. Surface the exact disagreement at the public-API
    // boundary so a future per-version trace can pick its spelling
    // without re-deriving the test fixtures.
    let (l_div, r_div) = reconstruct_pair(5, -3);
    // Rust integer division: -3 / 2 == -1, so R = 5 - (-1) = 6, L = 6 + (-3) = 3.
    assert_eq!((l_div, r_div), (3, 6));

    let (l_shift, r_shift) = reconstruct_pair_arith_shift(5, -3);
    // Arithmetic right shift: -3 >> 1 == -2, so R = 5 - (-2) = 7, L = 7 + (-3) = 4.
    assert_eq!((l_shift, r_shift), (4, 7));
}

#[test]
fn decorrelate_pair_round_trips_through_reconstruct_pair() {
    // `reconstruct_pair . decorrelate_pair == id` on every (L, R) pair
    // where Y == L - R is even (which is half the input box); for odd
    // Y the round-trip is still identity because the decoder reads Y
    // with the same rounding the encoder writes it with.
    for l in -8i32..=8 {
        for r in -8i32..=8 {
            let (x, y) = decorrelate_pair(l, r);
            let (l2, r2) = reconstruct_pair(x, y);
            assert_eq!((l2, r2), (l, r));
        }
    }
}

#[test]
fn block_reconstructor_processes_a_full_buffer() {
    let x = [0i32, 1, 2, 3, 4];
    let y = [0i32, 2, 4, 6, 8];
    let mut left = [0i32; 5];
    let mut right = [0i32; 5];
    reconstruct_block(&x, &y, &mut left, &mut right).unwrap();
    for i in 0..x.len() {
        let (l, r) = reconstruct_pair(x[i], y[i]);
        assert_eq!(left[i], l);
        assert_eq!(right[i], r);
    }
}

#[test]
fn block_reconstructor_arith_shift_processes_a_full_buffer() {
    let x = [0i32, 1, 2, 3, 4];
    let y = [0i32, -3, -5, -7, -9]; // all odd, all negative — the disagreement set.
    let mut left = [0i32; 5];
    let mut right = [0i32; 5];
    reconstruct_block_arith_shift(&x, &y, &mut left, &mut right).unwrap();
    for i in 0..x.len() {
        let (l, r) = reconstruct_pair_arith_shift(x[i], y[i]);
        assert_eq!(left[i], l);
        assert_eq!(right[i], r);
    }
}

#[test]
fn block_reconstructor_surfaces_length_mismatch_error() {
    let x = [1i32, 2];
    let y = [3i32, 4, 5];
    let mut left = [0i32; 2];
    let mut right = [0i32; 2];
    let err = reconstruct_block(&x, &y, &mut left, &mut right).unwrap_err();
    assert_eq!(
        err,
        Error::ChannelLengthMismatch {
            x: 2,
            y: 3,
            left: 2,
            right: 2,
        }
    );
}
