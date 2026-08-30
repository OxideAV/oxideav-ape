//! `oxideav-core` framework wire-up (behind the default-on `registry`
//! cargo feature): the codec registration, the packet-facing decoder
//! and encoder adapters, and the direct `make_decoder` /
//! `make_encoder` factories (the dual-API convention).
//!
//! Monkey's Audio is a self-contained file format — the `.ape` file
//! carries its own descriptor, header, seek table, and frame payload —
//! so the packet contract of [`FrameworkDecoder`] is **whole-file**:
//! feed the complete file's bytes through one or more
//! [`oxideav_core::Packet`]s (a single packet holding the entire file
//! is the common case), then pull one decoded
//! [`oxideav_core::AudioFrame`] per APE frame. Frames stream out as
//! soon as the buffered bytes cover the audio-data region;
//! [`oxideav_core::Decoder::flush`] marks end-of-input so a file whose
//! header never arrives surfaces an error instead of waiting forever.
//!
//! Output is interleaved little-endian PCM in the §6.9 stored order —
//! `U8` for 8-bit files (stored biased by +128), `S16` for 16-bit,
//! `S24` for 24-bit — with every frame verified against its stored
//! CRC before it is handed out.
//!
//! The **encoder** adapter mirrors the same whole-file contract in the
//! other direction: feed interleaved-PCM [`AudioFrame`]s (any
//! chunking), then [`Encoder::flush`]; the single output
//! [`oxideav_core::Packet`] is the complete `.ape` file (the container
//! stores the frame count, seek table, and `cFileMD5` up front, so the
//! file can only be finalised once the input ends). The
//! `compression_level` option selects the profile (label or raw code);
//! `blocks_per_frame` tunes the frame size.

use crate::decoder::ApeDecoder;
use crate::encoder::{ApeEncoder, EncoderConfig, DEFAULT_BLOCKS_PER_FRAME};
use crate::error::Error as ApeError;
use crate::file_header::FileInfo;
use crate::header::CompressionLevel;
use oxideav_core::registry::CodecInfo;
use oxideav_core::{
    parse_options, AudioFrame, CodecCapabilities, CodecId, CodecOptionsStruct, CodecParameters,
    Decoder, Encoder, Error as CoreError, Frame, OptionField, OptionKind, OptionValue, Packet,
    Result as CoreResult, RuntimeContext, SampleFormat, TimeBase,
};

/// The registry codec id this crate claims.
pub const CODEC_ID: &str = "ape";

/// Map a crate error onto the framework error type.
fn to_core(e: ApeError) -> CoreError {
    CoreError::invalid(e.to_string())
}

/// The [`SampleFormat`] a parsed file's PCM decodes to, per the §6.9
/// bit-depth table.
pub fn sample_format_for(bits_per_sample: u16) -> Option<SampleFormat> {
    match bits_per_sample {
        8 => Some(SampleFormat::U8),
        16 => Some(SampleFormat::S16),
        24 => Some(SampleFormat::S24),
        _ => None,
    }
}

/// Whole-file packet-facing adapter over [`ApeDecoder`].
pub struct FrameworkDecoder {
    codec_id: CodecId,
    buf: Vec<u8>,
    info: Option<FileInfo>,
    next_frame: u32,
    blocks_emitted: u64,
    flushed: bool,
}

impl FrameworkDecoder {
    /// Fresh decoder with an empty input buffer.
    pub fn new() -> Self {
        FrameworkDecoder {
            codec_id: CodecId::new(CODEC_ID),
            buf: Vec::new(),
            info: None,
            next_frame: 0,
            blocks_emitted: 0,
            flushed: false,
        }
    }

