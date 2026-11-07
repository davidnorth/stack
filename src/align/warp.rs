//! Lanczos3 warping, valid rectangle, and cropping.
//!
//! Implements `docs/alignment.md` §6–7. Images are interleaved, linear-light
//! `f32` samples (`channels` per pixel). The similarity maps output pixel
//! coordinates to source coordinates, matching [`crate::Similarity`].
//!
//! The sampler is a scanline gather with separable Lanczos3 weights: for each
//! output pixel the x and y kernels are evaluated independently and multiplied.
//! This supports arbitrary rotation while keeping the kernel separable.
//! Weights come from a linearly interpolated subpixel table, so identity and
//! axis-aligned 90° rotations sample exact integer positions and are bit-exact.
//!
//! Minification (`scale > 1/0.98`, where `scale` is the source step per output
//! pixel) applies a Gaussian anti-alias pre-blur with
//! `sigma = sqrt(scale² - 1)` in source pixels before resampling. Out-of-bounds
//! samples clamp to the nearest source pixel, so warping never introduces black
//! borders.
//!
//! Rows are processed in parallel with rayon. The valid rectangle insets the
//! source bounds by the Lanczos radius plus one pixel and is intended to be
//! applied after warping, before pyramid construction.

use crate::{Rect, Similarity};
use rayon::prelude::*;
use std::f64::consts::PI;
use std::fmt;

/// Lanczos3 radius in source pixels.
pub const LANCZOS_RADIUS: i32 = 3;

/// Minification factor below which the anti-alias pre-blur is applied.
///
/// This is the source-to-output scale from `docs/alignment.md` §6. The
/// transform stores the inverse (output-to-source), so the corresponding
/// threshold is `scale > 1 / 0.98`.
pub const MINIFICATION_SCALE: f64 = 0.98;

/// Crop inset: Lanczos radius (3 px) plus 1 px.
pub const CROP_INSET: u32 = LANCZOS_RADIUS as u32 + 1;

/// Number of subpixel samples in the precomputed kernel table.
const SUBPIXEL_STEPS: usize = 2048;

/// Errors from warping and cropping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WarpError {
    /// One of the image dimensions is zero.
    EmptyImage,
    /// The number of channels is zero.
    NoChannels,
    /// A source buffer length does not match `width * height * channels`.
    LengthMismatch { expected: usize, actual: usize },
    /// The similarity is non-finite or has zero scale.
    DegenerateTransform,
    /// The requested crop rectangle is empty or outside the image.
    InvalidRect,
}

impl fmt::Display for WarpError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyImage => write!(formatter, "image dimensions must be non-zero"),
            Self::NoChannels => write!(formatter, "channel count must be non-zero"),
            Self::LengthMismatch { expected, actual } => write!(
                formatter,
                "buffer length mismatch: expected {expected} samples, got {actual}"
            ),
            Self::DegenerateTransform => {
                write!(formatter, "similarity transform is degenerate")
            }
            Self::InvalidRect => {
                write!(formatter, "crop rectangle is empty or outside the image")
            }
        }
    }
}

impl std::error::Error for WarpError {}

/// Lanczos3 kernel.
///
/// `L(x) = sinc(x) * sinc(x / 3)` for `|x| < 3`, otherwise zero. Exact
/// non-zero integers return exactly zero, which keeps integer-aligned warps
/// bit-exact.
#[must_use]
pub fn lanczos3(x: f64) -> f64 {
    let abs = x.abs();
    if abs >= 3.0 {
        0.0
    } else if x == 0.0 {
        1.0
    } else if x == x.round() {
        0.0
    } else {
        sinc(x) * sinc(x / 3.0)
    }
}

fn sinc(x: f64) -> f64 {
    let angle = PI * x;
    angle.sin() / angle
}

/// Gaussian sigma, in source pixels, for the minification pre-blur.
///
/// Returns zero when the transform does not minify. `scale` is the
/// source-to-output scale (`1 / hypot(a, b)`), matching the `s` in
/// `docs/alignment.md` §6.
#[must_use]
pub fn prefilter_sigma(scale: f64) -> f64 {
    if !scale.is_finite() || scale <= 0.0 || scale >= MINIFICATION_SCALE {
        return 0.0;
    }
    ((1.0 / (scale * scale)) - 1.0).sqrt()
}

