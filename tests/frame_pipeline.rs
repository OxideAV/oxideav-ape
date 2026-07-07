//! Integration coverage of the frame pipeline through the crate's
//! public re-export surface only: `DeltaSource`/`DeltaSink` boundary,
//! `decode_frame`/`encode_frame` orchestration, `cascade_*` filter
//! walks, and the header parser's documented value space.

use oxideav_ape::{
    cascade_decode, cascade_encode, decode_frame, encode_frame, ksum_pivot, stage1_predict,
    CompressionLevel, CorrelationRounding, DeltaSink, DeltaSource, Error, FilterCascade,
    FrameChannels, HeaderPrefix, StageState,
};

fn xorshift(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *state = x;
    x
}

struct VecSource(Vec<Vec<i32>>);

impl DeltaSource for VecSource {
    fn unpack_deltas(&mut self, channel: usize, out: &mut [i32]) -> oxideav_ape::Result<()> {
        out.copy_from_slice(&self.0[channel]);
        Ok(())
    }
}

#[derive(Default)]
struct VecSink(Vec<Vec<i32>>);

impl DeltaSink for VecSink {
    fn pack_deltas(&mut self, _channel: usize, deltas: &[i32]) -> oxideav_ape::Result<()> {
        self.0.push(deltas.to_vec());
        Ok(())
    }
}

#[test]
fn stereo_frame_round_trips_with_a_primitive_composed_policy() {
    // The history policy is unpinned; compose it from the crate's OWN
    // pinned closed forms (stage1_predict over the residual, mixed with
    // a ksum_pivot-scaled term) to prove the primitives interoperate at
    // the public surface and the round-trip survives a nontrivial
    // policy.
    let policy = |_stage: usize, residual: i32, filtered: i32| -> i32 {
        let scaled = stage1_predict(residual);
        let pivot = ksum_pivot(filtered.unsigned_abs() as u64) as i32;
        scaled.wrapping_add(pivot)
    };

    for level in CompressionLevel::ALL {
        let cascade = FilterCascade::for_level(level);
        let mut rng = 0xABCD_EF01_2345_6789u64 ^ u64::from(u16::from(level));
        let frame_len = 64usize;
        let left: Vec<i32> = (0..frame_len)
            .map(|_| (xorshift(&mut rng) as i32) % 32768)
            .collect();
        let right: Vec<i32> = (0..frame_len)
            .map(|_| (xorshift(&mut rng) as i32) % 32768)
            .collect();

        let mut enc_states = [
            StageState::for_cascade(&cascade),
            StageState::for_cascade(&cascade),
        ];
        let mut sink = VecSink::default();
        encode_frame(
            &[left.clone(), right.clone()],
            &mut sink,
            FrameChannels::Stereo,
            |ch, arr| cascade_encode(arr, &mut enc_states[ch], policy),
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();

        let mut dec_states = [
            StageState::for_cascade(&cascade),
            StageState::for_cascade(&cascade),
        ];
        let mut src = VecSource(sink.0.clone());
        let out = decode_frame(
            &mut src,
            FrameChannels::Stereo,
            frame_len,
            |ch, arr| cascade_decode(arr, &mut dec_states[ch], policy),
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        assert_eq!(out[0], left, "{level:?}");
        assert_eq!(out[1], right, "{level:?}");
    }
}

#[test]
fn consecutive_frames_carry_filter_state_across_the_boundary() {
    // Filter state persists across frames of a stream: encoding two
    // frames back-to-back and decoding them back-to-back (states
    // carried through) must reproduce both frames; decoding the second
    // frame with FRESH states must not (the state carry is real).
    let cascade = FilterCascade::for_level(CompressionLevel::Normal);
    let mut rng = 0x1357_9BDF_u64;
    let frame_len = 32usize;
    let frames: Vec<Vec<i32>> = (0..2)
        .map(|_| {
            (0..frame_len)
                .map(|_| (xorshift(&mut rng) as i32) % 4096)
                .collect()
        })
        .collect();

    let mut enc_states = StageState::for_cascade(&cascade);
    let mut packed = Vec::new();
    for frame in &frames {
        let mut sink = VecSink::default();
        encode_frame(
            core::slice::from_ref(frame),
            &mut sink,
            FrameChannels::Mono,
            |_ch, arr| cascade_encode(arr, &mut enc_states, |_i, r, _f| r),
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        packed.push(sink.0.remove(0));
    }

    // Carried states: both frames reproduce.
    let mut dec_states = StageState::for_cascade(&cascade);
    for (frame, deltas) in frames.iter().zip(&packed) {
        let mut src = VecSource(vec![deltas.clone()]);
        let out = decode_frame(
            &mut src,
            FrameChannels::Mono,
            frame_len,
            |_ch, arr| cascade_decode(arr, &mut dec_states, |_i, r, _f| r),
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        assert_eq!(&out[0], frame);
    }

    // Fresh states on frame 2 alone: the decode diverges, proving the
    // cross-frame state carry is doing real work.
    let mut fresh = StageState::for_cascade(&cascade);
    let mut src = VecSource(vec![packed[1].clone()]);
    let out = decode_frame(
        &mut src,
        FrameChannels::Mono,
        frame_len,
        |_ch, arr| cascade_decode(arr, &mut fresh, |_i, r, _f| r),
        CorrelationRounding::TruncatingDiv,
    )
    .unwrap();
    assert_ne!(out[0], frames[1]);
}

#[test]
fn source_errors_abort_the_frame_walk() {
    struct FailingSource;
    impl DeltaSource for FailingSource {
        fn unpack_deltas(&mut self, _c: usize, _o: &mut [i32]) -> oxideav_ape::Result<()> {
            Err(Error::NotImplemented)
        }
    }
    let err = decode_frame(
        &mut FailingSource,
        FrameChannels::Stereo,
        4,
        |_ch, _arr| Ok(()),
        CorrelationRounding::TruncatingDiv,
    )
    .unwrap_err();
    assert_eq!(err, Error::NotImplemented);
}

#[test]
fn exhaustive_compression_level_field_sweep() {
    // Parse a header prefix for every possible 16-bit compression-level
    // value: exactly the five documented profiles are accepted, and
    // every other value surfaces UnknownCompressionLevel with the raw
    // value preserved.
    let mut accepted = 0usize;
    for raw in 0..=u16::MAX {
        let mut bytes = *b"MAC \x50\x0F\x00\x00";
        bytes[6..8].copy_from_slice(&raw.to_le_bytes());
        match HeaderPrefix::parse(&bytes) {
            Ok(h) => {
                accepted += 1;
                assert_eq!(u16::from(h.compression_level), raw);
            }
            Err(Error::UnknownCompressionLevel(v)) => assert_eq!(v, raw),
            Err(other) => panic!("unexpected error for raw {raw}: {other}"),
        }
    }
    assert_eq!(accepted, CompressionLevel::ALL.len());
}
