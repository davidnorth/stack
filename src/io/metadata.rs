//! Metadata copy policy.
//!
//! Output TIFFs are intermediates for further editing, so the policy keeps the
//! fields that affect color and geometry (ICC and resolution) and the
//! self-contained descriptive packets (XMP). EXIF needs translation rather
//! than a raw copy; that work belongs to the metadata output task.

use crate::{FrameMeta, Resolution};

/// How much source metadata to carry into an output TIFF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataPolicy {
    /// Copy ICC, XMP, and resolution; drop fields that cannot be relocated safely.
    #[default]
    Copy,
    /// Keep only ICC, orientation, resolution, and the Software tag.
    Strip,
}

/// Metadata selected for a single output frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct OutputMetadata {
    /// ICC profile bytes, copied verbatim.
    pub icc: Option<Vec<u8>>,
    /// Raw XMP packet (tag 700), copied verbatim.
    pub xmp: Option<Vec<u8>>,
    /// Resolution tags copied from the reference frame.
    pub resolution: Option<Resolution>,
    /// Source fields intentionally omitted from the output.
    pub dropped: Vec<DroppedField>,
}

/// A source metadata field that the copy policy omits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DroppedField {
    /// EXIF needs field-by-field translation and MakerNote removal.
    Exif,
    /// XMP was removed by [`MetadataPolicy::Strip`].
    Xmp,
}

impl OutputMetadata {
    /// Applies `policy` to the source frame metadata.
    pub fn from_frame(meta: &FrameMeta, policy: MetadataPolicy) -> Self {
        match policy {
            MetadataPolicy::Copy => {
                let mut dropped = Vec::new();
                if meta.exif.is_some() {
                    dropped.push(DroppedField::Exif);
                }
                Self {
                    icc: meta.icc.clone(),
                    xmp: meta.xmp.clone(),
                    resolution: meta.resolution,
                    dropped,
                }
            }
            MetadataPolicy::Strip => {
                let mut dropped = Vec::new();
                if meta.exif.is_some() {
                    dropped.push(DroppedField::Exif);
                }
                if meta.xmp.is_some() {
                    dropped.push(DroppedField::Xmp);
                }
                Self {
                    icc: meta.icc.clone(),
                    xmp: None,
                    resolution: meta.resolution,
                    dropped,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn meta() -> FrameMeta {
        FrameMeta {
            path: PathBuf::from("frame.tif"),
            width: 2,
            height: 1,
            bits: 16,
            icc: Some(vec![1, 2, 3]),
            orientation: 1,
            exif: Some(vec![4, 5]),
            xmp: Some(b"<xmp/>".to_vec()),
            resolution: Some(Resolution {
                x_numerator: 300,
                x_denominator: 1,
                y_numerator: 300,
                y_denominator: 1,
                unit: 2,
            }),
        }
    }

    #[test]
    fn copy_policy_keeps_icc_xmp_and_resolution() {
        let output = OutputMetadata::from_frame(&meta(), MetadataPolicy::Copy);
        assert_eq!(output.icc, Some(vec![1, 2, 3]));
        assert_eq!(output.xmp, Some(b"<xmp/>".to_vec()));
        assert_eq!(output.resolution, meta().resolution);
        assert_eq!(output.dropped, vec![DroppedField::Exif]);
    }

    #[test]
    fn strip_policy_keeps_only_color_and_geometry() {
        let output = OutputMetadata::from_frame(&meta(), MetadataPolicy::Strip);
        assert_eq!(output.icc, Some(vec![1, 2, 3]));
        assert_eq!(output.xmp, None);
        assert_eq!(output.resolution, meta().resolution);
        assert_eq!(output.dropped, vec![DroppedField::Exif, DroppedField::Xmp]);
    }
}