/// Warp an interleaved linear-light image with Lanczos3.
///
/// `transform` maps output pixel coordinates to source pixel coordinates.
/// `output_width`/`output_height` are the full (uncropped) output size; use
/// [`valid_rect`] or [`common_valid_rect`] followed by [`crop`] to remove the
/// invalid border.
///
/// # Errors
///
/// Returns [`WarpError`] for zero dimensions, zero channels, a mismatched
/// source buffer, or a degenerate transform.
pub fn warp_linear(
    source: &[f32],
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    channels: usize,
    transform: &Similarity,
) -> Result<Vec<f32>, WarpError> {
    if source_width == 0 || source_height == 0 || output_width == 0 || output_height == 0 {
        return Err(WarpError::EmptyImage);
    }
    if channels == 0 {
        return Err(WarpError::NoChannels);
    }

    let source_pixels = source_width as usize * source_height as usize;
    let expected = source_pixels * channels;
    if source.len() != expected {
        return Err(WarpError::LengthMismatch {
            expected,
            actual: source.len(),
        });
    }

    let det = transform.a * transform.a + transform.b * transform.b;
    if !transform.a.is_finite()
        || !transform.b.is_finite()
        || !transform.tx.is_finite()
        || !transform.ty.is_finite()
        || !det.is_finite()
        || det <= 1.0e-24
    {
        return Err(WarpError::DegenerateTransform);
    }

    // The common case is copied verbatim so identity warps are bit-exact.
    if is_identity(transform) && output_width == source_width && output_height == source_height {
        return Ok(source.to_vec());
    }

    let scale = det.sqrt();
    let sigma = prefilter_sigma(1.0 / scale);
    let filtered;
    let samples = if sigma > 0.0 {
        filtered = gaussian_blur(
            source,
            source_width as usize,
            source_height as usize,
            channels,
            sigma,
        );
        filtered.as_slice()
    } else {
        source
    };

    let source_width_usize = source_width as usize;
    let source_height_usize = source_height as usize;
    let output_width_usize = output_width as usize;
    let row_len = output_width_usize * channels;
    let mut output = vec![0.0f32; output_height as usize * row_len];
    let table = KernelTable::new();

    let a = transform.a;
    let b = transform.b;
    let tx = transform.tx;
    let ty = transform.ty;
    let max_x = (source_width_usize - 1) as f64;
    let max_y = (source_height_usize - 1) as f64;

    output
        .par_chunks_mut(row_len)
        .enumerate()
        .for_each(|(y, row)| {
            let y64 = y as f64;
            let x_term = tx - b * y64;
            let y_term = ty + a * y64;

            for (x, pixel) in row.chunks_mut(channels).enumerate() {
                let x64 = x as f64;
                let sx = (a * x64 + x_term).clamp(0.0, max_x);
                let sy = (b * x64 + y_term).clamp(0.0, max_y);

                let floor_x = sx.floor();
                let floor_y = sy.floor();
                let wx = table.weights((sx - floor_x) as f32);
                let wy = table.weights((sy - floor_y) as f32);
                let x_offsets = tap_offsets(floor_x as i64 - 2, source_width_usize);
                let y_offsets = tap_offsets(floor_y as i64 - 2, source_height_usize);

                if channels == 3 {
                    let mut sum = [0.0f32; 3];
                    for (ky, &wy_tap) in wy.iter().enumerate() {
                        let row_start = y_offsets[ky] * source_width_usize * 3;
                        for (kx, &wx_tap) in wx.iter().enumerate() {
                            let weight = wy_tap * wx_tap;
                            let index = row_start + x_offsets[kx] * 3;
                            sum[0] += weight * samples[index];
                            sum[1] += weight * samples[index + 1];
                            sum[2] += weight * samples[index + 2];
                        }
                    }
                    pixel.copy_from_slice(&sum);
                } else {
                    for (channel, value) in pixel.iter_mut().enumerate() {
                        let mut sum = 0.0f32;
                        for (ky, &wy_tap) in wy.iter().enumerate() {
                            let row_start = y_offsets[ky] * source_width_usize * channels + channel;
                            for (kx, &wx_tap) in wx.iter().enumerate() {
                                let index = row_start + x_offsets[kx] * channels;
                                sum += wy_tap * wx_tap * samples[index];
                            }
                        }
                        *value = sum;
                    }
                }
            }
        });

    Ok(output)
}

