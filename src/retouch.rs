//! Mask JSON driven recompositing.
//!
//! Implements the retouch format from `docs/outputs.md` §6 and the `retouch`
//! subcommand from `docs/cli.md` §4. Alignment transforms come from the run
//! report, so the base image is never re-aligned or modified; masks are
//! rasterized in output pixel space and composited in linear light.

use std::fmt;
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::Similarity;
use crate::align::warp::warp_linear;
use crate::cli::RetouchArgs;
use crate::color::transfer::{srgb_decode, srgb_encode};
use crate::io::tiff_read::read_tiff;
use crate::io::tiff_write::{self, WriteOptions};
use crate::output::report::sha256_file;

/// Mask JSON schema version understood by this module.
pub const MASK_VERSION: u32 = 1;

/// Report schema version understood by this module.
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Retouch failure with the exit code documented in `docs/cli.md` §5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetouchError {
    /// Bad arguments that clap cannot catch.
    Usage(String),
    /// The mask JSON or a referenced PNG is invalid.
    InvalidMask(String),
    /// The run report is invalid or incomplete.
    InvalidReport(String),
    /// An input is missing, corrupt, or does not match the report.
    Input(String),
    /// A file could not be read or written.
    Io(String),
}

impl RetouchError {
    /// Process exit code for this failure.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::InvalidMask(_) | Self::InvalidReport(_) | Self::Input(_) | Self::Io(_) => 3,
        }
    }
}

impl fmt::Display for RetouchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message)
            | Self::InvalidMask(message)
            | Self::InvalidReport(message)
            | Self::Input(message)
            | Self::Io(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for RetouchError {}

/// Parsed mask document.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    /// Mask schema version.
    pub version: u32,
    /// Masks in application order: the order they appear in the document.
    pub items: Vec<MaskItem>,
}

/// One mask in application order.
#[derive(Debug, Clone, PartialEq)]
pub enum MaskItem {
    /// A round-capped polyline.
    Stroke(StrokeMask),
    /// A filled polygon.
    Polygon(PolygonMask),
    /// A grayscale PNG in output pixel space.
    Png(PngMask),
}

impl MaskItem {
    /// Zero-based input index selected by this mask.
    #[must_use]
    pub fn source(&self) -> usize {
        match self {
            Self::Stroke(mask) => mask.source,
            Self::Polygon(mask) => mask.source,
            Self::Png(mask) => mask.source,
        }
    }

    /// Gaussian feather radius in pixels; zero is a hard edge.
    #[must_use]
    pub fn feather(&self) -> f64 {
        match self {
            Self::Stroke(mask) => mask.feather,
            Self::Polygon(mask) => mask.feather,
            Self::Png(mask) => mask.feather,
        }
    }
}

/// A stroked polyline.
#[derive(Debug, Clone, PartialEq)]
pub struct StrokeMask {
    /// Polyline points in output pixel coordinates.
    pub points: Vec<(f64, f64)>,
    /// Half-width in pixels.
    pub radius: f64,
    /// Gaussian feather radius in pixels.
    pub feather: f64,
    /// Zero-based input index.
    pub source: usize,
}

/// A filled polygon.
#[derive(Debug, Clone, PartialEq)]
pub struct PolygonMask {
    /// Polygon vertices in output pixel coordinates.
    pub points: Vec<(f64, f64)>,
    /// Gaussian feather radius in pixels.
    pub feather: f64,
    /// Zero-based input index.
    pub source: usize,
}

/// A grayscale PNG mask.
#[derive(Debug, Clone, PartialEq)]
pub struct PngMask {
    /// Path exactly as written in the mask JSON.
    pub path: String,
    /// Gaussian feather radius in pixels.
    pub feather: f64,
    /// Zero-based input index.
    pub source: usize,
}

/// One input record read from a run report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportInput {
    /// Path as recorded in the report, when present.
    pub path: Option<String>,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Pixel width, when present.
    pub width: Option<u32>,
    /// Pixel height, when present.
    pub height: Option<u32>,
}

/// The parts of a run report needed for a deterministic retouch.
#[derive(Debug, Clone, PartialEq)]
pub struct RetouchReport {
    /// Report schema version.
    pub schema_version: u32,
    /// Input records in command-line order.
    pub inputs: Vec<ReportInput>,
    /// Index of the reference frame.
    pub reference_index: usize,
    /// Per-input transforms mapping output coordinates to source coordinates.
    pub transforms: Vec<Similarity>,
    /// SHA-256 of the base file, when the report records it.
    pub output_sha256: Option<String>,
}

/// A retouch request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetouchRequest {
    /// Stacked output to use as the base.
    pub base: PathBuf,
    /// Original input frames in report order.
    pub sources: Vec<PathBuf>,
    /// Report from the same run as the base.
    pub report: PathBuf,
    /// Mask JSON.
    pub mask: PathBuf,
    /// Destination TIFF; defaults to `<base-stem>-retouched.tif`.
    pub output: Option<PathBuf>,
}

/// Result of a successful retouch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetouchOutcome {
    /// Written TIFF.
    pub output: PathBuf,
    /// Output width in pixels.
    pub width: u32,
    /// Output height in pixels.
    pub height: u32,
    /// Number of masks applied.
    pub masks_applied: usize,
}

/// Runs the `retouch` subcommand.
pub fn run(args: &RetouchArgs) -> Result<(), RetouchError> {
    let request = RetouchRequest {
        base: args.base.clone(),
        sources: args.sources.clone(),
        report: args.report.clone(),
        mask: args.mask.clone(),
        output: args.destination.clone(),
    };
    retouch(&request).map(|_| ())
}

