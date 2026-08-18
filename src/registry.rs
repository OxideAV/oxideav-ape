//! `oxideav-core` framework wire-up (behind the default-on `registry`
//! cargo feature): the codec registration, the packet-facing decoder
//! adapter, and the direct `make_decoder` factory.
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

use crate::decoder::ApeDecoder;
use crate::error::Error as ApeError;
use crate::file_header::FileInfo;
use oxideav_core::registry::CodecInfo;
use oxideav_core::{
    AudioFrame, CodecCapabilities, CodecId, CodecParameters, Decoder, Error as CoreError, Frame,
    Packet, Result as CoreResult, RuntimeContext, SampleFormat,
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

/// Install this crate's codec registration into `ctx`: the `"ape"`
/// codec id with the whole-file decoder factory and the `'MAC '`
/// payload-magic claim (an `.ape` file identifies itself by its own
/// leading bytes; no container tag exists for the native carriage).
pub fn register(ctx: &mut RuntimeContext) {
    let mut caps = CodecCapabilities::audio("ape_sw");
    caps.lossless = true;
    caps.max_channels = Some(2);
    ctx.codecs.register(
        CodecInfo::new(CodecId::new(CODEC_ID))
            .capabilities(caps)
            .decoder(make_decoder)
            .payload_magic(crate::header::MAGIC),
    );
}

oxideav_core::register!("ape", register);

#[cfg(test)]
mod tests {
    use super::*;
    use oxideav_core::TimeBase;

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

    #[test]
    fn sample_format_dispatch_covers_the_staged_depths() {
        assert_eq!(sample_format_for(8), Some(SampleFormat::U8));
        assert_eq!(sample_format_for(16), Some(SampleFormat::S16));
        assert_eq!(sample_format_for(24), Some(SampleFormat::S24));
        assert_eq!(sample_format_for(32), None);
    }
}