/// Axis-aligned output rectangle that maps inside the inset source bounds.
///
/// The result is conservative: it is the largest rectangle centred in the
/// feasible region that can be scaled uniformly without touching the inset
/// bounds. Returns `None` when the transform leaves no usable area.
pub fn valid_rect(
    output_width: u32,
    output_height: u32,
    source_width: u32,
    source_height: u32,
    transform: &Similarity,
) -> Option<Rect> {
    if output_width == 0 || output_height == 0 || source_width == 0 || source_height == 0 {
        return None;
    }
    let inset = CROP_INSET as f64;
    if (source_width as f64) <= 2.0 * inset || (source_height as f64) <= 2.0 * inset {
        return None;
    }

    let det = transform.a * transform.a + transform.b * transform.b;
    if !det.is_finite() || det <= 1.0e-24 {
        return None;
    }

    let lo_x = inset;
    let hi_x = source_width as f64 - 1.0 - inset;
    let lo_y = inset;
    let hi_y = source_height as f64 - 1.0 - inset;

    // Preimage of the inset source rectangle under the transform.
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for (sx, sy) in [(lo_x, lo_y), (hi_x, lo_y), (hi_x, hi_y), (lo_x, hi_y)] {
        let dx = sx - transform.tx;
        let dy = sy - transform.ty;
        let x = (transform.a * dx + transform.b * dy) / det;
        let y = (-transform.b * dx + transform.a * dy) / det;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }

    // Intersect with the output bounds.
    let min_x = min_x.max(0.0);
    let max_x = max_x.min(output_width as f64 - 1.0);
    let min_y = min_y.max(0.0);
    let max_y = max_y.min(output_height as f64 - 1.0);
    if min_x > max_x || min_y > max_y {
        return None;
    }

    let center_x = 0.5 * (min_x + max_x);
    let center_y = 0.5 * (min_y + max_y);
    let half_x = 0.5 * (max_x - min_x);
    let half_y = 0.5 * (max_y - min_y);

    // Each source-space bound is a half-plane in output coordinates. With the
    // rectangle written as centre +/- lambda * half, every constraint gives an
    // upper bound on lambda in closed form.
    let constraints = [
        (transform.a, -transform.b, hi_x - transform.tx),
        (-transform.a, transform.b, transform.tx - lo_x),
        (transform.b, transform.a, hi_y - transform.ty),
        (-transform.b, -transform.a, transform.ty - lo_y),
    ];
    let mut lambda = 1.0f64;
    for (normal_x, normal_y, limit) in constraints {
        let denominator = normal_x.abs() * half_x + normal_y.abs() * half_y;
        let numerator = limit - (normal_x * center_x + normal_y * center_y);
        if denominator <= 1.0e-15 {
            if numerator < -1.0e-12 {
                return None;
            }
        } else {
            lambda = lambda.min(numerator / denominator);
        }
    }

    if !lambda.is_finite() || lambda <= 0.0 {
        return None;
    }
    let lambda = lambda.min(1.0);

    // A small tolerance keeps exact integer boundaries from rounding outward.
    let epsilon = 1.0e-9;
    let x0 = (center_x - lambda * half_x - epsilon).ceil();
    let x1 = (center_x + lambda * half_x + epsilon).floor();
    let y0 = (center_y - lambda * half_y - epsilon).ceil();
    let y1 = (center_y + lambda * half_y + epsilon).floor();
    if x1 < x0 || y1 < y0 {
        return None;
    }

    let rect = Rect {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0 + 1.0) as u32,
        h: (y1 - y0 + 1.0) as u32,
    };
    if rect.x + rect.w > output_width || rect.y + rect.h > output_height {
        return None;
    }
    Some(rect)
}

/// Intersection of the valid rectangles of several transforms.
///
/// All transforms must describe the same output and source dimensions. This is
/// the crop shared by every frame in an aligned set.
pub fn common_valid_rect(
    output_width: u32,
    output_height: u32,
    source_width: u32,
    source_height: u32,
    transforms: &[Similarity],
) -> Option<Rect> {
    let mut common: Option<Rect> = None;
    for transform in transforms {
        let rect = valid_rect(
            output_width,
            output_height,
            source_width,
            source_height,
            transform,
        )?;
        common = Some(match common {
            None => rect,
            Some(current) => {
                let x0 = current.x.max(rect.x);
                let y0 = current.y.max(rect.y);
                let x1 = (current.x + current.w).min(rect.x + rect.w);
                let y1 = (current.y + current.h).min(rect.y + rect.h);
                if x1 <= x0 || y1 <= y0 {
                    return None;
                }
                Rect {
                    x: x0,
                    y: y0,
                    w: x1 - x0,
                    h: y1 - y0,
                }
            }
        });
    }
    common
}

/// Copy an axis-aligned rectangle out of an interleaved image.
///
/// # Errors
///
/// Returns [`WarpError`] for zero dimensions, zero channels, a mismatched
/// buffer, or a rectangle outside the image.
pub fn crop(
    source: &[f32],
    width: u32,
    height: u32,
    channels: usize,
    rect: Rect,
) -> Result<Vec<f32>, WarpError> {
    if width == 0 || height == 0 {
        return Err(WarpError::EmptyImage);
    }
    if channels == 0 {
        return Err(WarpError::NoChannels);
    }
    let expected = width as usize * height as usize * channels;
    if source.len() != expected {
        return Err(WarpError::LengthMismatch {
            expected,
            actual: source.len(),
        });
    }
    if rect.w == 0 || rect.h == 0 || rect.x + rect.w > width || rect.y + rect.h > height {
        return Err(WarpError::InvalidRect);
    }

    let source_stride = width as usize * channels;
    let crop_stride = rect.w as usize * channels;
    let mut output = Vec::with_capacity(crop_stride * rect.h as usize);
    for y in 0..rect.h as usize {
        let start = (rect.y as usize + y) * source_stride + rect.x as usize * channels;
        output.extend_from_slice(&source[start..start + crop_stride]);
    }
    Ok(output)
}