    /// Try to parse the header/tail once enough bytes are buffered.
    fn ensure_info(&mut self) -> CoreResult<()> {
        if self.info.is_some() {
            return Ok(());
        }
        match FileInfo::parse(&self.buf) {
            Ok(info) => {
                self.info = Some(info);
                Ok(())
            }
            // Both variants can mean "the header simply is not all
            // here yet" while more packets may still arrive.
            Err(ApeError::Truncated) | Err(ApeError::InvalidMagic) if !self.flushed => {
                Err(CoreError::NeedMore)
            }
            Err(e) => Err(to_core(e)),
        }
    }
}

impl Default for FrameworkDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for FrameworkDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> CoreResult<()> {
        self.buf.extend_from_slice(&packet.data);
        Ok(())
    }

    fn receive_frame(&mut self) -> CoreResult<Frame> {
        self.ensure_info()?;
        let info = self.info.as_ref().expect("ensure_info populated it");
        if self.next_frame >= info.total_frames {
            return Err(CoreError::Eof);
        }
        // Frames read through the shared audio-region bit array, so
        // decoding waits until the buffered bytes cover that region.
        if (self.buf.len() as u64) < info.audio_data_end() {
            return if self.flushed {
                Err(to_core(ApeError::Truncated))
            } else {
                Err(CoreError::NeedMore)
            };
        }
        let dec = ApeDecoder::from_parsed(&self.buf, info.clone()).map_err(to_core)?;
        let index = self.next_frame;
        let bytes = dec.decode_frame_bytes(index).map_err(to_core)?;
        let samples = dec.info().frame_blocks(index).map_err(to_core)?;
        let pts = i64::try_from(self.blocks_emitted).ok();
        self.next_frame += 1;
        self.blocks_emitted += u64::from(samples);
        Ok(Frame::Audio(AudioFrame {
            samples,
            pts,
            data: vec![bytes],
        }))
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.flushed = true;
        Ok(())
    }

    fn reset(&mut self) -> CoreResult<()> {
        self.buf.clear();
        self.info = None;
        self.next_frame = 0;
        self.blocks_emitted = 0;
        self.flushed = false;
        Ok(())
    }
}

/// Direct decoder factory — the [`oxideav_core`] `DecoderFactory`
/// signature, also usable without going through the registry.
pub fn make_decoder(_params: &CodecParameters) -> CoreResult<Box<dyn Decoder>> {
    Ok(Box::new(FrameworkDecoder::new()))
}

/// The inverse of [`sample_format_for`]: the bit depth a framework
/// sample format stores as (§6.9 table).
pub fn bits_for_sample_format(format: SampleFormat) -> Option<u16> {
    match format {
        SampleFormat::U8 => Some(8),
        SampleFormat::S16 => Some(16),
        SampleFormat::S24 => Some(24),
        _ => None,
    }
}

/// Typed encoder options (the `CodecParameters::options` schema).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApeEncoderOptions {
    /// Compression profile. Accepts the narrative label (`"fast"`,
    /// `"normal"`, `"high"`, `"extra high"` / `"extra_high"`,
    /// `"insane"`) or the raw on-wire code (`1000`..`5000`).
    pub compression_level: CompressionLevel,
    /// Audio blocks per APE frame.
    pub blocks_per_frame: u32,
}

impl Default for ApeEncoderOptions {
    fn default() -> Self {
        ApeEncoderOptions {
            compression_level: CompressionLevel::default(),
            blocks_per_frame: DEFAULT_BLOCKS_PER_FRAME,
        }
    }
}

/// Parse a compression-level option value: narrative label (with `_`
/// accepted for the space in "extra high") or raw decimal code.
fn parse_level(raw: &str) -> CoreResult<CompressionLevel> {
    if let Ok(code) = raw.trim().parse::<u16>() {
        return CompressionLevel::from_u16(code).map_err(|e| CoreError::invalid(e.to_string()));
    }
    raw.replace('_', " ")
        .parse::<CompressionLevel>()
        .map_err(|e| CoreError::invalid(e.to_string()))
}

