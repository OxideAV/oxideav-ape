//! Whole-file encoder validation: vendor-fixture re-encode parity and
//! the synthetic PCM battery.
//!
//! The decoder is the encoder's oracle — it is proven byte-exact
//! against the vendor corpus — so every test closes the loop
//! `PCM -> ApeEncoder -> ApeDecoder -> PCM` and asserts exact
//! recovery (which also CRC-verifies every frame against its stored
//! checksum inside `decode_all_bytes`). The re-encode tests
//! additionally pin the encoder's *output shape* against the vendor
//! files themselves: re-encoding a fixture's exact PCM under the
//! fixture's own parameters must reproduce the vendor's frame CRCs,
//! flags, and compressed size to within the final coder-flush bytes.
//!
//! (Black-box validation against the vendor console binary — our
//! files decoding byte-exactly through it and passing its whole-file
//! verify, its files decoding byte-exactly through us, at every level
//! and depth — ran out-of-tree on the reference binary; the results
//! are recorded in the README.)

use oxideav_ape::decoder::ApeDecoder;
use oxideav_ape::encoder::{encode_pcm, encode_wav, ApeEncoder, EncoderConfig};
use oxideav_ape::header::CompressionLevel;
use oxideav_ape::pcm::{deinterleave_pcm_bytes, interleave_pcm_bytes};
use oxideav_ape::writer::canonical_wav_header;

const FIXTURES: [(&str, &[u8]); 7] = [
    (
        "left_silent_stereo",
        include_bytes!("fixtures/left_silent_stereo.ape"),
    ),
    ("noise_stereo", include_bytes!("fixtures/noise_stereo.ape")),
    (
        "silence_mono8k",
        include_bytes!("fixtures/silence_mono8k.ape"),
    ),
    (
        "silence_stereo",
        include_bytes!("fixtures/silence_stereo.ape"),
    ),
    (
        "tone_lr_equal",
        include_bytes!("fixtures/tone_lr_equal.ape"),
    ),
    (
        "two_frame_mono8k",
        include_bytes!("fixtures/two_frame_mono8k.ape"),
    ),
    (
        "zeros_then_noise_mono",
        include_bytes!("fixtures/zeros_then_noise_mono.ape"),
    ),
];

/// Deterministic xorshift noise in `[-bound, bound)`.
fn noise(seed: u64, len: usize, bound: i32) -> Vec<i32> {
    let mut s = seed | 1;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            ((s >> 33) as i32) % (2 * bound) - bound
        })
        .collect()
}

/// Encode `pcm` under `cfg`, decode it back, and assert byte-exact
/// recovery. Returns the encoded file.
fn assert_round_trip(cfg: EncoderConfig, pcm: &[Vec<i32>], label: &str) -> Vec<u8> {
    let bits = cfg.bits_per_sample;
    let file = encode_pcm(cfg, pcm).unwrap_or_else(|e| panic!("{label}: encode — {e}"));
    let dec = ApeDecoder::new(&file).unwrap_or_else(|e| panic!("{label}: parse — {e}"));
    let got = dec
        .decode_all_bytes()
        .unwrap_or_else(|e| panic!("{label}: decode — {e}"));
    assert_eq!(
        got,
        interleave_pcm_bytes(pcm, bits).unwrap(),
        "{label}: PCM recovery"
    );
    file
}

/// Re-encode every vendor fixture's exact PCM under the fixture's own
/// parameters (level, geometry, stored WAV header, terminating blob)
/// and pin the output against the vendor file: identical parsed
/// fields, identical per-frame stored CRCs and flags, byte-exact
/// decode, and a compressed size within the final coder-flush bytes
/// of the vendor's.
#[test]
fn re_encoding_every_vendor_fixture_reproduces_its_shape() {
    for (name, data) in FIXTURES {
        let vendor = ApeDecoder::new(data).unwrap();
        let vinfo = vendor.info().clone();
        let pcm_bytes = vendor.decode_all_bytes().unwrap();
        let pcm =
            deinterleave_pcm_bytes(&pcm_bytes, vinfo.bits_per_sample, vinfo.channels).unwrap();

        let mut cfg = EncoderConfig::new(
            vinfo.compression_level,
            vinfo.channels,
            vinfo.sample_rate,
            vinfo.bits_per_sample,
        );
        cfg.blocks_per_frame = vinfo.blocks_per_frame;
        cfg.wav_header = Some(vinfo.wav_header.clone());
        cfg.terminating_data = data[vinfo.audio_data_end() as usize..].to_vec();
        let ours = encode_pcm(cfg, &pcm).unwrap();

        let dec = ApeDecoder::new(&ours).unwrap();
        let info = dec.info();
        assert_eq!(info.version, 3990, "{name}");
        assert_eq!(info.compression_level, vinfo.compression_level, "{name}");
        assert_eq!(info.channels, vinfo.channels, "{name}");
        assert_eq!(info.sample_rate, vinfo.sample_rate, "{name}");
        assert_eq!(info.bits_per_sample, vinfo.bits_per_sample, "{name}");
        assert_eq!(info.blocks_per_frame, vinfo.blocks_per_frame, "{name}");
        assert_eq!(info.total_frames, vinfo.total_frames, "{name}");
        assert_eq!(info.final_frame_blocks, vinfo.final_frame_blocks, "{name}");
        assert_eq!(info.wav_header, vinfo.wav_header, "{name}");
        assert_eq!(info.seek_table, vinfo.seek_table, "{name}: seek table");

        // Identical PCM in, identical stored CRC + flags out.
        for i in 0..info.total_frames {
            let ours_p = dec.frame_residuals(i).unwrap().prologue;
            let vendor_p = vendor.frame_residuals(i).unwrap().prologue;
            assert_eq!(ours_p.crc31, vendor_p.crc31, "{name}: frame {i} CRC");
            assert_eq!(ours_p.flags, vendor_p.flags, "{name}: frame {i} flags");
        }

        // Byte-exact decode of our own file.
        assert_eq!(dec.decode_all_bytes().unwrap(), pcm_bytes, "{name}: PCM");

        // Size parity: the payload matches the vendor's to within the
        // final flush bytes plus the word-alignment pad of each frame
        // tail (the vendor picks a rounded value inside the final
        // coder interval; the arithmetic path is otherwise identical).
        let ours_len = ours.len() as i64;
        let vendor_len = data.len() as i64;
        assert!(
            (ours_len - vendor_len).abs() <= 8 * i64::from(info.total_frames),
            "{name}: size {ours_len} vs vendor {vendor_len}"
        );
    }
}

