//! Decode strips/tiles, compression, predictor, BigTIFF.

use std::fmt;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::path::{Path, PathBuf};

use tiff::ColorType;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

use crate::io::exif;
use crate::{Frame, FrameMeta};

/// XMP packet tag (not in the `tiff` crate's named tag set).
const TAG_XMP: Tag = Tag::from_u16_exhaustive(700);

/// Error returned while reading or interpreting a TIFF file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TiffReadError {
    path: PathBuf,
    detail: String,
}

impl TiffReadError {
    fn new(path: &Path, detail: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            detail: detail.into(),
        }
    }

    /// Path of the file that failed to decode.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Human-readable reason for the failure.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for TiffReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to read TIFF '{}': {}",
            self.path.display(),
            self.detail
        )
    }
}

impl std::error::Error for TiffReadError {}

/// Reads the first image of a TIFF file, applying EXIF orientation.
///
/// The returned frame is always interleaved upright RGB. `meta.orientation`
/// retains the original tag value for the report; `meta.width` and
/// `meta.height` describe the upright buffer.
pub fn read_tiff(path: impl AsRef<Path>) -> Result<Frame, TiffReadError> {
    let path = path.as_ref();
    let file = File::open(path)
        .map_err(|err| TiffReadError::new(path, format!("cannot open file: {err}")))?;
    decode_tiff(BufReader::new(file), path)
}

/// Decodes a TIFF image from an arbitrary reader. `path` is used in errors.
pub fn decode_tiff<R: Read + Seek>(reader: R, path: &Path) -> Result<Frame, TiffReadError> {
    let mut decoder =
        Decoder::new(reader).map_err(|err| tiff_error(path, "invalid TIFF header", err))?;

    let (width, height) = decoder
        .dimensions()
        .map_err(|err| tiff_error(path, "cannot read image dimensions", err))?;
    if width == 0 || height == 0 {
        return Err(TiffReadError::new(
            path,
            format!("invalid image dimensions {width}x{height}"),
        ));
    }

    let photometric =
        unsigned_tag(&mut decoder, Tag::PhotometricInterpretation, path)?.unwrap_or(1);
    match photometric {
        0..=2 => {}
        3 => {
            return Err(TiffReadError::new(
                path,
                "palette-color TIFF is unsupported",
            ));
        }
        5 => {
            return Err(TiffReadError::new(path, "CMYK TIFF is unsupported"));
        }
        6 => {
            return Err(TiffReadError::new(path, "YCbCr TIFF is unsupported"));
        }
        8..=10 => {
            return Err(TiffReadError::new(path, "Lab TIFF is unsupported"));
        }
        other => {
            return Err(TiffReadError::new(
                path,
                format!("unsupported photometric interpretation {other}"),
            ));
        }
    }

    let color = decoder
        .colortype()
        .map_err(|err| tiff_error(path, "unsupported color layout", err))?;
    let samples = samples_per_pixel(&color);
    let bits = color.bit_depth();

    match (photometric, samples) {
        (0 | 1, 1 | 2) | (2, 3 | 4) => {}
        _ => {
            return Err(TiffReadError::new(
                path,
                format!(
                    "unsupported photometric interpretation {photometric} with {samples} samples per pixel"
                ),
            ));
        }
    }

    let compression = unsigned_tag(&mut decoder, Tag::Compression, path)?.unwrap_or(1);
    match compression {
        1 | 5 | 8 | 32_946 | 32_773 => {}
        other => {
            return Err(TiffReadError::new(
                path,
                format!(
                    "unsupported TIFF compression {other} (expected none, LZW, Deflate, or PackBits)"
                ),
            ));
        }
    }

    let planar = unsigned_tag(&mut decoder, Tag::PlanarConfiguration, path)?.unwrap_or(1);
    if planar != 1 {
        return Err(TiffReadError::new(
            path,
            format!("unsupported planar configuration {planar} (chunky is required)"),
        ));
    }

    if let Some(formats) = decoder
        .find_tag_unsigned_vec::<u16>(Tag::SampleFormat)
        .map_err(|err| tiff_error(path, "cannot read sample format", err))?
        && let Some(format) = formats.iter().copied().find(|format| *format != 1)
    {
        return Err(TiffReadError::new(
            path,
            format!(
                "unsupported sample format {format} (only unsigned integer samples are supported)"
            ),
        ));
    }

    if let Some(bits_per_sample) = decoder
        .find_tag_unsigned_vec::<u16>(Tag::BitsPerSample)
        .map_err(|err| tiff_error(path, "cannot read bits per sample", err))?
        && (bits_per_sample.is_empty()
            || bits_per_sample
                .iter()
                .any(|value| *value != u16::from(bits)))
    {
        return Err(TiffReadError::new(
            path,
            format!("unsupported bits per sample {bits_per_sample:?}"),
        ));
    }

    if bits != 8 && bits != 16 {
        return Err(TiffReadError::new(
            path,
            format!("unsupported bit depth {bits} (expected 8 or 16)"),
        ));
    }

    let orientation = unsigned_tag(&mut decoder, Tag::Orientation, path)?.unwrap_or(1);
    exif::oriented_dimensions(width, height, orientation)
        .map_err(|err| TiffReadError::new(path, err.to_string()))?;

    let icc = bytes_tag(&mut decoder, Tag::IccProfile, path)?;
    let xmp = bytes_tag(&mut decoder, TAG_XMP, path)?;

    let associated_alpha = decoder
        .find_tag_unsigned_vec::<u16>(Tag::ExtraSamples)
        .map_err(|err| tiff_error(path, "cannot read extra samples", err))?
        .and_then(|extra| extra.first().copied())
        .is_some_and(|kind| kind == 1);

    let decoded = decoder
        .read_image()
        .map_err(|err| tiff_error(path, "cannot decode image data", err))?;

    let samples_per_pixel = samples as usize;
    let rgb = match (decoded, bits) {
        (DecodingResult::U8(buffer), 8) => {
            convert_samples(&buffer, samples_per_pixel, true, associated_alpha)
        }
        (DecodingResult::U16(buffer), 16) => {
            convert_samples(&buffer, samples_per_pixel, false, associated_alpha)
        }
        (_, expected) => {
            return Err(TiffReadError::new(
                path,
                format!("decoder returned samples incompatible with {expected}-bit unsigned data"),
            ));
        }
    };

    let expected_len = (width as usize) * (height as usize) * 3;
    if rgb.len() != expected_len {
        return Err(TiffReadError::new(
            path,
            format!(
                "decoded pixel data has {} samples, expected {expected_len}",
                rgb.len()
            ),
        ));
    }

    let (out_width, out_height, rgb) = if orientation == 1 {
        (width, height, rgb)
    } else {
        let oriented = exif::apply_orientation(&rgb, width, height, orientation)
            .map_err(|err| TiffReadError::new(path, err.to_string()))?;
        (oriented.width, oriented.height, oriented.rgb)
    };

    Ok(Frame {
        meta: FrameMeta {
            path: path.to_path_buf(),
            width: out_width,
            height: out_height,
            bits,
            icc,
            orientation,
            // EXIF payload copy policy is implemented with the metadata work.
            exif: None,
            xmp,
        },
        rgb,
    })
}