impl CodecOptionsStruct for ApeEncoderOptions {
    const SCHEMA: &'static [OptionField] = &[
        OptionField {
            name: "compression_level",
            kind: OptionKind::String,
            default: OptionValue::String(String::new()), // "normal"
            help: "compression profile: fast | normal | high | extra_high | insane, or the raw 1000..5000 code",
        },
        OptionField {
            name: "blocks_per_frame",
            kind: OptionKind::U32,
            default: OptionValue::U32(DEFAULT_BLOCKS_PER_FRAME),
            help: "audio blocks per APE frame (default 73728, the reference encoder's value)",
        },
    ];

    fn apply(&mut self, key: &str, value: &OptionValue) -> CoreResult<()> {
        match key {
            "compression_level" => self.compression_level = parse_level(value.as_str()?)?,
            "blocks_per_frame" => self.blocks_per_frame = value.as_u32()?,
            _ => unreachable!("guarded by SCHEMA"),
        }
        Ok(())
    }
}

/// Whole-file packet-facing adapter over [`ApeEncoder`]: PCM frames
/// in, one packet holding the complete `.ape` file out after `flush`.
pub struct FrameworkEncoder {
    codec_id: CodecId,
    output_params: CodecParameters,
    enc: Option<ApeEncoder>,
    samples_in: u64,
    sample_rate: u32,
    finished: Option<Vec<u8>>,
    flushed: bool,
}

impl FrameworkEncoder {
    /// Build an encoder from stream parameters: `sample_rate` and
    /// `channels` are required; `sample_format` defaults to `S16`;
    /// `options` may carry the [`ApeEncoderOptions`] keys.
    pub fn from_params(params: &CodecParameters) -> CoreResult<Self> {
        let sample_rate = params
            .sample_rate
            .ok_or_else(|| CoreError::invalid("ape encoder needs sample_rate"))?;
        let channels = params
            .channels
            .ok_or_else(|| CoreError::invalid("ape encoder needs channels"))?;
        let format = params.sample_format.unwrap_or(SampleFormat::S16);
        let bits = bits_for_sample_format(format).ok_or_else(|| {
            CoreError::invalid(format!(
                "ape stores U8 / S16 / S24 PCM only, got {format:?}"
            ))
        })?;
        let opts: ApeEncoderOptions = parse_options(&params.options)?;
        let mut cfg = EncoderConfig::new(opts.compression_level, channels, sample_rate, bits);
        cfg.blocks_per_frame = opts.blocks_per_frame;
        let enc = ApeEncoder::new(cfg).map_err(to_core)?;

        let mut output_params = CodecParameters::audio(CodecId::new(CODEC_ID));
        output_params.sample_rate = Some(sample_rate);
        output_params.channels = Some(channels);
        output_params.sample_format = Some(format);
        Ok(FrameworkEncoder {
            codec_id: CodecId::new(CODEC_ID),
            output_params,
            enc: Some(enc),
            samples_in: 0,
            sample_rate,
            finished: None,
            flushed: false,
        })
    }
}

impl Encoder for FrameworkEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.output_params
    }

    fn send_frame(&mut self, frame: &Frame) -> CoreResult<()> {
        let audio = match frame {
            Frame::Audio(a) => a,
            _ => return Err(CoreError::invalid("ape encodes audio frames only")),
        };
        let enc = self
            .enc
            .as_mut()
            .ok_or_else(|| CoreError::invalid("ape encoder already flushed"))?;
        // Interleaved PCM rides in one plane.
        let [plane] = audio.data.as_slice() else {
            return Err(CoreError::invalid(
                "ape expects one interleaved sample plane",
            ));
        };
        enc.push_interleaved_bytes(plane).map_err(to_core)?;
        self.samples_in += u64::from(audio.samples);
        Ok(())
    }

    fn receive_packet(&mut self) -> CoreResult<Packet> {
        match self.finished.take() {
            Some(data) => {
                let mut packet =
                    Packet::new(0, TimeBase::new(1, i64::from(self.sample_rate)), data);
                packet.pts = Some(0);
                packet.dts = Some(0);
                packet.duration = i64::try_from(self.samples_in).ok();
                packet.flags.keyframe = true;
                Ok(packet)
            }
            None if self.flushed => Err(CoreError::Eof),
            None => Err(CoreError::NeedMore),
        }
    }

    fn flush(&mut self) -> CoreResult<()> {
        if let Some(enc) = self.enc.take() {
            self.finished = Some(enc.finish().map_err(to_core)?);
        }
        self.flushed = true;
        Ok(())
    }
}