/// Applies a mask document to a stacked output.
///
/// The base and every selected source are decoded to linear light. Sources are
/// warped with the transforms stored in the report, masks update the result in
/// document order, and the result is encoded once at the end. The base file is
/// only read.
///
/// # Errors
///
/// Returns [`RetouchError`] when a file cannot be read or written, the report or
/// mask is invalid, an input hash does not match, or the output would overwrite
/// the base.
pub fn retouch(request: &RetouchRequest) -> Result<RetouchOutcome, RetouchError> {
    let output = request
        .output
        .clone()
        .unwrap_or_else(|| default_output_path(&request.base));
    if same_path(&output, &request.base) {
        return Err(RetouchError::Usage(format!(
            "refusing to overwrite the base image '{}'",
            request.base.display()
        )));
    }

    let report_text = fs::read_to_string(&request.report).map_err(|err| {
        RetouchError::Io(format!(
            "failed to read report '{}': {err}",
            request.report.display()
        ))
    })?;
    let report = parse_report(&report_text)?;

    let mask_text = fs::read_to_string(&request.mask).map_err(|err| {
        RetouchError::Io(format!(
            "failed to read mask '{}': {err}",
            request.mask.display()
        ))
    })?;
    let mask = parse_mask(&mask_text)?;

    validate_sources(request, &report, &mask)?;

    let base = read_tiff(&request.base).map_err(|err| RetouchError::Input(err.to_string()))?;
    if base.meta.bits != 8 && base.meta.bits != 16 {
        return Err(RetouchError::Input(format!(
            "base '{}' has unsupported bit depth {}",
            request.base.display(),
            base.meta.bits
        )));
    }
    let width = base.meta.width;
    let height = base.meta.height;

    let mut warped_sources = Vec::with_capacity(request.sources.len());
    for (index, path) in request.sources.iter().enumerate() {
        let frame = read_tiff(path).map_err(|err| RetouchError::Input(err.to_string()))?;
        if let Some(expected_width) = report.inputs[index].width
            && expected_width != frame.meta.width
        {
            return Err(RetouchError::Input(format!(
                "source '{}' is {} px wide but the report records {expected_width}",
                path.display(),
                frame.meta.width
            )));
        }
        if let Some(expected_height) = report.inputs[index].height
            && expected_height != frame.meta.height
        {
            return Err(RetouchError::Input(format!(
                "source '{}' is {} px high but the report records {expected_height}",
                path.display(),
                frame.meta.height
            )));
        }

        let encoded: Vec<f32> = frame
            .rgb
            .iter()
            .map(|&value| f32::from(value) / 65_535.0)
            .collect();
        let mut linear: Vec<f32> = encoded.iter().copied().map(srgb_decode).collect();
        let warped = warp_linear(
            &linear,
            frame.meta.width,
            frame.meta.height,
            width,
            height,
            3,
            &report.transforms[index],
        )
        .map_err(|err| {
            RetouchError::InvalidReport(format!(
                "transform {} in report is unusable: {err}",
                index + 1
            ))
        })?;
        linear.clear();
        warped_sources.push(warped);
    }

    let mut result: Vec<f32> = base
        .rgb
        .iter()
        .map(|&value| srgb_decode(f32::from(value) / 65_535.0))
        .collect();

    let mask_directory = request.mask.parent().unwrap_or_else(|| Path::new(""));
    for item in &mask.items {
        let mut alpha = render_mask(item, width, height, mask_directory)?;
        feather_alpha(&mut alpha, width, height, item.feather());
        composite(&mut result, &warped_sources[item.source()], &alpha);
    }

    let mut output_frame = base;
    output_frame.meta.path = output.clone();
    output_frame.rgb = result
        .iter()
        .map(|&value| {
            let encoded = srgb_encode(value.clamp(0.0, 1.0));
            (encoded * 65_535.0).round() as u16
        })
        .collect();

    let options = WriteOptions {
        bits: output_frame.meta.bits,
        ..WriteOptions::default()
    };
    tiff_write::write_tiff(&output, &output_frame, &options)
        .map_err(|err| RetouchError::Io(err.to_string()))?;

    Ok(RetouchOutcome {
        output,
        width,
        height,
        masks_applied: mask.items.len(),
    })
}

fn validate_sources(
    request: &RetouchRequest,
    report: &RetouchReport,
    mask: &Mask,
) -> Result<(), RetouchError> {
    if request.sources.is_empty() {
        return Err(RetouchError::Usage(
            "at least one source is required".to_string(),
        ));
    }
    if request.sources.len() != report.inputs.len() {
        return Err(RetouchError::Input(format!(
            "report records {} input(s) but {} source(s) were provided",
            report.inputs.len(),
            request.sources.len()
        )));
    }
    if report.transforms.len() != report.inputs.len() {
        return Err(RetouchError::InvalidReport(format!(
            "report has {} transform(s) for {} input(s)",
            report.transforms.len(),
            report.inputs.len()
        )));
    }

    for (index, path) in request.sources.iter().enumerate() {
        let actual = sha256_file(path).map_err(|err| {
            RetouchError::Input(format!("cannot hash source '{}': {err}", path.display()))
        })?;
        if !actual.eq_ignore_ascii_case(&report.inputs[index].sha256) {
            return Err(RetouchError::Input(format!(
                "source '{}' does not match the report hash",
                path.display()
            )));
        }
    }

    let base_hash = sha256_file(&request.base).map_err(|err| {
        RetouchError::Input(format!(
            "cannot hash base '{}': {err}",
            request.base.display()
        ))
    })?;
    if let Some(expected) = &report.output_sha256
        && !expected.is_empty()
        && !base_hash.eq_ignore_ascii_case(expected)
    {
        return Err(RetouchError::Input(format!(
            "base '{}' does not match the report output hash",
            request.base.display()
        )));
    }

    for (index, item) in mask.items.iter().enumerate() {
        if item.source() >= request.sources.len() {
            return Err(RetouchError::InvalidMask(format!(
                "mask {} selects source {} but only {} source(s) were provided",
                index + 1,
                item.source(),
                request.sources.len()
            )));
        }
    }

    Ok(())
}