fn samples_per_pixel(color: &ColorType) -> u16 {
    match color {
        ColorType::Gray(_) => 1,
        ColorType::GrayA(_) => 2,
        ColorType::RGB(_) => 3,
        ColorType::RGBA(_) => 4,
        ColorType::Multiband { num_samples, .. } => *num_samples,
        _ => color.num_samples(),
    }
}

fn unsigned_tag<R: Read + Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
    path: &Path,
) -> Result<Option<u16>, TiffReadError> {
    decoder
        .find_tag_unsigned::<u16>(tag)
        .map_err(|err| tiff_error(path, "cannot read tag", err))
}

fn bytes_tag<R: Read + Seek>(
    decoder: &mut Decoder<R>,
    tag: Tag,
    path: &Path,
) -> Result<Option<Vec<u8>>, TiffReadError> {
    match decoder
        .find_tag(tag)
        .map_err(|err| tiff_error(path, "cannot read tag", err))?
    {
        Some(value) => value
            .into_u8_vec()
            .map(Some)
            .map_err(|err| tiff_error(path, "cannot read tag bytes", err)),
        None => Ok(None),
    }
}

fn tiff_error(path: &Path, context: &str, err: tiff::TiffError) -> TiffReadError {
    TiffReadError::new(path, format!("{context}: {err}"))
}

fn convert_samples<T: Copy + Into<u32>>(
    samples: &[T],
    channels: usize,
    eight_bit: bool,
    associated_alpha: bool,
) -> Vec<u16> {
    let mut out = Vec::with_capacity(samples.len() / channels * 3);
    for pixel in samples.chunks_exact(channels) {
        let scale = |value: T| -> u16 {
            let value = value.into();
            if eight_bit {
                (value * 257) as u16
            } else {
                value as u16
            }
        };

        let rgb = match channels {
            1 => {
                let grey = scale(pixel[0]);
                [grey, grey, grey]
            }
            2 => {
                let grey = scale(pixel[0]);
                let alpha = scale(pixel[1]);
                let grey = maybe_unpremultiply(grey, alpha, associated_alpha);
                [grey, grey, grey]
            }
            3 => [scale(pixel[0]), scale(pixel[1]), scale(pixel[2])],
            4 => {
                let alpha = scale(pixel[3]);
                [
                    maybe_unpremultiply(scale(pixel[0]), alpha, associated_alpha),
                    maybe_unpremultiply(scale(pixel[1]), alpha, associated_alpha),
                    maybe_unpremultiply(scale(pixel[2]), alpha, associated_alpha),
                ]
            }
            _ => unreachable!("sample count validated before decoding"),
        };

        out.extend_from_slice(&rgb);
    }
    out
}

fn maybe_unpremultiply(value: u16, alpha: u16, associated: bool) -> u16 {
    if !associated || alpha == u16::MAX {
        return value;
    }
    if alpha == 0 {
        return 0;
    }
    ((value as u32 * u16::MAX as u32 + alpha as u32 / 2) / alpha as u32).min(u16::MAX as u32) as u16
}