/// Direct encoder factory — the [`oxideav_core`] `EncoderFactory`
/// signature, also usable without going through the registry.
pub fn make_encoder(params: &CodecParameters) -> CoreResult<Box<dyn Encoder>> {
    Ok(Box::new(FrameworkEncoder::from_params(params)?))
}

/// Install this crate's codec registration into `ctx`: the `"ape"`
/// codec id with the whole-file decoder and encoder factories and the
/// `'MAC '` payload-magic claim (an `.ape` file identifies itself by
/// its own leading bytes; no container tag exists for the native
/// carriage).
pub fn register(ctx: &mut RuntimeContext) {
    let mut caps = CodecCapabilities::audio("ape_sw");
    caps.lossless = true;
    caps.max_channels = Some(2);
    ctx.codecs.register(
        CodecInfo::new(CodecId::new(CODEC_ID))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder)
            .encoder_options::<ApeEncoderOptions>()
            .payload_magic(crate::header::MAGIC),
    );
}

oxideav_core::register!("ape", register);

#[cfg(test)]
mod tests {
    use super::*;

    const NOISE_STEREO: &[u8] = include_bytes!("../tests/fixtures/noise_stereo.ape");
    const TWO_FRAME: &[u8] = include_bytes!("../tests/fixtures/two_frame_mono8k.ape");

    fn packet(data: &[u8]) -> Packet {
        Packet::new(0, TimeBase::new(1, 44100), data.to_vec())
    }