/// The synthetic battery the encoder must survive at every level:
/// silence, full-scale square, uniform noise, a sweep, DC, and odd
/// lengths — multi-frame, both channel shapes.
#[test]
fn synthetic_battery_round_trips_at_every_level() {
    let n = 2600usize;
    let full_scale: Vec<i32> = (0..n)
        .map(|i| if (i / 64) % 2 == 0 { 32767 } else { -32768 })
        .collect();
    let sweep: Vec<i32> = (0..n)
        .map(|i| {
            let i = i as f64;
            (18000.0 * (0.0002 * i * i).sin()) as i32
        })
        .collect();
    let battery: [(&str, Vec<Vec<i32>>); 6] = [
        ("silence", vec![vec![0; n], vec![0; n]]),
        (
            "full_scale_square",
            vec![
                full_scale.clone(),
                // The rail-to-rail inverse (negating -32768 would
                // overflow 16 bits — the encoder rejects that).
                full_scale
                    .iter()
                    .map(|&v| if v > 0 { -32768 } else { 32767 })
                    .collect(),
            ],
        ),
        (
            "noise",
            vec![noise(0xBA77E51, n, 32768), noise(0xBA77E52, n, 32768)],
        ),
        ("sweep", vec![sweep.clone(), sweep]),
        ("dc", vec![vec![1000; n], vec![-1000; n]]),
        (
            "odd_length",
            vec![noise(0x0DD, 1237, 4000), noise(0x0DE, 1237, 4000)],
        ),
    ];
    for level in CompressionLevel::ALL {
        for (name, pcm) in &battery {
            let mut cfg = EncoderConfig::new(level, 2, 44100, 16);
            cfg.blocks_per_frame = 1024; // multi-frame with a short tail
            let file = assert_round_trip(cfg, pcm, &format!("{name} {level:?}"));
            let dec = ApeDecoder::new(&file).unwrap();
            let blocks = pcm[0].len() as u32;
            assert_eq!(dec.info().total_frames, blocks.div_ceil(1024));
            assert_eq!(
                dec.info().final_frame_blocks,
                if blocks % 1024 == 0 {
                    1024
                } else {
                    blocks % 1024
                },
                "{name} {level:?}"
            );
            // Mono variant of the same signal.
            let mut cfg = EncoderConfig::new(level, 1, 22050, 16);
            cfg.blocks_per_frame = 999;
            assert_round_trip(cfg, &pcm[..1], &format!("{name} mono {level:?}"));
        }
    }
}

/// Odd frame geometries: 1-sample file, exactly one frame, one block
/// over a frame boundary.
#[test]
fn frame_boundary_geometries_round_trip() {
    for blocks in [1usize, 999, 1000, 1001, 2000, 2001] {
        let pcm = vec![noise(blocks as u64, blocks, 12000)];
        let mut cfg = EncoderConfig::new(CompressionLevel::Normal, 1, 44100, 16);
        cfg.blocks_per_frame = 1000;
        let file = assert_round_trip(cfg, &pcm, &format!("{blocks} blocks"));
        let dec = ApeDecoder::new(&file).unwrap();
        assert_eq!(
            dec.info().total_frames,
            (blocks as u32).div_ceil(1000),
            "{blocks} blocks"
        );
    }
}