fn default_output_path(base: &Path) -> PathBuf {
    let stem = base
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("output");
    let parent = base.parent().unwrap_or_else(|| Path::new(""));
    parent.join(format!("{stem}-retouched.tif"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

fn render_mask(
    item: &MaskItem,
    width: u32,
    height: u32,
    mask_directory: &Path,
) -> Result<Vec<f32>, RetouchError> {
    match item {
        MaskItem::Stroke(mask) => Ok(rasterize_stroke(mask, width, height)),
        MaskItem::Polygon(mask) => Ok(rasterize_polygon(mask, width, height)),
        MaskItem::Png(mask) => {
            let path = Path::new(&mask.path);
            let resolved = if path.is_absolute() {
                path.to_path_buf()
            } else {
                mask_directory.join(path)
            };
            load_png_mask(&resolved, width, height)
        }
    }
}

fn composite(result: &mut [f32], source: &[f32], alpha: &[f32]) {
    for (pixel, &weight) in alpha.iter().enumerate() {
        let weight = weight.clamp(0.0, 1.0);
        if weight <= 0.0 {
            continue;
        }
        let index = pixel * 3;
        let out = &mut result[index..index + 3];
        let src = &source[index..index + 3];
        for (out, &src) in out.iter_mut().zip(src) {
            *out = (1.0 - weight) * *out + weight * src;
        }
    }
}

/// Rasterizes a round-capped, round-joined stroke.
#[must_use]
pub fn rasterize_stroke(mask: &StrokeMask, width: u32, height: u32) -> Vec<f32> {
    let width_usize = width as usize;
    let height_usize = height as usize;
    let mut alpha = vec![0.0f32; width_usize * height_usize];
    if mask.points.is_empty() {
        return alpha;
    }

    for y in 0..height_usize {
        let py = y as f64 + 0.5;
        for x in 0..width_usize {
            let px = x as f64 + 0.5;
            let mut distance = f64::INFINITY;
            if mask.points.len() == 1 {
                let (ax, ay) = mask.points[0];
                distance = (px - ax).hypot(py - ay);
            } else {
                for pair in mask.points.windows(2) {
                    let (ax, ay) = pair[0];
                    let (bx, by) = pair[1];
                    distance = distance.min(point_segment_distance(px, py, ax, ay, bx, by));
                }
            }
            alpha[y * width_usize + x] = (mask.radius + 0.5 - distance).clamp(0.0, 1.0) as f32;
        }
    }
    alpha
}

/// Rasterizes a filled polygon using the even-odd rule at pixel centres.
#[must_use]
pub fn rasterize_polygon(mask: &PolygonMask, width: u32, height: u32) -> Vec<f32> {
    let width_usize = width as usize;
    let height_usize = height as usize;
    let mut alpha = vec![0.0f32; width_usize * height_usize];
    if mask.points.len() < 3 {
        return alpha;
    }

    for y in 0..height_usize {
        let py = y as f64 + 0.5;
        for x in 0..width_usize {
            let px = x as f64 + 0.5;
            if point_in_polygon(px, py, &mask.points) {
                alpha[y * width_usize + x] = 1.0;
            }
        }
    }
    alpha
}

fn point_segment_distance(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let dx = bx - ax;
    let dy = by - ay;
    let length_squared = dx * dx + dy * dy;
    if length_squared <= f64::EPSILON {
        return (px - ax).hypot(py - ay);
    }
    let t = (((px - ax) * dx + (py - ay) * dy) / length_squared).clamp(0.0, 1.0);
    (px - (ax + t * dx)).hypot(py - (ay + t * dy))
}

fn point_in_polygon(px: f64, py: f64, points: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let mut previous = points.len() - 1;
    for current in 0..points.len() {
        let (x0, y0) = points[previous];
        let (x1, y1) = points[current];
        if (y0 > py) != (y1 > py) && px < (x1 - x0) * (py - y0) / (y1 - y0) + x0 {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

/// Applies a separable Gaussian blur to an alpha mask.
///
/// `sigma` is the feather radius in pixels; zero leaves the mask unchanged.
/// The kernel is normalised, so a fully opaque mask stays opaque.
pub fn feather_alpha(alpha: &mut [f32], width: u32, height: u32, sigma: f64) {
    if !sigma.is_finite() || sigma <= 0.0 || width == 0 || height == 0 {
        return;
    }
    let width_usize = width as usize;
    let height_usize = height as usize;
    if alpha.len() != width_usize * height_usize {
        return;
    }

    let radius = (3.0 * sigma)
        .ceil()
        .min(width_usize.max(height_usize) as f64) as isize;
    if radius < 1 {
        return;
    }
    let mut kernel = Vec::with_capacity((2 * radius + 1) as usize);
    let mut sum = 0.0f64;
    for offset in -radius..=radius {
        let x = offset as f64 / sigma;
        let weight = (-0.5 * x * x).exp();
        kernel.push(weight);
        sum += weight;
    }
    for weight in &mut kernel {
        *weight /= sum;
    }

    let mut horizontal = vec![0.0f32; alpha.len()];
    for y in 0..height_usize {
        for x in 0..width_usize {
            let mut accumulator = 0.0f64;
            for (index, &weight) in kernel.iter().enumerate() {
                let sample_x = (x as isize + index as isize - radius)
                    .clamp(0, width_usize as isize - 1) as usize;
                accumulator += weight * f64::from(alpha[y * width_usize + sample_x]);
            }
            horizontal[y * width_usize + x] = accumulator as f32;
        }
    }

    for y in 0..height_usize {
        for x in 0..width_usize {
            let mut accumulator = 0.0f64;
            for (index, &weight) in kernel.iter().enumerate() {
                let sample_y = (y as isize + index as isize - radius)
                    .clamp(0, height_usize as isize - 1) as usize;
                accumulator += weight * f64::from(horizontal[sample_y * width_usize + x]);
            }
            alpha[y * width_usize + x] = accumulator as f32;
        }
    }
}

/// Reads and decodes a grayscale PNG mask.
///
/// # Errors
///
/// Returns [`RetouchError::InvalidMask`] for a malformed PNG, a colour type
/// other than grayscale, an unsupported bit depth, interlacing, or dimensions
/// that do not match the output.
pub fn load_png_mask(
    path: &Path,
    expected_width: u32,
    expected_height: u32,
) -> Result<Vec<f32>, RetouchError> {
    let bytes = fs::read(path).map_err(|err| {
        RetouchError::InvalidMask(format!(
            "failed to read mask PNG '{}': {err}",
            path.display()
        ))
    })?;
    decode_png_mask(&bytes, expected_width, expected_height).map_err(|err| match err {
        RetouchError::InvalidMask(message) => {
            RetouchError::InvalidMask(format!("mask PNG '{}': {message}", path.display()))
        }
        other => other,
    })
}

/// Decodes an in-memory grayscale PNG mask.
///
/// # Errors
///
/// Returns [`RetouchError::InvalidMask`] for malformed or unsupported PNG data.
pub fn decode_png_mask(
    bytes: &[u8],
    expected_width: u32,
    expected_height: u32,
) -> Result<Vec<f32>, RetouchError> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

    if !bytes.starts_with(&SIGNATURE) {
        return Err(RetouchError::InvalidMask(
            "not a PNG file (bad signature)".to_string(),
        ));
    }

    let mut position = SIGNATURE.len();
    let mut header: Option<(u32, u32, u8, u8)> = None;
    let mut data = Vec::new();
    let mut saw_end = false;

    while position + 8 <= bytes.len() {
        let length = u32::from_be_bytes([
            bytes[position],
            bytes[position + 1],
            bytes[position + 2],
            bytes[position + 3],
        ]) as usize;
        let kind = &bytes[position + 4..position + 8];
        let start = position + 8;
        let end = start
            .checked_add(length)
            .ok_or_else(|| RetouchError::InvalidMask("PNG chunk length overflow".to_string()))?;
        if end + 4 > bytes.len() {
            return Err(RetouchError::InvalidMask("truncated PNG chunk".to_string()));
        }
        let chunk = &bytes[start..end];
        position = end + 4;

        match kind {
            b"IHDR" => {
                if length != 13 {
                    return Err(RetouchError::InvalidMask(
                        "PNG IHDR has the wrong length".to_string(),
                    ));
                }
                let width = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
                let height = u32::from_be_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]);
                let bit_depth = chunk[8];
                let colour_type = chunk[9];
                let compression = chunk[10];
                let filter = chunk[11];
                let interlace = chunk[12];
                if width == 0 || height == 0 {
                    return Err(RetouchError::InvalidMask(
                        "PNG dimensions must be non-zero".to_string(),
                    ));
                }
                if colour_type != 0 {
                    return Err(RetouchError::InvalidMask(format!(
                        "PNG mask must be grayscale, got colour type {colour_type}"
                    )));
                }
                if bit_depth != 8 && bit_depth != 16 {
                    return Err(RetouchError::InvalidMask(format!(
                        "PNG mask bit depth {bit_depth} is unsupported (expected 8 or 16)"
                    )));
                }
                if compression != 0 || filter != 0 {
                    return Err(RetouchError::InvalidMask(
                        "PNG mask uses unsupported compression or filter method".to_string(),
                    ));
                }
                if interlace != 0 {
                    return Err(RetouchError::InvalidMask(
                        "interlaced PNG masks are unsupported".to_string(),
                    ));
                }
                header = Some((width, height, bit_depth, colour_type));
            }
            b"IDAT" => data.extend_from_slice(chunk),
            b"IEND" => {
                saw_end = true;
                break;
            }
            _ => {}
        }
    }

    if !saw_end {
        return Err(RetouchError::InvalidMask("PNG is missing IEND".to_string()));
    }
    let (width, height, bit_depth, _) =
        header.ok_or_else(|| RetouchError::InvalidMask("PNG is missing IHDR".to_string()))?;
    if width != expected_width || height != expected_height {
        return Err(RetouchError::InvalidMask(format!(
            "PNG mask is {width}x{height} but the output is {expected_width}x{expected_height}"
        )));
    }
    if data.is_empty() {
        return Err(RetouchError::InvalidMask(
            "PNG contains no image data".to_string(),
        ));
    }

    let bytes_per_pixel = usize::from(bit_depth / 8);
    let row_bytes = (width as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| RetouchError::InvalidMask("PNG row size overflow".to_string()))?;
    let expected_raw = (height as usize)
        .checked_mul(row_bytes + 1)
        .ok_or_else(|| RetouchError::InvalidMask("PNG data size overflow".to_string()))?;

    let mut decoder = flate2::read::ZlibDecoder::new(data.as_slice()).take(expected_raw as u64 + 1);
    let mut raw = Vec::with_capacity(expected_raw);
    decoder
        .read_to_end(&mut raw)
        .map_err(|err| RetouchError::InvalidMask(format!("cannot inflate PNG data: {err}")))?;
    if raw.len() != expected_raw {
        return Err(RetouchError::InvalidMask(
            "PNG image data has the wrong size".to_string(),
        ));
    }

    let mut alpha = vec![0.0f32; width as usize * height as usize];
    let mut previous = vec![0u8; row_bytes];
    let mut current = vec![0u8; row_bytes];
    let mut offset = 0;
    for y in 0..height as usize {
        let filter = raw[offset];
        offset += 1;
        let scanline = &raw[offset..offset + row_bytes];
        offset += row_bytes;

        for index in 0..row_bytes {
            let left = if index >= bytes_per_pixel {
                current[index - bytes_per_pixel]
            } else {
                0
            };
            let above = if y > 0 { previous[index] } else { 0 };
            let upper_left = if y > 0 && index >= bytes_per_pixel {
                previous[index - bytes_per_pixel]
            } else {
                0
            };
            current[index] = match filter {
                0 => scanline[index],
                1 => scanline[index].wrapping_add(left),
                2 => scanline[index].wrapping_add(above),
                3 => scanline[index].wrapping_add(((u16::from(left) + u16::from(above)) / 2) as u8),
                4 => scanline[index].wrapping_add(paeth(left, above, upper_left)),
                other => {
                    return Err(RetouchError::InvalidMask(format!(
                        "PNG mask uses unsupported filter {other}"
                    )));
                }
            };
        }

        for x in 0..width as usize {
            let value = if bit_depth == 8 {
                f32::from(current[x]) / 255.0
            } else {
                let index = x * 2;
                let sample = u16::from_be_bytes([current[index], current[index + 1]]);
                f32::from(sample) / 65_535.0
            };
            alpha[y * width as usize + x] = value;
        }
        std::mem::swap(&mut previous, &mut current);
    }

    Ok(alpha)
}

fn paeth(left: u8, above: u8, upper_left: u8) -> u8 {
    let p = i16::from(left) + i16::from(above) - i16::from(upper_left);
    let pa = (p - i16::from(left)).abs();
    let pb = (p - i16::from(above)).abs();
    let pc = (p - i16::from(upper_left)).abs();
    if pa <= pb && pa <= pc {
        left
    } else if pb <= pc {
        above
    } else {
        upper_left
    }
}

/// Parses mask JSON v1.
///
/// Strokes, polygons, and PNG masks are appended in the order their arrays
/// appear in the document. Unknown fields are ignored, matching the report
/// compatibility rule in `docs/outputs.md` §4.
///
/// # Errors
///
/// Returns [`RetouchError::InvalidMask`] for malformed JSON, an unsupported
/// version, missing fields, non-finite coordinates, or invalid geometry.
pub fn parse_mask(text: &str) -> Result<Mask, RetouchError> {
    let root = parse_json(text)
        .map_err(|err| RetouchError::InvalidMask(format!("mask JSON is invalid: {err}")))?;
    let version = root
        .get("version")
        .and_then(Json::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            RetouchError::InvalidMask("mask JSON must contain an integer 'version'".to_string())
        })?;
    if version != MASK_VERSION {
        return Err(RetouchError::InvalidMask(format!(
            "unsupported mask version {version} (expected {MASK_VERSION})"
        )));
    }

    let fields = root
        .as_object()
        .ok_or_else(|| RetouchError::InvalidMask("mask JSON must be an object".to_string()))?;
    let mut items = Vec::new();
    for (key, value) in fields {
        match key.as_str() {
            "strokes" => {
                let entries = value.as_array().ok_or_else(|| {
                    RetouchError::InvalidMask("'strokes' must be an array".to_string())
                })?;
                for (index, entry) in entries.iter().enumerate() {
                    items.push(MaskItem::Stroke(parse_stroke(entry, index + 1)?));
                }
            }
            "polygons" => {
                let entries = value.as_array().ok_or_else(|| {
                    RetouchError::InvalidMask("'polygons' must be an array".to_string())
                })?;
                for (index, entry) in entries.iter().enumerate() {
                    items.push(MaskItem::Polygon(parse_polygon(entry, index + 1)?));
                }
            }
            "masks" => {
                let entries = value.as_array().ok_or_else(|| {
                    RetouchError::InvalidMask("'masks' must be an array".to_string())
                })?;
                for (index, entry) in entries.iter().enumerate() {
                    items.push(MaskItem::Png(parse_png(entry, index + 1)?));
                }
            }
            _ => {}
        }
    }

    Ok(Mask { version, items })
}

fn parse_stroke(value: &Json, index: usize) -> Result<StrokeMask, RetouchError> {
    let context = format!("stroke {index}");
    let points = parse_points(value, &context)?;
    if points.len() < 2 {
        return Err(RetouchError::InvalidMask(format!(
            "{context} needs at least two points"
        )));
    }
    let radius = required_f64(value, "radius", &context)?;
    if radius <= 0.0 {
        return Err(RetouchError::InvalidMask(format!(
            "{context} radius must be greater than zero"
        )));
    }
    let feather = optional_f64(value, "feather", &context)?.unwrap_or(0.0);
    if feather < 0.0 {
        return Err(RetouchError::InvalidMask(format!(
            "{context} feather must be non-negative"
        )));
    }
    let source = required_usize(value, "source", &context)?;

    Ok(StrokeMask {
        points,
        radius,
        feather,
        source,
    })
}

fn parse_polygon(value: &Json, index: usize) -> Result<PolygonMask, RetouchError> {
    let context = format!("polygon {index}");
    let points = parse_points(value, &context)?;
    if points.len() < 3 {
        return Err(RetouchError::InvalidMask(format!(
            "{context} needs at least three points"
        )));
    }
    let feather = optional_f64(value, "feather", &context)?.unwrap_or(0.0);
    if feather < 0.0 {
        return Err(RetouchError::InvalidMask(format!(
            "{context} feather must be non-negative"
        )));
    }
    let source = required_usize(value, "source", &context)?;

    Ok(PolygonMask {
        points,
        feather,
        source,
    })
}

fn parse_png(value: &Json, index: usize) -> Result<PngMask, RetouchError> {
    let context = format!("mask {index}");
    let path = value
        .get("path")
        .and_then(Json::as_str)
        .filter(|path| !path.is_empty())
        .ok_or_else(|| RetouchError::InvalidMask(format!("{context} needs a non-empty 'path'")))?
        .to_string();
    let feather = optional_f64(value, "feather", &context)?.unwrap_or(0.0);
    if feather < 0.0 {
        return Err(RetouchError::InvalidMask(format!(
            "{context} feather must be non-negative"
        )));
    }
    let source = required_usize(value, "source", &context)?;

    Ok(PngMask {
        path,
        feather,
        source,
    })
}

fn parse_points(value: &Json, context: &str) -> Result<Vec<(f64, f64)>, RetouchError> {
    let entries = value
        .get("points")
        .and_then(Json::as_array)
        .ok_or_else(|| RetouchError::InvalidMask(format!("{context} needs a 'points' array")))?;
    let mut points = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let pair = entry.as_array().ok_or_else(|| {
            RetouchError::InvalidMask(format!(
                "{context} point {} must be an [x, y] pair",
                index + 1
            ))
        })?;
        if pair.len() != 2 {
            return Err(RetouchError::InvalidMask(format!(
                "{context} point {} must be an [x, y] pair",
                index + 1
            )));
        }
        let x = pair[0].as_f64().filter(|value| value.is_finite());
        let y = pair[1].as_f64().filter(|value| value.is_finite());
        match (x, y) {
            (Some(x), Some(y)) => points.push((x, y)),
            _ => {
                return Err(RetouchError::InvalidMask(format!(
                    "{context} point {} has a non-finite coordinate",
                    index + 1
                )));
            }
        }
    }
    Ok(points)
}

fn required_f64(value: &Json, key: &str, context: &str) -> Result<f64, RetouchError> {
    value
        .get(key)
        .and_then(Json::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| RetouchError::InvalidMask(format!("{context} needs a finite '{key}'")))
}

fn optional_f64(value: &Json, key: &str, context: &str) -> Result<Option<f64>, RetouchError> {
    match value.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .map(Some)
            .ok_or_else(|| RetouchError::InvalidMask(format!("{context} has an invalid '{key}'"))),
    }
}

