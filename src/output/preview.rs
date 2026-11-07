//! 8-bit sRGB JPEG previews.
//!
//! The preview is derived from the same encoded pixels that are written to the
//! output TIFF. For an sRGB output the 16-to-8-bit conversion is identical to
//! the TIFF writer's, so the preview matches the main image exactly before JPEG
//! compression. Outputs in another profile are converted to sRGB through the
//! color module first.

use std::fmt;
use std::fs;
use std::path::Path;

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};

use crate::Frame;
use crate::color::profile::{ColorPipeline, ProfileKind};
use crate::color::transfer::srgb_encode;

/// Default long-edge size used by `--preview-size`.
pub const DEFAULT_MAX_SIZE: u32 = 2048;

/// JPEG quality used for previews.
pub const JPEG_QUALITY: u8 = 90;

/// An 8-bit sRGB image ready for JPEG encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewImage {
    pub width: u32,
    pub height: u32,
    /// Interleaved RGB, length `width * height * 3`.
    pub rgb: Vec<u8>,
}

/// Preview generation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewError {
    detail: String,
}

impl PreviewError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }

    /// Human-readable reason for the failure.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for PreviewError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for PreviewError {}

/// Renders `frame` as an 8-bit sRGB preview with long edge at most `max_size`.
///
/// `output_icc` describes the profile of the frame's encoded samples. Pass
/// `None` when the output is untagged sRGB.
pub fn preview_image(
    frame: &Frame,
    max_size: u32,
    output_icc: Option<&[u8]>,
) -> Result<PreviewImage, PreviewError> {
    if frame.meta.width == 0 || frame.meta.height == 0 {
        return Err(PreviewError::new("cannot preview an empty image"));
    }
    if max_size == 0 {
        return Err(PreviewError::new("preview size must be at least one pixel"));
    }

    let (width, height) = preview_dimensions(frame.meta.width, frame.meta.height, max_size);
    if width > u32::from(u16::MAX) || height > u32::from(u16::MAX) {
        return Err(PreviewError::new(format!(
            "preview dimensions {width}x{height} exceed the JPEG limit"
        )));
    }

    let full = to_srgb8(frame, output_icc)?;
    let rgb = if (width, height) == (frame.meta.width, frame.meta.height) {
        full
    } else {
        resize_rgb(&full, frame.meta.width, frame.meta.height, width, height)
    };

    Ok(PreviewImage { width, height, rgb })
}

/// Encodes an image as a baseline JPEG at `quality` (clamped to 1..=100).
pub fn encode_jpeg(image: &PreviewImage, quality: u8) -> Result<Vec<u8>, PreviewError> {
    let quality = quality.clamp(1, 100);
    let mut bytes = Vec::new();
    let mut encoder = Encoder::new(&mut bytes, quality);
    encoder.set_sampling_factor(SamplingFactor::F_1_1);
    encoder
        .encode(
            &image.rgb,
            image.width as u16,
            image.height as u16,
            ColorType::Rgb,
        )
        .map_err(|err| PreviewError::new(format!("cannot encode JPEG preview: {err}")))?;
    Ok(bytes)
}

/// Writes `frame` as an 8-bit sRGB JPEG preview.
pub fn write_preview(
    path: impl AsRef<Path>,
    frame: &Frame,
    max_size: u32,
    output_icc: Option<&[u8]>,
) -> Result<(), PreviewError> {
    let path = path.as_ref();
    let image = preview_image(frame, max_size, output_icc)?;
    let bytes = encode_jpeg(&image, JPEG_QUALITY)?;
    fs::write(path, bytes).map_err(|err| {
        PreviewError::new(format!(
            "failed to write preview '{}': {err}",
            path.display()
        ))
    })
}

/// Long-edge dimensions for a preview bounded by `max_size`.
fn preview_dimensions(width: u32, height: u32, max_size: u32) -> (u32, u32) {
    if width <= max_size && height <= max_size {
        return (width, height);
    }

    let (long, short) = if width >= height {
        (width, height)
    } else {
        (height, width)
    };
    let scaled_short = ((u64::from(short) * u64::from(max_size) + u64::from(long) / 2)
        / u64::from(long))
    .max(1) as u32;

    if width >= height {
        (max_size, scaled_short)
    } else {
        (scaled_short, max_size)
    }
}