fn is_identity(transform: &Similarity) -> bool {
    transform.a == 1.0 && transform.b == 0.0 && transform.tx == 0.0 && transform.ty == 0.0
}

/// Six source indices for taps `base .. base + 5`, clamped to the image.
#[inline]
fn tap_offsets(base: i64, len: usize) -> [usize; 6] {
    let last = len as i64 - 1;
    [
        (base).clamp(0, last) as usize,
        (base + 1).clamp(0, last) as usize,
        (base + 2).clamp(0, last) as usize,
        (base + 3).clamp(0, last) as usize,
        (base + 4).clamp(0, last) as usize,
        (base + 5).clamp(0, last) as usize,
    ]
}

/// Precomputed, normalised Lanczos3 weights for six taps at each subpixel
/// position.
struct KernelTable {
    /// `table[i]` holds weights for fractional offset `i / SUBPIXEL_STEPS`.
    table: Vec<[f32; 6]>,
}

impl KernelTable {
    fn new() -> Self {
        let mut table = Vec::with_capacity(SUBPIXEL_STEPS + 1);
        for step in 0..=SUBPIXEL_STEPS {
            let frac = step as f64 / SUBPIXEL_STEPS as f64;
            let mut weights = [0.0f64; 6];
            for (tap, weight) in weights.iter_mut().enumerate() {
                *weight = lanczos3(frac + 2.0 - tap as f64);
            }
            let sum: f64 = weights.iter().sum();
            let mut quantized = [0.0f32; 6];
            for (value, weight) in quantized.iter_mut().zip(weights) {
                *value = (weight / sum) as f32;
            }
            table.push(quantized);
        }
        Self { table }
    }

    /// Linearly interpolate the table at `frac` in `0..=1`.
    #[inline]
    fn weights(&self, frac: f32) -> [f32; 6] {
        let position = f64::from(frac.clamp(0.0, 1.0)) * SUBPIXEL_STEPS as f64;
        let index = position.floor() as usize;
        if index >= SUBPIXEL_STEPS {
            return self.table[SUBPIXEL_STEPS];
        }
        let t = (position - index as f64) as f32;
        let left = &self.table[index];
        let right = &self.table[index + 1];
        let mut weights = [0.0f32; 6];
        for (weight, (&a, &b)) in weights.iter_mut().zip(left.iter().zip(right)) {
            *weight = a + (b - a) * t;
        }
        weights
    }
}

/// Separable Gaussian blur with edge clamping, in source pixels.
fn gaussian_blur(
    source: &[f32],
    width: usize,
    height: usize,
    channels: usize,
    sigma: f64,
) -> Vec<f32> {
    if sigma <= 0.0 {
        return source.to_vec();
    }

    let radius = (3.0 * sigma).ceil() as i64;
    let mut kernel = Vec::with_capacity((2 * radius + 1) as usize);
    let mut sum = 0.0f64;
    for offset in -radius..=radius {
        let x = offset as f64 / sigma;
        let weight = (-0.5 * x * x).exp();
        kernel.push(weight);
        sum += weight;
    }
    let kernel: Vec<f32> = kernel.into_iter().map(|w| (w / sum) as f32).collect();

    let row_len = width * channels;
    let mut horizontal = vec![0.0f32; source.len()];
    horizontal
        .par_chunks_mut(row_len)
        .enumerate()
        .for_each(|(y, row)| {
            let row_start = y * row_len;
            for (x, pixel) in row.chunks_mut(channels).enumerate() {
                for (channel, value) in pixel.iter_mut().enumerate() {
                    let mut accumulator = 0.0f32;
                    for (index, &weight) in kernel.iter().enumerate() {
                        let sample_x =
                            (x as i64 + index as i64 - radius).clamp(0, width as i64 - 1) as usize;
                        accumulator += weight * source[row_start + sample_x * channels + channel];
                    }
                    *value = accumulator;
                }
            }
        });

    let mut output = vec![0.0f32; source.len()];
    output
        .par_chunks_mut(row_len)
        .enumerate()
        .for_each(|(y, row)| {
            for (x, pixel) in row.chunks_mut(channels).enumerate() {
                for (channel, value) in pixel.iter_mut().enumerate() {
                    let mut accumulator = 0.0f32;
                    for (index, &weight) in kernel.iter().enumerate() {
                        let sample_y =
                            (y as i64 + index as i64 - radius).clamp(0, height as i64 - 1) as usize;
                        accumulator +=
                            weight * horizontal[sample_y * row_len + x * channels + channel];
                    }
                    *value = accumulator;
                }
            }
        });
    output
}