fn required_usize(value: &Json, key: &str, context: &str) -> Result<usize, RetouchError> {
    value
        .get(key)
        .and_then(Json::as_u64)
        .and_then(|number| usize::try_from(number).ok())
        .ok_or_else(|| {
            RetouchError::InvalidMask(format!("{context} needs a non-negative integer '{key}'"))
        })
}

/// Parses the retouch-relevant fields of a run report.
///
/// Unknown fields are ignored and optional report sections are skipped, as
/// required by `docs/outputs.md` §4.
///
/// # Errors
///
/// Returns [`RetouchError::InvalidReport`] for malformed JSON, a schema other
/// than version 1, missing input/alignment data, or a degenerate transform.
pub fn parse_report(text: &str) -> Result<RetouchReport, RetouchError> {
    let root = parse_json(text)
        .map_err(|err| RetouchError::InvalidReport(format!("report JSON is invalid: {err}")))?;
    let schema_version = root
        .get("schema_version")
        .and_then(Json::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or_else(|| {
            RetouchError::InvalidReport(
                "report must contain an integer 'schema_version'".to_string(),
            )
        })?;
    if schema_version != REPORT_SCHEMA_VERSION {
        return Err(RetouchError::InvalidReport(format!(
            "unsupported report schema_version {schema_version} (expected {REPORT_SCHEMA_VERSION})"
        )));
    }

    let input_entries = root
        .get("inputs")
        .and_then(Json::as_array)
        .ok_or_else(|| RetouchError::InvalidReport("report needs an 'inputs' array".to_string()))?;
    if input_entries.is_empty() {
        return Err(RetouchError::InvalidReport(
            "report has no inputs".to_string(),
        ));
    }
    let mut inputs = Vec::with_capacity(input_entries.len());
    for (index, entry) in input_entries.iter().enumerate() {
        let sha256 = entry
            .get("sha256")
            .and_then(Json::as_str)
            .filter(|hash| valid_sha256(hash))
            .ok_or_else(|| {
                RetouchError::InvalidReport(format!(
                    "report input {} needs a 64-digit hex 'sha256'",
                    index + 1
                ))
            })?
            .to_ascii_lowercase();
        let path = entry.get("path").and_then(Json::as_str).map(str::to_owned);
        let width = entry
            .get("width")
            .and_then(Json::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        let height = entry
            .get("height")
            .and_then(Json::as_u64)
            .and_then(|value| u32::try_from(value).ok());
        inputs.push(ReportInput {
            path,
            sha256,
            width,
            height,
        });
    }

    let alignment = root.get("alignment").ok_or_else(|| {
        RetouchError::InvalidReport("report needs an 'alignment' object".to_string())
    })?;
    let reference_index = alignment
        .get("reference_index")
        .and_then(Json::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or_else(|| {
            RetouchError::InvalidReport("report alignment needs a 'reference_index'".to_string())
        })?;
    let transform_entries = alignment
        .get("transforms")
        .and_then(Json::as_array)
        .ok_or_else(|| {
            RetouchError::InvalidReport("report alignment needs a 'transforms' array".to_string())
        })?;
    let mut transforms = Vec::with_capacity(transform_entries.len());
    for (index, entry) in transform_entries.iter().enumerate() {
        let a = report_f64(entry, "a", index)?;
        let b = report_f64(entry, "b", index)?;
        let tx = report_f64(entry, "tx", index)?;
        let ty = report_f64(entry, "ty", index)?;
        let determinant = a * a + b * b;
        if !determinant.is_finite() || determinant <= 1.0e-24 {
            return Err(RetouchError::InvalidReport(format!(
                "report transform {} is degenerate",
                index + 1
            )));
        }
        transforms.push(Similarity { a, b, tx, ty });
    }

    let output_sha256 = root
        .get("output")
        .and_then(|output| output.get("sha256"))
        .and_then(Json::as_str)
        .filter(|hash| hash.is_empty() || valid_sha256(hash))
        .map(str::to_ascii_lowercase);

    Ok(RetouchReport {
        schema_version,
        inputs,
        reference_index,
        transforms,
        output_sha256,
    })
}

fn report_f64(value: &Json, key: &str, index: usize) -> Result<f64, RetouchError> {
    value
        .get(key)
        .and_then(Json::as_f64)
        .filter(|number| number.is_finite())
        .ok_or_else(|| {
            RetouchError::InvalidReport(format!(
                "report transform {} needs a finite '{key}'",
                index + 1
            ))
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> Option<&Self> {
        match self {
            Self::Object(fields) => fields
                .iter()
                .find(|(field, _)| field == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    fn as_object(&self) -> Option<&[(String, Self)]> {
        match self {
            Self::Object(fields) => Some(fields),
            _ => None,
        }
    }

    fn as_array(&self) -> Option<&[Self]> {
        match self {
            Self::Array(values) => Some(values),
            _ => None,
        }
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }

    fn as_u64(&self) -> Option<u64> {
        match self {
            Self::Number(value)
                if value.is_finite()
                    && *value >= 0.0
                    && value.fract() == 0.0
                    && *value <= u64::MAX as f64 =>
            {
                Some(*value as u64)
            }
            _ => None,
        }
    }
}

fn parse_json(input: &str) -> Result<Json, String> {
    let mut parser = JsonParser {
        input,
        bytes: input.as_bytes(),
        position: 0,
    };
    let value = parser.value(0)?;
    parser.skip_whitespace();
    if parser.position != parser.bytes.len() {
        return Err(parser.error("trailing characters"));
    }
    Ok(value)
}

struct JsonParser<'a> {
    input: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl JsonParser<'_> {
    fn value(&mut self, depth: usize) -> Result<Json, String> {
        if depth > 128 {
            return Err(self.error("nesting is too deep"));
        }
        self.skip_whitespace();
        match self.peek() {
            Some(b'n') => {
                self.keyword("null")?;
                Ok(Json::Null)
            }
            Some(b't') => {
                self.keyword("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.keyword("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'"') => self.string().map(Json::String),
            Some(b'[') => self.array(depth),
            Some(b'{') => self.object(depth),
            Some(b'-' | b'0'..=b'9') => self.number().map(Json::Number),
            Some(_) => Err(self.error("unexpected character")),
            None => Err(self.error("unexpected end of input")),
        }
    }

    fn array(&mut self, depth: usize) -> Result<Json, String> {
        self.position += 1;
        self.skip_whitespace();
        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.position += 1;
            return Ok(Json::Array(values));
        }
        loop {
            values.push(self.value(depth + 1)?);
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.position += 1;
                }
                Some(b']') => {
                    self.position += 1;
                    return Ok(Json::Array(values));
                }
                Some(_) => return Err(self.error("expected ',' or ']'")),
                None => return Err(self.error("unterminated array")),
            }
        }
    }

    fn object(&mut self, depth: usize) -> Result<Json, String> {
        self.position += 1;
        self.skip_whitespace();
        let mut fields = Vec::new();
        if self.peek() == Some(b'}') {
            self.position += 1;
            return Ok(Json::Object(fields));
        }
        loop {
            self.skip_whitespace();
            if self.peek() != Some(b'"') {
                return Err(self.error("expected an object key"));
            }
            let key = self.string()?;
            self.skip_whitespace();
            if self.peek() != Some(b':') {
                return Err(self.error("expected ':'"));
            }
            self.position += 1;
            let value = self.value(depth + 1)?;
            fields.push((key, value));
            self.skip_whitespace();
            match self.peek() {
                Some(b',') => {
                    self.position += 1;
                }
                Some(b'}') => {
                    self.position += 1;
                    return Ok(Json::Object(fields));
                }
                Some(_) => return Err(self.error("expected ',' or '}'")),
                None => return Err(self.error("unterminated object")),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.position += 1;
        let mut out = String::new();
        loop {
            match self.bump() {
                None => return Err(self.error("unterminated string")),
                Some(b'"') => return Ok(out),
                Some(b'\\') => {
                    let escape = self
                        .bump()
                        .ok_or_else(|| self.error("unterminated escape"))?;
                    match escape {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let high = self.hex4()?;
                            let code = if (0xd800..=0xdbff).contains(&high) {
                                if self.bump() != Some(b'\\') || self.bump() != Some(b'u') {
                                    return Err(self.error("invalid surrogate pair"));
                                }
                                let low = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&low) {
                                    return Err(self.error("invalid surrogate pair"));
                                }
                                0x1_0000 + ((high - 0xd800) << 10) + (low - 0xdc00)
                            } else {
                                high
                            };
                            out.push(
                                char::from_u32(code)
                                    .ok_or_else(|| self.error("invalid unicode escape"))?,
                            );
                        }
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                Some(byte) if byte < 0x20 => {
                    return Err(self.error("control character in string"));
                }
                Some(_) => {
                    let start = self.position - 1;
                    let character = self.input[start..]
                        .chars()
                        .next()
                        .ok_or_else(|| self.error("invalid UTF-8"))?;
                    self.position = start + character.len_utf8();
                    out.push(character);
                }
            }
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let start = self.position;
        let end = start
            .checked_add(4)
            .ok_or_else(|| self.error("unicode escape overflow"))?;
        if end > self.bytes.len() {
            return Err(self.error("truncated unicode escape"));
        }
        let digits = std::str::from_utf8(&self.bytes[start..end])
            .map_err(|_| self.error("invalid unicode escape"))?;
        let value =
            u32::from_str_radix(digits, 16).map_err(|_| self.error("invalid unicode escape"))?;
        self.position = end;
        Ok(value)
    }

    fn number(&mut self) -> Result<f64, String> {
        let start = self.position;
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => {
                self.position += 1;
            }
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.position += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number fraction"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number exponent"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.position += 1;
            }
        }
        let text = &self.input[start..self.position];
        let value = text
            .parse::<f64>()
            .map_err(|_| self.error("invalid number"))?;
        if !value.is_finite() {
            return Err(self.error("number is not finite"));
        }
        Ok(value)
    }

    fn keyword(&mut self, keyword: &str) -> Result<(), String> {
        if self.input[self.position..].starts_with(keyword) {
            self.position += keyword.len();
            Ok(())
        } else {
            Err(self.error("invalid literal"))
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.position += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.position).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.position += 1;
        Some(byte)
    }

    fn error(&self, message: &str) -> String {
        format!("{message} at byte {}", self.position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stroke_polygon_and_png_masks() {
        let mask = parse_mask(
            r#"{
                "version": 1,
                "strokes": [
                    { "points": [[1, 2], [3, 4]], "radius": 2, "feather": 1, "source": 1 }
                ],
                "polygons": [
                    { "points": [[0, 0], [4, 0], [4, 4]], "feather": 0, "source": 0 }
                ],
                "masks": [
                    { "path": "mask.png", "source": 2 }
                ]
            }"#,
        )
        .expect("valid mask");

        assert_eq!(mask.version, MASK_VERSION);
        assert_eq!(mask.items.len(), 3);
        assert_eq!(mask.items[0].source(), 1);
        assert_eq!(mask.items[0].feather(), 1.0);
        assert_eq!(mask.items[1].source(), 0);
        assert_eq!(mask.items[2].source(), 2);
    }

    #[test]
    fn rejects_invalid_mask_documents() {
        for text in [
            "{",
            r#"{"version": 2}"#,
            r#"{"version": 1, "strokes": [{"points": [[0, 0]], "radius": 2, "source": 0}]}"#,
            r#"{"version": 1, "strokes": [{"points": [[0, 0], [1, 1]], "radius": 0, "source": 0}]}"#,
            r#"{"version": 1, "polygons": [{"points": [[0, 0], [1, 1]], "source": 0}]}"#,
            r#"{"version": 1, "masks": [{"path": "mask.png", "source": -1}]}"#,
        ] {
            assert!(parse_mask(text).is_err(), "accepted invalid mask: {text}");
        }
    }

    #[test]
    fn parses_report_transforms_and_rejects_degenerate_ones() {
        let report = parse_report(
            r#"{
                "schema_version": 1,
                "inputs": [
                    { "path": "a.tif", "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "width": 4, "height": 3 }
                ],
                "alignment": {
                    "reference_index": 0,
                    "transforms": [{ "a": 1, "b": 0, "tx": 0.5, "ty": -0.5 }]
                },
                "output": { "sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }
            }"#,
        )
        .expect("valid report");

        assert_eq!(report.inputs.len(), 1);
        assert_eq!(report.transforms[0].tx, 0.5);
        assert_eq!(report.output_sha256.as_deref().map(str::len), Some(64));

        let degenerate = r#"{
            "schema_version": 1,
            "inputs": [{ "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }],
            "alignment": { "reference_index": 0, "transforms": [{ "a": 0, "b": 0, "tx": 0, "ty": 0 }] }
        }"#;
        assert!(parse_report(degenerate).is_err());
    }

    #[test]
    fn polygon_and_stroke_cover_expected_pixels() {
        let polygon = PolygonMask {
            points: vec![(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)],
            feather: 0.0,
            source: 0,
        };
        let alpha = rasterize_polygon(&polygon, 4, 4);
        assert_eq!(alpha[0], 1.0);
        assert_eq!(alpha[1], 1.0);
        assert_eq!(alpha[2], 0.0);
        assert_eq!(alpha[3], 0.0);

        let stroke = StrokeMask {
            points: vec![(1.5, 1.5), (1.5, 1.5)],
            radius: 1.0,
            feather: 0.0,
            source: 0,
        };
        let alpha = rasterize_stroke(&stroke, 4, 4);
        assert_eq!(alpha[5], 1.0);
        assert_eq!(alpha[15], 0.0);
    }

    #[test]
    fn feather_keeps_a_full_mask_opaque_and_spreads_an_impulse() {
        let mut full = vec![1.0f32; 5 * 5];
        feather_alpha(&mut full, 5, 5, 1.0);
        for value in full {
            assert!((value - 1.0).abs() < 1e-6);
        }

        let mut impulse = vec![0.0f32; 7 * 7];
        impulse[3 * 7 + 3] = 1.0;
        feather_alpha(&mut impulse, 7, 7, 1.0);
        assert!(impulse[3 * 7 + 3] > impulse[3 * 7 + 2]);
        assert!(impulse[3 * 7 + 2] > 0.0);
        assert!((impulse[0] - 0.0).abs() < 0.01);
    }

    #[test]
    fn json_parser_handles_escapes_and_unicode() {
        let value = parse_json(r#"{"a": "x\n\u00e9", "n": -1.5e2, "b": true}"#).unwrap();
        assert_eq!(value.get("a").and_then(Json::as_str), Some("x\né"));
        assert_eq!(value.get("n").and_then(Json::as_f64), Some(-150.0));
        assert_eq!(value.get("b"), Some(&Json::Bool(true)));
    }
}