    #[test]
    fn registration_installs_a_working_decoder_factory() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let params = CodecParameters::audio(CodecId::new(CODEC_ID));
        let mut dec = ctx.codecs.first_decoder(&params).expect("factory resolves");
        dec.send_packet(&packet(NOISE_STEREO)).unwrap();
        dec.flush().unwrap();
        match dec.receive_frame().unwrap() {
            Frame::Audio(a) => {
                assert_eq!(a.samples, 6000);
                assert_eq!(a.pts, Some(0));
                assert_eq!(a.data.len(), 1, "interleaved: one plane");
                assert_eq!(a.data[0].len(), 6000 * 2 * 2);
            }
            other => panic!("expected an audio frame, got {other:?}"),
        }
        assert!(matches!(dec.receive_frame(), Err(CoreError::Eof)));
    }

    #[test]
    fn payload_magic_resolves_to_this_codec() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let id = ctx.codecs.resolve_payload_magic_ref(NOISE_STEREO);
        assert_eq!(id.map(CodecId::as_str), Some(CODEC_ID));
    }

    #[test]
    fn multi_frame_file_streams_frames_with_running_pts() {
        let mut dec = FrameworkDecoder::new();
        // Split the file across two packets to exercise accumulation.
        let mid = TWO_FRAME.len() / 2;
        dec.send_packet(&packet(&TWO_FRAME[..mid])).unwrap();
        assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
        dec.send_packet(&packet(&TWO_FRAME[mid..])).unwrap();
        let f0 = match dec.receive_frame().unwrap() {
            Frame::Audio(a) => a,
            other => panic!("{other:?}"),
        };
        assert_eq!(f0.samples, 73728);
        assert_eq!(f0.pts, Some(0));
        let f1 = match dec.receive_frame().unwrap() {
            Frame::Audio(a) => a,
            other => panic!("{other:?}"),
        };
        assert_eq!(f1.samples, 5000);
        assert_eq!(f1.pts, Some(73728));
        // Frame 1's spike survives into the interleaved bytes.
        assert_eq!(&f1.data[0][200..202], &1234i16.to_le_bytes());
        assert!(matches!(dec.receive_frame(), Err(CoreError::Eof)));
        // Reset restores the fresh-stream state.
        dec.reset().unwrap();
        assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
    }

    #[test]
    fn flushed_truncation_errors_instead_of_stalling() {
        let mut dec = FrameworkDecoder::new();
        dec.send_packet(&packet(&NOISE_STEREO[..NOISE_STEREO.len() / 2]))
            .unwrap();
        dec.flush().unwrap();
        assert!(matches!(
            dec.receive_frame(),
            Err(CoreError::InvalidData(_)) | Err(CoreError::Eof)
        ));
        // Garbage that can never parse errors out once flushed.
        let mut dec = FrameworkDecoder::new();
        dec.send_packet(&packet(&[0u8; 64])).unwrap();
        assert!(matches!(dec.receive_frame(), Err(CoreError::NeedMore)));
        dec.flush().unwrap();
        assert!(dec.receive_frame().is_err());
    }

    fn audio_params(rate: u32, ch: u16) -> CodecParameters {
        let mut p = CodecParameters::audio(CodecId::new(CODEC_ID));
        p.sample_rate = Some(rate);
        p.channels = Some(ch);
        p.sample_format = Some(SampleFormat::S16);
        p
    }

    #[test]
    fn registration_installs_a_working_encoder_factory() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let mut params = audio_params(44100, 2);
        params.options = params.options.set("compression_level", "high");
        let mut enc = ctx.codecs.first_encoder(&params).expect("factory resolves");
        // 300 stereo frames of deterministic ramp PCM.
        let pcm: Vec<i32> = (0..600).map(|i| (i * 37) % 20000 - 10000).collect();
        let bytes = crate::pcm::interleave_pcm_bytes(
            &[
                pcm.iter().step_by(2).copied().collect(),
                pcm.iter().skip(1).step_by(2).copied().collect(),
            ],
            16,
        )
        .unwrap();
        enc.send_frame(&Frame::Audio(AudioFrame {
            samples: 300,
            pts: Some(0),
            data: vec![bytes.clone()],
        }))
        .unwrap();
        assert!(matches!(enc.receive_packet(), Err(CoreError::NeedMore)));
        enc.flush().unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert_eq!(pkt.duration, Some(300));
        assert!(matches!(enc.receive_packet(), Err(CoreError::Eof)));
        // The packet is a complete .ape file: the decoder loops it back.
        let dec = ApeDecoder::new(&pkt.data).unwrap();
        assert_eq!(
            dec.info().compression_level,
            crate::header::CompressionLevel::High
        );
        assert_eq!(dec.decode_all_bytes().unwrap(), bytes);
        // And it resolves through the payload-magic probe.
        assert_eq!(
            ctx.codecs
                .resolve_payload_magic_ref(&pkt.data)
                .map(|id| id.as_str().to_owned()),
            Some(CODEC_ID.to_owned())
        );
    }

    #[test]
    fn encoder_decoder_loop_through_the_registry() {
        // Framework encoder -> framework decoder, whole loop through
        // registry-resolved factories.
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let mut params = audio_params(8000, 1);
        params.options = params
            .options
            .set("compression_level", "3000")
            .set("blocks_per_frame", "256");
        let mut enc = ctx.codecs.first_encoder(&params).unwrap();
        let samples: Vec<i32> = (0..1000).map(|i| ((i * i) % 4001) - 2000).collect();
        let bytes = crate::pcm::interleave_pcm_bytes(core::slice::from_ref(&samples), 16).unwrap();
        // Ragged chunking across frame boundaries.
        for chunk in bytes.chunks(154) {
            enc.send_frame(&Frame::Audio(AudioFrame {
                samples: (chunk.len() / 2) as u32,
                pts: None,
                data: vec![chunk.to_vec()],
            }))
            .unwrap();
        }
        enc.flush().unwrap();
        let pkt = enc.receive_packet().unwrap();
        let parsed = FileInfo::parse(&pkt.data).unwrap();
        assert_eq!(parsed.blocks_per_frame, 256);
        assert_eq!(parsed.total_frames, 4);

        let mut dec = ctx.codecs.first_decoder(&params).unwrap();
        dec.send_packet(&pkt).unwrap();
        dec.flush().unwrap();
        let mut out = Vec::new();
        loop {
            match dec.receive_frame() {
                Ok(Frame::Audio(a)) => out.extend(a.data.into_iter().flatten()),
                Ok(other) => panic!("{other:?}"),
                Err(CoreError::Eof) => break,
                Err(e) => panic!("{e}"),
            }
        }
        assert_eq!(out, bytes);
    }

    #[test]
    fn encoder_options_parse_labels_and_codes() {
        for (raw, level) in [
            ("fast", CompressionLevel::Fast),
            ("normal", CompressionLevel::Normal),
            ("extra high", CompressionLevel::ExtraHigh),
            ("extra_high", CompressionLevel::ExtraHigh),
            ("insane", CompressionLevel::Insane),
            ("1000", CompressionLevel::Fast),
            ("5000", CompressionLevel::Insane),
        ] {
            assert_eq!(parse_level(raw).unwrap(), level, "{raw}");
        }
        assert!(parse_level("ultra").is_err());
        assert!(parse_level("1500").is_err());
        // Through the schema-validated bag.
        let mut params = audio_params(44100, 2);
        params.options = params.options.set("compression_level", "bogus");
        assert!(FrameworkEncoder::from_params(&params).is_err());
        let mut params = audio_params(44100, 2);
        params.options = params.options.set("no_such_key", "1");
        assert!(FrameworkEncoder::from_params(&params).is_err());
    }

    #[test]
    fn encoder_rejects_unsupported_shapes() {
        // Missing required fields.
        let p = CodecParameters::audio(CodecId::new(CODEC_ID));
        assert!(FrameworkEncoder::from_params(&p).is_err());
        // Unsupported sample format.
        let mut p = audio_params(44100, 2);
        p.sample_format = Some(SampleFormat::F32);
        assert!(FrameworkEncoder::from_params(&p).is_err());
        // Video frames rejected; ragged plane counts rejected.
        let mut enc = FrameworkEncoder::from_params(&audio_params(44100, 1)).unwrap();
        assert!(enc
            .send_frame(&Frame::Audio(AudioFrame {
                samples: 1,
                pts: None,
                data: vec![vec![0, 0], vec![0, 0]],
            }))
            .is_err());
        // A non-frame-aligned byte plane is rejected.
        assert!(enc
            .send_frame(&Frame::Audio(AudioFrame {
                samples: 1,
                pts: None,
                data: vec![vec![0u8; 3]],
            }))
            .is_err());
    }

    #[test]
    fn sample_format_dispatch_covers_the_staged_depths() {
        assert_eq!(sample_format_for(8), Some(SampleFormat::U8));
        assert_eq!(sample_format_for(16), Some(SampleFormat::S16));
        assert_eq!(sample_format_for(24), Some(SampleFormat::S24));
        assert_eq!(sample_format_for(32), None);
        for (f, bits) in [
            (SampleFormat::U8, 8u16),
            (SampleFormat::S16, 16),
            (SampleFormat::S24, 24),
        ] {
            assert_eq!(bits_for_sample_format(f), Some(bits));
            assert_eq!(sample_format_for(bits), Some(f));
        }
        assert_eq!(bits_for_sample_format(SampleFormat::F32), None);
    }
}