/// Silence / pseudo-stereo / partial-silence frames mix correctly in
/// one multi-frame stream (per-frame flags, no payload for silent
/// frames, one shared array for pseudo-stereo).
#[test]
fn special_frames_mix_within_one_stream() {
    let f = 512usize;
    let loud = noise(0x51E, f, 20000);
    let mut ch0 = Vec::new();
    let mut ch1 = Vec::new();
    // Frame 1: both silent. Frame 2: pseudo-stereo. Frame 3: stored
    // channel 0 silent. Frame 4: plain stereo. Frame 5 (short): stored
    // channel 1 silent.
    ch0.extend(vec![0; f]);
    ch1.extend(vec![0; f]);
    ch0.extend(loud.clone());
    ch1.extend(loud.clone());
    ch0.extend(vec![0; f]);
    ch1.extend(loud.clone());
    ch0.extend(noise(4, f, 100));
    ch1.extend(loud.clone());
    ch0.extend(noise(5, f / 2, 100));
    ch1.extend(vec![0; f / 2]);

    let mut cfg = EncoderConfig::new(CompressionLevel::High, 2, 44100, 16);
    cfg.blocks_per_frame = f as u32;
    let pcm = vec![ch0, ch1];
    let file = assert_round_trip(cfg, &pcm, "special mix");
    let dec = ApeDecoder::new(&file).unwrap();
    assert_eq!(dec.info().total_frames, 5);
    let flags: Vec<u32> = (0..5)
        .map(|i| {
            dec.frame_residuals(i)
                .unwrap()
                .prologue
                .flags
                .map_or(0, |f| f.0)
        })
        .collect();
    // Bit 0 = stored channel 1 silent, bit 1 = stored channel 0
    // silent, bit 2 = pseudo-stereo (the empirical §4.2 assignment).
    assert_eq!(flags, vec![7, 4, 2, 0, 1]);
    let arrays: Vec<usize> = (0..5)
        .map(|i| dec.frame_residuals(i).unwrap().arrays.len())
        .collect();
    // Partial silence changes nothing about the layout (§4.2): only
    // full silence (no arrays decoded as zeros) and pseudo-stereo (one
    // shared array) reshape the frame.
    assert_eq!(arrays, vec![2, 1, 2, 2, 2], "coded-array shapes");
}

/// 8- and 24-bit files round-trip at every level, and the header
/// carries the right depth.
#[test]
fn depth_variants_round_trip() {
    for level in CompressionLevel::ALL {
        let pcm8 = vec![noise(8, 1500, 128), noise(9, 1500, 128)];
        let mut cfg = EncoderConfig::new(level, 2, 11025, 8);
        cfg.blocks_per_frame = 700;
        let file = assert_round_trip(cfg, &pcm8, &format!("8-bit {level:?}"));
        assert_eq!(ApeDecoder::new(&file).unwrap().info().bits_per_sample, 8);

        let pcm24 = vec![noise(24, 1500, 8_000_000)];
        let mut cfg = EncoderConfig::new(level, 1, 96000, 24);
        cfg.blocks_per_frame = 700;
        let file = assert_round_trip(cfg, &pcm24, &format!("24-bit {level:?}"));
        assert_eq!(ApeDecoder::new(&file).unwrap().info().bits_per_sample, 24);
    }
}

/// `encode_wav` splits a source WAV into the three stored blobs and
/// the decode side reproduces all of them.
#[test]
fn wav_pass_through_survives_the_full_loop() {
    let ch0 = noise(0x3AF, 3000, 25000);
    let ch1 = noise(0x3B0, 3000, 25000);
    let pcm_bytes = interleave_pcm_bytes(&[ch0, ch1], 16).unwrap();
    let mut wav = canonical_wav_header(2, 44100, 16, pcm_bytes.len() as u32);
    wav.extend_from_slice(&pcm_bytes);
    wav.extend_from_slice(b"LIST\x0a\x00\x00\x00INFOtrail!");
    let file = encode_wav(&wav, CompressionLevel::ExtraHigh).unwrap();
    let dec = ApeDecoder::new(&file).unwrap();
    let info = dec.info();
    assert_eq!(info.wav_header, &wav[..44]);
    assert_eq!(info.terminating_data_bytes, 18);
    let end = info.audio_data_end() as usize;
    assert_eq!(&file[end..], b"LIST\x0a\x00\x00\x00INFOtrail!");
    assert_eq!(dec.decode_all_bytes().unwrap(), pcm_bytes);
}

/// The streaming encoder is chunking-invariant across frame
/// boundaries: pushing one sample at a time produces the identical
/// file to one shot.
#[test]
fn one_sample_at_a_time_streaming_is_identical() {
    let ch = noise(0x111, 2500, 9000);
    let mut cfg = EncoderConfig::new(CompressionLevel::Fast, 1, 8000, 16);
    cfg.blocks_per_frame = 512;
    let one_shot = encode_pcm(cfg.clone(), std::slice::from_ref(&ch)).unwrap();
    let mut enc = ApeEncoder::new(cfg).unwrap();
    for &s in &ch {
        enc.push_samples(&[&[s]]).unwrap();
    }
    assert_eq!(enc.finish().unwrap(), one_shot);
}