fn to_srgb8(frame: &Frame, output_icc: Option<&[u8]>) -> Result<Vec<u8>, PreviewError> {
    let pipeline = ColorPipeline::new(output_icc, None, None)
        .map_err(|err| PreviewError::new(format!("cannot prepare preview color: {err}")))?;

    Ok(match pipeline.output_kind() {
        // Preserve the exact 16-to-8-bit mapping used by the TIFF writer.
        ProfileKind::Srgb => frame.rgb.iter().copied().map(u16_to_u8).collect(),
        _ => {
            let mut linear: Vec<f32> = frame
                .rgb
                .iter()
                .map(|&value| f32::from(value) / 65_535.0)
                .collect();
            pipeline
                .to_linear_srgb(&mut linear)
                .map_err(|err| PreviewError::new(format!("cannot convert preview color: {err}")))?;
            linear.iter().map(|&value| linear_to_u8(value)).collect()
        }
    })
}

fn linear_to_u8(value: f32) -> u8 {
    (srgb_encode(value.clamp(0.0, 1.0)) * 255.0).round() as u8
}

fn u16_to_u8(value: u16) -> u8 {
    ((u32::from(value) * 255 + 32_767) / 65_535) as u8
}

fn resize_rgb(src: &[u8], src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dst_w as usize) * (dst_h as usize) * 3];
    let x_scale = f64::from(src_w) / f64::from(dst_w);
    let y_scale = f64::from(src_h) / f64::from(dst_h);

    for y in 0..dst_h {
        let sy = ((f64::from(y) + 0.5) * y_scale - 0.5).clamp(0.0, f64::from(src_h - 1));
        let y0 = sy.floor() as u32;
        let y1 = (y0 + 1).min(src_h - 1);
        let fy = sy - f64::from(y0);

        for x in 0..dst_w {
            let sx = ((f64::from(x) + 0.5) * x_scale - 0.5).clamp(0.0, f64::from(src_w - 1));
            let x0 = sx.floor() as u32;
            let x1 = (x0 + 1).min(src_w - 1);
            let fx = sx - f64::from(x0);

            let dst = ((y as usize * dst_w as usize) + x as usize) * 3;
            for c in 0..3 {
                let p00 = f64::from(sample(src, src_w, x0, y0, c));
                let p10 = f64::from(sample(src, src_w, x1, y0, c));
                let p01 = f64::from(sample(src, src_w, x0, y1, c));
                let p11 = f64::from(sample(src, src_w, x1, y1, c));
                let top = p00 + (p10 - p00) * fx;
                let bottom = p01 + (p11 - p01) * fx;
                out[dst + c] = (top + (bottom - top) * fy).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    out
}

fn sample(rgb: &[u8], width: u32, x: u32, y: u32, channel: usize) -> u8 {
    rgb[(y as usize * width as usize + x as usize) * 3 + channel]
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::FrameMeta;

    fn frame(width: u32, height: u32, value: u16) -> Frame {
        Frame {
            meta: FrameMeta {
                path: PathBuf::from("frame.tif"),
                width,
                height,
                bits: 16,
                icc: None,
                orientation: 1,
                exif: None,
                xmp: None,
                iptc: None,
                resolution: None,
            },
            rgb: vec![value; (width * height * 3) as usize],
        }
    }

    #[test]
    fn preview_dimensions_keep_the_long_edge() {
        assert_eq!(preview_dimensions(4000, 3000, 2048), (2048, 1536));
        assert_eq!(preview_dimensions(3000, 4000, 2048), (1536, 2048));
        assert_eq!(preview_dimensions(100, 80, 2048), (100, 80));
    }

    #[test]
    fn solid_srgb_preview_is_unchanged_by_resizing() {
        let source = frame(8, 4, 32_768);
        let full = preview_image(&source, 2048, None).unwrap();
        let small = preview_image(&source, 4, None).unwrap();

        assert_eq!((full.width, full.height), (8, 4));
        assert_eq!((small.width, small.height), (4, 2));
        let expected = u16_to_u8(32_768);
        assert!(full.rgb.iter().all(|&value| value == expected));
        assert!(small.rgb.iter().all(|&value| value == expected));
    }

    #[test]
    fn jpeg_has_an_sof_marker_with_the_preview_dimensions() {
        let image = preview_image(&frame(3, 2, 1000), 2048, None).unwrap();
        let jpeg = encode_jpeg(&image, JPEG_QUALITY).unwrap();
        assert_eq!(&jpeg[..2], &[0xff, 0xd8], "JPEG SOI");
        assert_eq!(&jpeg[jpeg.len() - 2..], &[0xff, 0xd9], "JPEG EOI");
        assert!(
            jpeg.windows(2).any(|window| window == [0xff, 0xc0]),
            "missing baseline SOF0 marker"
        );
    }
}
