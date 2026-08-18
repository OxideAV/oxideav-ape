//! Whole-file decoder facade: header/tail parse
//! ([`crate::file_header`]) plus seek-table frame slicing plus the
//! vendor frame entropy layer ([`crate::frame`]) plus the staged §6
//! predictor chain ([`crate::predict`] via [`crate::pcm`]).
//!
//! With the format reference's §6 staging the pipeline is complete for
//! files of version 3930 and above: [`ApeDecoder::decode_frame`]
//! returns the frame's **exact PCM** (validated byte-exact against the
//! vendor-encoded fixture corpus, each frame's stored CRC agreeing),
//! [`ApeDecoder::decode_frame_bytes`] additionally assembles the
//! stored interleaved byte order and **verifies the stored CRC**, and
//! [`ApeDecoder::decode_all_bytes`] walks every frame into one PCM
//! byte stream. Files below version 3930 sit outside the staged
//! predictor material (§6.2); for those `decode_frame` still returns
//! the entropy layer's residual arrays rather than guessing.

use crate::error::{Error, Result};
use crate::file_header::FileInfo;
use crate::frame::{decode_frame_residuals, FrameResiduals};
use crate::pcm::{frame_pcm, interleave_pcm_bytes};
use crate::pipeline::DeltaSource;

/// One frame's decode outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameDecode {
    /// The frame's exact PCM, one array per channel.
    Pcm(Vec<Vec<i32>>),
    /// The entropy-layer residual arrays (one per **coded** array; a
    /// pseudo-stereo frame codes a single shared array) — returned
    /// only for pre-3930 files, whose predictor pass is outside the
    /// staged material (§6.2).
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

    /// Rebind an already-parsed [`FileInfo`] to the buffer it was
    /// parsed from — lets a caller that keeps its own `FileInfo`
    /// (e.g. the framework registry adapter) skip re-walking the
    /// header/tail per frame. The buffer must be at least as long as
    /// the one the info was parsed from; every per-frame accessor
    /// still bounds-checks against the buffer it is handed.
    pub fn from_parsed(data: &'a [u8], info: FileInfo) -> Result<Self> {
        if data.len() < info.data_len() {
            return Err(Error::Truncated);
        }
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
    /// exact PCM for every file of version 3930 or above (and for
    /// flag-determined all-silent frames of any version), residual
    /// arrays for pre-3930 files whose predictor form is outside the
    /// staged material (§6.2).
    pub fn decode_frame(&self, index: u32) -> Result<FrameDecode> {
        let out = self.frame_residuals(index)?;
        if out.silent {
            // All-silent: the residual arrays are the PCM (zeros), one
            // per channel.
            return Ok(FrameDecode::Pcm(out.arrays));
        }
        match frame_pcm(
            &out,
            self.info.version,
            self.info.compression_level,
            self.info.channels,
        ) {
            Ok(pcm) => Ok(FrameDecode::Pcm(pcm)),
            Err(Error::NotImplemented) => Ok(FrameDecode::Residuals(out)),
            Err(e) => Err(e),
        }
    }

    /// Decode frame `index` to per-channel PCM samples. Unlike
    /// [`Self::decode_frame`] this never falls back to residual
    /// arrays: a pre-3930 file surfaces [`Error::NotImplemented`].
    pub fn decode_frame_pcm(&self, index: u32) -> Result<Vec<Vec<i32>>> {
        let out = self.frame_residuals(index)?;
        if out.silent {
            return Ok(out.arrays);
        }
        frame_pcm(
            &out,
            self.info.version,
            self.info.compression_level,
            self.info.channels,
        )
    }

    /// Decode frame `index` into the stored interleaved PCM byte order
    /// (the §6.9 per-bit-depth layout — what a WAV `data` chunk
    /// carries) and verify it against the frame's stored CRC.
    pub fn decode_frame_bytes(&self, index: u32) -> Result<Vec<u8>> {
        let out = self.frame_residuals(index)?;
        let pcm = if out.silent {
            out.arrays.clone()
        } else {
            frame_pcm(
                &out,
                self.info.version,
                self.info.compression_level,
                self.info.channels,
            )?
        };
        let bytes = interleave_pcm_bytes(&pcm, self.info.bits_per_sample)?;
        if !out.prologue.matches_pcm_crc(&bytes) {
            return Err(Error::Malformed(
                "stored frame CRC disagrees with the decoded PCM",
            ));
        }
        Ok(bytes)
    }

    /// Decode the whole file into one interleaved PCM byte stream,
    /// every frame CRC-verified.
    pub fn decode_all_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        for i in 0..self.frame_count() {
            out.extend_from_slice(&self.decode_frame_bytes(i)?);
        }
        Ok(out)
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
