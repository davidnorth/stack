//! Metadata copy policy.
//!
//! Output TIFFs are intermediates for further editing, so the policy keeps the
//! fields that affect color and geometry (ICC and resolution) and the
//! self-contained descriptive packets (XMP and IPTC). EXIF is copied field by
//! field: common descriptive fields survive, while MakerNote is always
//! dropped because it cannot be relocated safely.

use crate::{ExifFields, FrameMeta, Resolution};

/// How much source metadata to carry into an output TIFF.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MetadataPolicy {
    /// Copy ICC, XMP, IPTC, common EXIF fields, and resolution.
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
    /// Raw IPTC-NAA record (tag 33723), copied verbatim.
    pub iptc: Option<Vec<u8>>,
    /// Common EXIF descriptive fields.
    pub exif: Option<ExifFields>,
    /// Resolution tags copied from the reference frame.
    pub resolution: Option<Resolution>,
    /// Source fields intentionally omitted from the output.
    pub dropped: Vec<DroppedField>,
}

/// A source metadata field that the copy policy omits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DroppedField {
    /// EXIF MakerNote is private to the source camera and is never copied.
    MakerNote,
    /// All EXIF fields were removed by [`MetadataPolicy::Strip`].
    Exif,
    /// XMP was removed by [`MetadataPolicy::Strip`].
    Xmp,
    /// IPTC was removed by [`MetadataPolicy::Strip`].
    Iptc,
}

impl OutputMetadata {
    /// Applies `policy` to the source frame metadata.
    pub fn from_frame(meta: &FrameMeta, policy: MetadataPolicy) -> Self {
        match policy {
            MetadataPolicy::Copy => {
                let mut dropped = Vec::new();
                if meta.exif.is_some() {
                    dropped.push(DroppedField::MakerNote);
                }
                Self {
                    icc: meta.icc.clone(),
                    xmp: meta.xmp.clone(),
                    iptc: meta.iptc.clone(),
                    exif: meta.exif.clone(),
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
                if meta.iptc.is_some() {
                    dropped.push(DroppedField::Iptc);
                }
                Self {
                    icc: meta.icc.clone(),
                    xmp: None,
                    iptc: None,
                    exif: None,
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
            exif: Some(ExifFields {
                make: Some("Focus Camera".to_string()),
                model: Some("Model 1".to_string()),
                ..ExifFields::default()
            }),
            xmp: Some(b"<xmp/>".to_vec()),
            iptc: Some(vec![0x1c, 0x02, 0x05]),
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
    fn copy_policy_keeps_icc_xmp_iptc_and_common_exif() {
        let output = OutputMetadata::from_frame(&meta(), MetadataPolicy::Copy);
        assert_eq!(output.icc, Some(vec![1, 2, 3]));
        assert_eq!(output.xmp, Some(b"<xmp/>".to_vec()));
        assert_eq!(output.iptc, Some(vec![0x1c, 0x02, 0x05]));
        assert_eq!(output.exif, meta().exif);
        assert_eq!(output.resolution, meta().resolution);
        assert_eq!(output.dropped, vec![DroppedField::MakerNote]);
    }

    #[test]
    fn strip_policy_keeps_only_color_and_geometry() {
        let output = OutputMetadata::from_frame(&meta(), MetadataPolicy::Strip);
        assert_eq!(output.icc, Some(vec![1, 2, 3]));
        assert_eq!(output.xmp, None);
        assert_eq!(output.iptc, None);
        assert_eq!(output.exif, None);
        assert_eq!(output.resolution, meta().resolution);
        assert_eq!(
            output.dropped,
            vec![DroppedField::Exif, DroppedField::Xmp, DroppedField::Iptc]
        );
    }

    #[test]
    fn copy_policy_without_exif_has_no_dropped_fields() {
        let mut source = meta();
        source.exif = None;
        let output = OutputMetadata::from_frame(&source, MetadataPolicy::Copy);
        assert!(output.dropped.is_empty());
    }
}
