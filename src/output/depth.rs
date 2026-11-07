//! 16-bit depth and source maps.
//!
//! Implements `docs/outputs.md` §2: the continuous DMap depth is encoded as
//! `round(d / (N - 1) * 65535)` in a 16-bit grayscale TIFF.

use std::fmt;
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use tiff::encoder::colortype::Gray16;
use tiff::encoder::{Compression as TiffCompression, TiffEncoder};
use tiff::tags::Tag;

use crate::VERSION;

/// Default compression for depth maps.
const DEPTH_COMPRESSION: TiffCompression = TiffCompression::Lzw;

/// Error returned while encoding a depth map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepthWriteError {
    path: PathBuf,
    detail: String,
}

impl DepthWriteError {
    fn new(path: &Path, detail: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }

    /// Path of the file that failed to encode.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Human-readable reason for the failure.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for DepthWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to write depth map '{}': {}",
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for DepthWriteError {}

/// Metadata about a written depth map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DepthMapReport {
    /// Input frame count used for the encoding.
    pub frames: usize,
    /// True when `frames == 1`; the map is all zeros and the caller should warn.
    pub all_zero: bool,
}

/// Encode a continuous frame index as the §2 depth value.
#[must_use]
pub fn encode_depth(depth: f32, frames: usize) -> u16 {
    if frames <= 1 {
        return 0;
    }
    let last = (frames - 1) as f32;
    let normalized = if depth.is_finite() {
        (depth / last).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (normalized * f32::from(u16::MAX)).round() as u16
}

/// Decode a §2 depth value back to a continuous frame index.
#[must_use]
pub fn decode_depth(value: u16, frames: usize) -> f32 {
    if frames <= 1 {
        0.0
    } else {
        f32::from(value) / f32::from(u16::MAX) * (frames - 1) as f32
    }
}

/// ImageDescription written into every depth map.
#[must_use]
pub fn depth_description(frames: usize) -> String {
    format!("focus-stack depth map; frames={frames}; 0=first,65535=last")
}

/// Write `depth` as a 16-bit grayscale TIFF from `docs/outputs.md` §2.
///
/// `depth` is row-major continuous frame depth. For a single input frame the
/// map is written as all zeros and [`DepthMapReport::all_zero`] is set so the
/// caller can warn.
pub fn write_depth_map(
    path: impl AsRef<Path>,
    depth: &[f32],
    width: u32,
    height: u32,
    frames: usize,
) -> Result<DepthMapReport, DepthWriteError> {
    let path = path.as_ref();
    if width == 0 || height == 0 {
        return Err(DepthWriteError::new(
            path,
            format!("invalid image dimensions {width}x{height}"),
        ));
    }
    if frames == 0 {
        return Err(DepthWriteError::new(
            path,
            "at least one input frame is required",
        ));
    }
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or_else(|| DepthWriteError::new(path, "image dimensions overflow address space"))?;
    if depth.len() != count {
        return Err(DepthWriteError::new(
            path,
            format!("depth buffer has {} samples, expected {count}", depth.len()),
        ));
    }

    let samples: Vec<u16> = depth
        .iter()
        .map(|&value| encode_depth(value, frames))
        .collect();

    let file = File::create(path)
        .map_err(|err| DepthWriteError::new(path, format!("cannot create file: {err}")))?;
    let writer = BufWriter::new(file);
    let mut tiff = TiffEncoder::new(writer)
        .map_err(|err| DepthWriteError::new(path, format!("cannot write TIFF header: {err}")))?
        .with_compression(DEPTH_COMPRESSION);
    let mut image = tiff
        .new_image::<Gray16>(width, height)
        .map_err(|err| DepthWriteError::new(path, format!("cannot start image: {err}")))?;
    {
        let encoder = image.encoder();
        encoder
            .write_tag(Tag::ImageDescription, depth_description(frames).as_str())
            .map_err(|err| {
                DepthWriteError::new(path, format!("cannot write image description: {err}"))
            })?;
        encoder
            .write_tag(Tag::Software, format!("focus-stack {VERSION}").as_str())
            .map_err(|err| {
                DepthWriteError::new(path, format!("cannot write software tag: {err}"))
            })?;
    }
    image
        .write_data(&samples)
        .map_err(|err| DepthWriteError::new(path, format!("cannot encode image data: {err}")))?;

    Ok(DepthMapReport {
        frames,
        all_zero: frames == 1,
    })
}
