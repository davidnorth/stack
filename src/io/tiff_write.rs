//! Encode 16-bit output, compression, and extra tags.

use std::fmt;
use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

use tiff::encoder::colortype::{ColorType, RGB8, RGB16};
use tiff::encoder::{
    Compression as TiffCompression, DeflateLevel, ImageEncoder, Rational, TiffEncoder, TiffKind,
};
use tiff::tags::Tag;

use crate::io::metadata::{MetadataPolicy, OutputMetadata};
use crate::{Frame, Resolution, VERSION};

/// XMP packet tag (not in the `tiff` crate's named tag set).
const TAG_XMP: Tag = Tag::from_u16_exhaustive(700);

/// Output compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// No compression.
    None,
    /// LZW, the documented default.
    #[default]
    Lzw,
    /// Deflate/ZIP.
    Deflate,
}

impl From<Compression> for TiffCompression {
    fn from(value: Compression) -> Self {
        match value {
            Compression::None => TiffCompression::Uncompressed,
            Compression::Lzw => TiffCompression::Lzw,
            Compression::Deflate => TiffCompression::Deflate(DeflateLevel::default()),
        }
    }
}

/// Options for [`write_tiff`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteOptions {
    /// Strip compression.
    pub compression: Compression,
    /// Output bit depth: 8 or 16. Defaults to 16.
    pub bits: u8,
    /// Metadata copy policy.
    pub metadata: MetadataPolicy,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self {
            compression: Compression::Lzw,
            bits: 16,
            metadata: MetadataPolicy::Copy,
        }
    }
}

/// Error returned while encoding a TIFF file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffWriteError {
    path: PathBuf,
    detail: String,
}

impl TiffWriteError {
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

impl fmt::Display for TiffWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to write TIFF '{}': {}",
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for TiffWriteError {}

/// Writes `frame` as a classic TIFF with strips, `Orientation = 1`, and the
/// requested compression and bit depth.
pub fn write_tiff(
    path: impl AsRef<Path>,
    frame: &Frame,
    options: &WriteOptions,
) -> Result<(), TiffWriteError> {
    let path = path.as_ref();
    let file = File::create(path)
        .map_err(|err| TiffWriteError::new(path, format!("cannot create file: {err}")))?;
    encode_tiff(BufWriter::new(file), path, frame, options)
}

/// Encodes `frame` into an arbitrary writer. `path` is only used in errors.
pub fn encode_tiff<W: Write + Seek>(
    writer: W,
    path: &Path,
    frame: &Frame,
    options: &WriteOptions,
) -> Result<(), TiffWriteError> {
    validate_frame(path, frame, options)?;

    let metadata = OutputMetadata::from_frame(&frame.meta, options.metadata);
    let tiff = TiffEncoder::new(writer)
        .map_err(|err| tiff_error(path, "cannot write TIFF header", err))?;
    let mut tiff = tiff.with_compression(options.compression.into());

    match options.bits {
        16 => {
            let mut image = tiff
                .new_image::<RGB16>(frame.meta.width, frame.meta.height)
                .map_err(|err| tiff_error(path, "cannot start 16-bit image", err))?;
            write_tags(&mut image, &metadata, path)?;
            image
                .write_data(&frame.rgb)
                .map_err(|err| tiff_error(path, "cannot encode 16-bit image data", err))?;
        }
        8 => {
            let samples: Vec<u8> = frame.rgb.iter().copied().map(u16_to_u8).collect();
            let mut image = tiff
                .new_image::<RGB8>(frame.meta.width, frame.meta.height)
                .map_err(|err| tiff_error(path, "cannot start 8-bit image", err))?;
            write_tags(&mut image, &metadata, path)?;
            image
                .write_data(&samples)
                .map_err(|err| tiff_error(path, "cannot encode 8-bit image data", err))?;
        }
        _ => unreachable!("bit depth validated before encoding"),
    }

    Ok(())
}

fn validate_frame(
    path: &Path,
    frame: &Frame,
    options: &WriteOptions,
) -> Result<(), TiffWriteError> {
    if frame.meta.width == 0 || frame.meta.height == 0 {
        return Err(TiffWriteError::new(
            path,
            format!(
                "invalid image dimensions {}x{}",
                frame.meta.width, frame.meta.height
            ),
        ));
    }
    if options.bits != 8 && options.bits != 16 {
        return Err(TiffWriteError::new(
            path,
            format!(
                "unsupported output bit depth {} (expected 8 or 16)",
                options.bits
            ),
        ));
    }

    let expected = (frame.meta.width as usize)
        .checked_mul(frame.meta.height as usize)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or_else(|| TiffWriteError::new(path, "image dimensions overflow address space"))?;
    if frame.rgb.len() != expected {
        return Err(TiffWriteError::new(
            path,
            format!(
                "pixel buffer has {} samples, expected {expected}",
                frame.rgb.len()
            ),
        ));
    }
    if let Some(resolution) = frame.meta.resolution
        && (resolution.x_denominator == 0 || resolution.y_denominator == 0)
    {
        return Err(TiffWriteError::new(
            path,
            "resolution denominator must be non-zero",
        ));
    }

    Ok(())
}

fn write_tags<W: Write + Seek, C: ColorType, K: TiffKind>(
    image: &mut ImageEncoder<'_, W, C, K>,
    metadata: &OutputMetadata,
    path: &Path,
) -> Result<(), TiffWriteError> {
    let encoder = image.encoder();
    let write = |result: tiff::TiffResult<()>, context: &str| {
        result.map_err(|err| tiff_error(path, context, err))
    };

    write(
        encoder.write_tag(Tag::Orientation, 1u16),
        "cannot write orientation",
    )?;
    write(
        encoder.write_tag(Tag::Software, format!("focus-stack {VERSION}").as_str()),
        "cannot write software tag",
    )?;

    if let Some(icc) = &metadata.icc {
        write(
            encoder.write_tag(Tag::IccProfile, icc.as_slice()),
            "cannot write ICC profile",
        )?;
    }
    if let Some(xmp) = &metadata.xmp {
        write(
            encoder.write_tag(TAG_XMP, xmp.as_slice()),
            "cannot write XMP packet",
        )?;
    }
    if let Some(resolution) = metadata.resolution {
        write_resolution(encoder, resolution, path)?;
    }

    Ok(())
}

fn write_resolution<K: TiffKind>(
    encoder: &mut tiff::encoder::DirectoryEncoder<'_, impl Write + Seek, K>,
    resolution: Resolution,
    path: &Path,
) -> Result<(), TiffWriteError> {
    let write = |result: tiff::TiffResult<()>, context: &str| {
        result.map_err(|err| tiff_error(path, context, err))
    };

    write(
        encoder.write_tag(
            Tag::XResolution,
            Rational {
                n: resolution.x_numerator,
                d: resolution.x_denominator,
            },
        ),
        "cannot write X resolution",
    )?;
    write(
        encoder.write_tag(
            Tag::YResolution,
            Rational {
                n: resolution.y_numerator,
                d: resolution.y_denominator,
            },
        ),
        "cannot write Y resolution",
    )?;
    write(
        encoder.write_tag(Tag::ResolutionUnit, resolution.unit),
        "cannot write resolution unit",
    )
}

fn u16_to_u8(value: u16) -> u8 {
    ((u32::from(value) * 255 + 32_767) / 65_535) as u8
}

fn tiff_error(path: &Path, context: &str, err: tiff::TiffError) -> TiffWriteError {
    TiffWriteError::new(path, format!("{context}: {err}"))
}
