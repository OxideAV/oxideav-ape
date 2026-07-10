//! Whole-file decoder facade: header/tail parse
//! ([`crate::file_header`]) plus seek-table frame slicing plus the
//! vendor frame entropy layer ([`crate::frame`]), wired behind the
//! General-Decoding-Process boundary the [`crate::pipeline`] module
//! pins.
//!
//! The entropy layer is complete and validated bit-exact against
//! vendor-encoded fixtures; the passes between residual arrays and PCM
//! — the adaptive predictor cascade's per-version `delta[]`
//! maintenance, the per-stage `shift` position, and the X/Y
//! decorrelation orientation — remain pending further staged docs.
//! [`ApeDecoder::decode_frame`] therefore returns either **exact PCM**
//! (frames the flags fully determine: all-silent frames) or the coded
//! **residual arrays** (everything else), and never guesses at the
//! unpinned passes.

use crate::error::{Error, Result};
use crate::file_header::FileInfo;
use crate::frame::{decode_frame_residuals, FrameResiduals};
use crate::pipeline::DeltaSource;

/// One frame's decode outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameDecode {
    /// The frame's exact PCM, one array per channel — produced when
    /// the entropy layer alone fully determines it (all-silent
    /// frames).
    Pcm(Vec<Vec<i32>>),
    /// The entropy-layer residual arrays (one per **coded** array; a
    /// pseudo-stereo frame codes a single shared array). Turning these
    /// into PCM awaits the staged predictor material.
    Residuals(FrameResiduals),
}

/// Whole-file decoder over a borrowed file buffer.
#[derive(Debug, Clone)]
pub struct ApeDecoder<'a> {
    data: &'a [u8],
    info: FileInfo,
}

impl<'a> ApeDecoder<'a> {
    /// Parse the header/tail layout and bind the decoder to `data`
    /// (the whole file, junk prefix and trailing tag included).
    pub fn new(data: &'a [u8]) -> Result<Self> {
        let info = FileInfo::parse(data)?;
        Ok(ApeDecoder { data, info })
    }

    /// The parsed header/tail view.
    pub fn info(&self) -> &FileInfo {
        &self.info
    }

    /// Number of frames.
    pub fn frame_count(&self) -> u32 {
        self.info.total_frames
    }

    /// The raw byte slice of frame `index`, per the seek table.
    pub fn frame_bytes(&self, index: u32) -> Result<&'a [u8]> {
        let (start, end) = self.info.frame_byte_range(index)?;
        let (start, end) = (start as usize, end as usize);
        if end > self.data.len() || start > end {
            return Err(Error::Truncated);
        }
        Ok(&self.data[start..end])
    }

    /// The whole audio-data region — the frame bit array's word grid
    /// is anchored at its start.
    pub fn audio_region(&self) -> Result<&'a [u8]> {
        let start = self.info.audio_data_offset;
        let end = self.info.audio_data_end();
        let end = usize::try_from(end).map_err(|_| Error::Truncated)?;
        if end > self.data.len() || start > end {
            return Err(Error::Truncated);
        }
        Ok(&self.data[start..end])
    }

    /// Decode frame `index` through the entropy layer.
    pub fn frame_residuals(&self, index: u32) -> Result<FrameResiduals> {
        let (start, _) = self.info.frame_byte_range(index)?;
        let offset = (start as usize)
            .checked_sub(self.info.audio_data_offset)
            .ok_or(Error::Malformed("seek entry before the audio region"))?;
        decode_frame_residuals(
            self.audio_region()?,
            offset,
            self.info.version,
            self.info.channels,
            self.info.frame_blocks(index)?,
        )
    }

    /// Decode frame `index` as far as the staged material allows:
    /// exact PCM for flag-determined frames, residual arrays
    /// otherwise.
    pub fn decode_frame(&self, index: u32) -> Result<FrameDecode> {
        let out = self.frame_residuals(index)?;
        if out.silent {
            // All-silent: the residual arrays are the PCM (zeros), one
            // per channel, and the stored CRC can be checked now.
            Ok(FrameDecode::Pcm(out.arrays))
        } else {
            Ok(FrameDecode::Residuals(out))
        }
    }

    /// Verify frame `index`'s stored checksum against caller-supplied
    /// decoded PCM bytes (little-endian sample layout, channels
    /// interleaved — the stored WAV byte order).
    pub fn verify_frame_crc(&self, index: u32, pcm_bytes: &[u8]) -> Result<bool> {
        Ok(self
            .frame_residuals(index)?
            .prologue
            .matches_pcm_crc(pcm_bytes))
    }
}

