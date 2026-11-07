//! `focus-stack` library surface.
//!
//! The module tree mirrors `docs/architecture.md` §2. Implementations are
//! filled in by later sessions; this file fixes the public layout and the
//! shared data model from `docs/architecture.md` §3.

pub mod align;
pub mod cli;
pub mod color;
pub mod focus;
pub mod io;
pub mod output;
pub mod pipeline;
pub mod quality;
pub mod retouch;
pub mod stack;

use std::path::PathBuf;

/// Crate version, taken from `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// TIFF resolution tags (282/283/296), copied verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub x_numerator: u32,
    pub x_denominator: u32,
    pub y_numerator: u32,
    pub y_denominator: u32,
    /// TIFF resolution unit (296): 1 = none, 2 = inch, 3 = centimetre.
    pub unit: u16,
}

/// EXIF descriptive fields that can be copied safely to an output TIFF.
///
/// MakerNote is deliberately not represented: it is private to the source
/// camera and cannot be relocated safely.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ExifFields {
    pub image_description: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub date_time: Option<String>,
    pub artist: Option<String>,
    pub copyright: Option<String>,
}

impl ExifFields {
    /// Whether every field is absent.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.image_description.is_none()
            && self.make.is_none()
            && self.model.is_none()
            && self.date_time.is_none()
            && self.artist.is_none()
            && self.copyright.is_none()
    }
}

/// Per-frame metadata extracted during load.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameMeta {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Source bit depth: 8 or 16.
    pub bits: u8,
    /// Raw ICC profile bytes, if the source contains one.
    pub icc: Option<Vec<u8>>,
    /// EXIF orientation tag (274).
    pub orientation: u16,
    /// Common EXIF descriptive fields, subject to the metadata copy policy.
    pub exif: Option<ExifFields>,
    /// Raw XMP packet (tag 700), if present.
    pub xmp: Option<Vec<u8>>,
    /// Raw IPTC-NAA record (tag 33723), if present.
    pub iptc: Option<Vec<u8>>,
    /// Resolution tags (282/283/296), if present.
    pub resolution: Option<Resolution>,
}

/// Decoded frame with interleaved RGB samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub meta: FrameMeta,
    /// Interleaved RGB, length `width * height * 3`.
    pub rgb: Vec<u16>,
}

/// Similarity transform mapping output pixel coordinates to source pixel
/// coordinates:
///
/// `x' = a*x - b*y + tx`
///
/// `y' = b*x + a*y + ty`
#[derive(Debug, Clone, PartialEq)]
pub struct Similarity {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

/// Axis-aligned rectangle in pixel coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// Alignment result for a set of frames.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedSet {
    pub reference: usize,
    pub transforms: Vec<Similarity>,
    pub valid: Rect,
}

/// Laplacian pyramid for one channel of a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusPyramid {
    pub levels: usize,
    /// Per level laplacian coefficients, row-major.
    pub laplacian: Vec<Vec<f32>>,
}

/// Continuous depth and confidence field.
#[derive(Debug, Clone, PartialEq)]
pub struct DepthField {
    /// Continuous frame index in `0..N-1`.
    pub d: Vec<f32>,
    /// Confidence in `0..1`.
    pub confidence: Vec<f32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crate_exposes_version() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn frame_buffer_is_interleaved_rgb() {
        let width = 4;
        let height = 3;
        let frame = Frame {
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
            rgb: vec![0; (width * height * 3) as usize],
        };

        assert_eq!(frame.rgb.len(), (width * height * 3) as usize);
    }
}