/// [`DeltaSource`] adapter over one decoded frame, wiring the vendor
/// entropy layer behind the [`crate::pipeline::decode_frame`] walk:
/// the interleaved coded arrays are materialised once, then served
/// per-channel in the pinned unpack order. A pseudo-stereo frame
/// serves its single shared array to both channels.
#[derive(Debug, Clone)]
pub struct FrameDeltaSource {
    arrays: Vec<Vec<i32>>,
    pseudo_stereo: bool,
}

impl FrameDeltaSource {
    /// Build the source by running the entropy layer over the frame at
    /// `frame_byte_offset` within the `audio` region (see
    /// [`decode_frame_residuals`]).
    pub fn decode(
        audio: &[u8],
        frame_byte_offset: usize,
        file_version: u16,
        channels: u16,
        blocks: u32,
    ) -> Result<Self> {
        let out = decode_frame_residuals(audio, frame_byte_offset, file_version, channels, blocks)?;
        let pseudo_stereo = channels == 2 && out.arrays.len() == 1;
        Ok(FrameDeltaSource {
            arrays: out.arrays,
            pseudo_stereo,
        })
    }

    /// One decoded array per coded channel.
    pub fn arrays(&self) -> &[Vec<i32>] {
        &self.arrays
    }
}

impl DeltaSource for FrameDeltaSource {
    fn unpack_deltas(&mut self, channel: usize, out: &mut [i32]) -> Result<()> {
        let idx = if self.pseudo_stereo { 0 } else { channel };
        let arr = self
            .arrays
            .get(idx)
            .ok_or(Error::Malformed("channel index past coded arrays"))?;
        if arr.len() != out.len() {
            return Err(Error::Malformed("frame length disagrees with delta array"));
        }
        out.copy_from_slice(arr);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{decode_frame, CorrelationRounding, FrameChannels};

    #[test]
    fn frame_delta_source_serves_channels_in_unpack_order() {
        let src = FrameDeltaSource {
            arrays: vec![vec![1, 2, 3], vec![4, 5, 6]],
            pseudo_stereo: false,
        };
        let mut s = src.clone();
        let mut buf = [0i32; 3];
        s.unpack_deltas(0, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3]);
        s.unpack_deltas(1, &mut buf).unwrap();
        assert_eq!(buf, [4, 5, 6]);
        // Length mismatch is a hard error.
        let mut short = [0i32; 2];
        assert!(matches!(
            s.unpack_deltas(0, &mut short),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn pseudo_stereo_serves_the_shared_array_to_both_channels() {
        let mut src = FrameDeltaSource {
            arrays: vec![vec![7, 8]],
            pseudo_stereo: true,
        };
        let mut a = [0i32; 2];
        let mut b = [0i32; 2];
        src.unpack_deltas(0, &mut a).unwrap();
        src.unpack_deltas(1, &mut b).unwrap();
        assert_eq!(a, b);
        assert_eq!(a, [7, 8]);
    }

    #[test]
    fn delta_source_plugs_into_the_pinned_pipeline_walk() {
        // The wiki-pinned frame walk over the entropy boundary: no
        // filters (identity), stereo correlation over the two arrays.
        let mut src = FrameDeltaSource {
            arrays: vec![vec![10, 4], vec![4, 2]],
            pseudo_stereo: false,
        };
        let out = decode_frame(
            &mut src,
            FrameChannels::Stereo,
            2,
            |_ch, _arr| Ok(()),
            CorrelationRounding::TruncatingDiv,
        )
        .unwrap();
        assert_eq!(out, vec![vec![12, 5], vec![8, 3]]);
    }
}
