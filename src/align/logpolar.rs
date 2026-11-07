//! Fourier-Mellin rotation and scale estimation.
//!
//! Implements the coarse rotation/scale stage from `docs/alignment.md` §4.
//! The high-passed luma is transformed to the log magnitude spectrum and
//! resampled into log-polar coordinates. A scale/rotation pair becomes a
//! translation there, which [`phase_correlate`] recovers.
//!
//! Convention: the returned [`FourierMellinEstimate::rotation`] and
//! [`FourierMellinEstimate::scale`] describe the similarity that maps reference
//! coordinates to source coordinates, matching [`crate::Similarity`]:
//!
//! ```text
//! x_source = center + scale * R(rotation) * (x_reference - center)
//! ```
//!
//! The magnitude spectrum is invariant to translation, so this stage ignores
//! translation. It is deliberately coarse; ECC refinement (S1.4) follows.

use std::f64::consts::{PI, TAU};
use std::fmt;

use super::phase::{Complex, PhaseError, fft2d, phase_correlate};

/// Default maximum rotation, 3° (`docs/alignment.md` §1).
pub const DEFAULT_MAX_ROTATION: f64 = 3.0 * PI / 180.0;
/// Default minimum scale, 0.97 (`docs/alignment.md` §1).
pub const DEFAULT_MIN_SCALE: f64 = 0.97;
/// Default maximum scale, 1.03 (`docs/alignment.md` §1).
pub const DEFAULT_MAX_SCALE: f64 = 1.03;

/// Parameters for [`estimate_rotation_scale`].
#[derive(Debug, Clone, PartialEq)]
pub struct FourierMellinParams {
    /// Largest accepted rotation, in radians, applied symmetrically.
    pub max_rotation: f64,
    /// Smallest accepted scale.
    pub min_scale: f64,
    /// Largest accepted scale.
    pub max_scale: f64,
    /// Radial samples in the log-polar image. Rounded up to a power of two.
    pub radial_samples: usize,
    /// Angular samples in the log-polar image. Rounded up to a power of two.
    pub angular_samples: usize,
    /// Lowest sampled frequency radius as a fraction of the FFT half-size.
    /// Frequencies below this are dominated by the high-pass transition.
    pub min_radius_fraction: f64,
    /// High-pass Gaussian sigma in pixels.
    pub high_pass_sigma: f32,
}

impl Default for FourierMellinParams {
    fn default() -> Self {
        Self {
            max_rotation: DEFAULT_MAX_ROTATION,
            min_scale: DEFAULT_MIN_SCALE,
            max_scale: DEFAULT_MAX_SCALE,
            radial_samples: 1024,
            angular_samples: 1024,
            min_radius_fraction: 0.07,
            high_pass_sigma: 3.0,
        }
    }
}

/// Rotation/scale estimate from Fourier-Mellin correlation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FourierMellinEstimate {
    /// Uniform scale mapping reference coordinates to source coordinates.
    pub scale: f64,
    /// Rotation in radians mapping reference coordinates to source
    /// coordinates. Positive is counter-clockwise in image coordinates.
    pub rotation: f64,
    /// Correlation peak value in `0..=1`.
    pub peak: f64,
    /// Raw phase-correlation confidence. The magnitude spectrum is
    /// centrosymmetric, so the 180° twin peak is usually equally strong and
    /// this value is near 1; it is not a match-quality score.
    pub confidence: f64,
    /// Whether the scale estimate was clamped to the configured range.
    pub scale_clamped: bool,
    /// Whether the rotation estimate was clamped to the configured range.
    pub rotation_clamped: bool,
    /// Whether the `rotation + pi` candidate won the 180° disambiguation.
    pub flipped_180: bool,
}

/// Invalid input to [`estimate_rotation_scale`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FourierMellinError {
    /// One of the image dimensions is zero.
    EmptyInput,
    /// A buffer length does not match `width * height`.
    LengthMismatch {
        expected: usize,
        source: usize,
        reference: usize,
    },
    /// A parameter is outside its meaningful range.
    InvalidParameters,
    /// The shared phase-correlation stage rejected its input.
    Phase(PhaseError),
}

impl fmt::Display for FourierMellinError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(formatter, "Fourier-Mellin needs non-zero dimensions"),
            Self::LengthMismatch {
                expected,
                source,
                reference,
            } => write!(
                formatter,
                "Fourier-Mellin buffer length mismatch: expected {expected}, \
                 source {source}, reference {reference}"
            ),
            Self::InvalidParameters => write!(formatter, "invalid Fourier-Mellin parameters"),
            Self::Phase(error) => write!(formatter, "phase correlation failed: {error}"),
        }
    }
}

impl std::error::Error for FourierMellinError {}

/// Estimate rotation and scale of `source` relative to `reference`.
///
/// Both buffers are row-major encoded luma with `width * height` samples.
///
/// # Errors
///
/// Returns [`FourierMellinError`] for empty or mismatched inputs and invalid
/// parameters.
pub fn estimate_rotation_scale(
    source: &[f32],
    reference: &[f32],
    width: u32,
    height: u32,
    params: &FourierMellinParams,
) -> Result<FourierMellinEstimate, FourierMellinError> {
    if width == 0 || height == 0 {
        return Err(FourierMellinError::EmptyInput);
    }

    let expected = width as usize * height as usize;
    if source.len() != expected || reference.len() != expected {
        return Err(FourierMellinError::LengthMismatch {
            expected,
            source: source.len(),
            reference: reference.len(),
        });
    }

    let radial_samples = params.radial_samples.next_power_of_two().max(8);
    let angular_samples = params.angular_samples.next_power_of_two().max(8);
    if !params.max_rotation.is_finite()
        || params.max_rotation < 0.0
        || !params.min_scale.is_finite()
        || !params.max_scale.is_finite()
        || params.min_scale <= 0.0
        || params.min_scale > params.max_scale
        || !params.min_radius_fraction.is_finite()
        || params.min_radius_fraction <= 0.0
        || params.min_radius_fraction >= 0.5
        || !params.high_pass_sigma.is_finite()
        || params.high_pass_sigma < 0.0
    {
        return Err(FourierMellinError::InvalidParameters);
    }

    let width_usize = width as usize;
    let height_usize = height as usize;

    let source_high = high_pass(source, width_usize, height_usize, params.high_pass_sigma);
    let reference_high = high_pass(reference, width_usize, height_usize, params.high_pass_sigma);

    let (source_polar, radial_step, angular_step) = log_polar_magnitude(
        source,
        width_usize,
        height_usize,
        params.high_pass_sigma,
        radial_samples,
        angular_samples,
        params.min_radius_fraction,
    );
    let (reference_polar, _, _) = log_polar_magnitude(
        reference,
        width_usize,
        height_usize,
        params.high_pass_sigma,
        radial_samples,
        angular_samples,
        params.min_radius_fraction,
    );

    let correlation = phase_correlate(
        &source_polar,
        &reference_polar,
        angular_samples as u32,
        radial_samples as u32,
    )
    .map_err(FourierMellinError::Phase)?;

    // In the log-polar image the width axis is angle and the height axis is
    // log radius. `source(x + dx, y + dy) ~= reference(x, y)` therefore gives
    // `rotation = dx * angular_step` and `scale = exp(-dy * radial_step)`.
    let raw_rotation = correlation.dx * angular_step;
    let raw_scale = (-correlation.dy * radial_step).exp();

    let base = wrap_to_pi(raw_rotation);
    let candidates = [base, wrap_to_pi(base + PI)];
    let (correlation_0, correlation_1) = disambiguation_correlations(
        &source_high,
        &reference_high,
        width_usize,
        height_usize,
        raw_scale,
        candidates,
    );
    let flipped_180 = correlation_1 > correlation_0;
    let chosen = candidates[usize::from(flipped_180)];
    let rotation = wrap_to_half_pi(chosen);

    let scale = raw_scale.clamp(params.min_scale, params.max_scale);
    let scale_clamped = (scale - raw_scale).abs() > f64::EPSILON;

    let rotation = rotation.clamp(-params.max_rotation, params.max_rotation);
    let rotation_clamped = (rotation - wrap_to_half_pi(chosen)).abs() > f64::EPSILON;

    Ok(FourierMellinEstimate {
        scale,
        rotation,
        peak: correlation.peak,
        confidence: correlation.confidence,
        scale_clamped,
        rotation_clamped,
        flipped_180,
    })
}

fn wrap_to_pi(angle: f64) -> f64 {
    let mut wrapped = angle % TAU;
    if wrapped > PI {
        wrapped -= TAU;
    } else if wrapped <= -PI {
        wrapped += TAU;
    }
    wrapped
}

/// Map an angle modulo pi into `(-pi/2, pi/2]`.
fn wrap_to_half_pi(angle: f64) -> f64 {
    let mut wrapped = wrap_to_pi(angle);
    if wrapped > PI / 2.0 {
        wrapped -= PI;
    } else if wrapped <= -PI / 2.0 {
        wrapped += PI;
    }
    wrapped
}

fn disambiguation_correlations(
    source: &[f32],
    reference: &[f32],
    width: usize,
    height: usize,
    scale: f64,
    candidates: [f64; 2],
) -> (f64, f64) {
    let (source, source_width, source_height) =
        downsample_for_correlation(source, width, height, 256);
    let (reference, reference_width, reference_height) =
        downsample_for_correlation(reference, width, height, 256);
    debug_assert_eq!(
        (source_width, source_height),
        (reference_width, reference_height)
    );
    let first = normalized_correlation(
        &source,
        &reference,
        source_width,
        source_height,
        scale,
        candidates[0],
    );
    let second = normalized_correlation(
        &source,
        &reference,
        source_width,
        source_height,
        scale,
        candidates[1],
    );
    (first, second)
}

fn normalized_correlation(
    source: &[f32],
    reference: &[f32],
    width: usize,
    height: usize,
    scale: f64,
    rotation: f64,
) -> f64 {
    let (sin, cos) = rotation.sin_cos();
    let center_x = (width as f64 - 1.0) * 0.5;
    let center_y = (height as f64 - 1.0) * 0.5;

    let mut sum_a = 0.0;
    let mut sum_b = 0.0;
    let mut sum_aa = 0.0;
    let mut sum_bb = 0.0;
    let mut sum_ab = 0.0;
    let mut count = 0usize;

    for y in 0..height {
        for x in 0..width {
            let offset_x = x as f64 - center_x;
            let offset_y = y as f64 - center_y;
            let source_x = center_x + scale * (cos * offset_x - sin * offset_y);
            let source_y = center_y + scale * (sin * offset_x + cos * offset_y);
            let Some(sample) = sample_bilinear(source, width, height, source_x, source_y) else {
                continue;
            };
            let value = f64::from(reference[y * width + x]);
            sum_a += f64::from(sample);
            sum_b += value;
            sum_aa += f64::from(sample) * f64::from(sample);
            sum_bb += value * value;
            sum_ab += f64::from(sample) * value;
            count += 1;
        }
    }

    if count < 4 {
        return f64::NEG_INFINITY;
    }

    let n = count as f64;
    let covariance = sum_ab - sum_a * sum_b / n;
    let variance_a = sum_aa - sum_a * sum_a / n;
    let variance_b = sum_bb - sum_b * sum_b / n;
    let denominator = (variance_a.max(0.0) * variance_b.max(0.0)).sqrt();
    if denominator <= 1e-12 {
        f64::NEG_INFINITY
    } else {
        covariance / denominator
    }
}

fn sample_bilinear(image: &[f32], width: usize, height: usize, x: f64, y: f64) -> Option<f32> {
    if x < 0.0 || y < 0.0 || x > (width as f64 - 1.0) || y > (height as f64 - 1.0) {
        return None;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = (x - x0 as f64) as f32;
    let ty = (y - y0 as f64) as f32;

    let top = image[y0 * width + x0] * (1.0 - tx) + image[y0 * width + x1] * tx;
    let bottom = image[y1 * width + x0] * (1.0 - tx) + image[y1 * width + x1] * tx;
    Some(top * (1.0 - ty) + bottom * ty)
}

fn downsample_for_correlation(
    image: &[f32],
    width: usize,
    height: usize,
    max_edge: usize,
) -> (Vec<f32>, usize, usize) {
    let long = width.max(height);
    if long <= max_edge {
        return (image.to_vec(), width, height);
    }

    let factor = (long as f64 / max_edge as f64).ceil() as usize;
    let out_width = width.div_ceil(factor).max(1);
    let out_height = height.div_ceil(factor).max(1);
    let mut out = vec![0.0f32; out_width * out_height];

    for out_y in 0..out_height {
        let y0 = out_y * factor;
        let y1 = (y0 + factor).min(height);
        for out_x in 0..out_width {
            let x0 = out_x * factor;
            let x1 = (x0 + factor).min(width);
            let mut sum = 0.0f64;
            let mut count = 0usize;
            for y in y0..y1 {
                for x in x0..x1 {
                    sum += f64::from(image[y * width + x]);
                    count += 1;
                }
            }
            out[out_y * out_width + out_x] = if count > 0 {
                (sum / count as f64) as f32
            } else {
                0.0
            };
        }
    }

    (out, out_width, out_height)
}

/// High-pass `G - Gaussian(G, sigma)` (`docs/alignment.md` §2).
fn high_pass(src: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return src.to_vec();
    }
    let blurred = gaussian_blur(src, width, height, sigma);
    src.iter()
        .zip(blurred)
        .map(|(&value, blur)| value - blur)
        .collect()
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil().max(1.0) as i32;
    let denominator = 2.0 * sigma * sigma;
    let mut kernel = Vec::with_capacity((2 * radius + 1) as usize);
    let mut sum = 0.0;
    for offset in -radius..=radius {
        let value = (-(offset * offset) as f32 / denominator).exp();
        kernel.push(value);
        sum += value;
    }
    for value in &mut kernel {
        *value /= sum;
    }
    kernel
}

fn gaussian_blur(src: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    let kernel = gaussian_kernel(sigma);
    let radius = (kernel.len() / 2) as i32;
    let max_x = width as i32 - 1;
    let max_y = height as i32 - 1;

    let mut horizontal = vec![0.0; src.len()];
    for y in 0..height {
        let row = y * width;
        for x in 0..width {
            let mut sum = 0.0;
            for (tap, &weight) in kernel.iter().enumerate() {
                let sx = (x as i32 + tap as i32 - radius).clamp(0, max_x) as usize;
                sum += src[row + sx] * weight;
            }
            horizontal[row + x] = sum;
        }
    }

    let mut out = vec![0.0; src.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (tap, &weight) in kernel.iter().enumerate() {
                let sy = (y as i32 + tap as i32 - radius).clamp(0, max_y) as usize;
                sum += horizontal[sy * width + x] * weight;
            }
            out[y * width + x] = sum;
        }
    }
    out
}

/// Resample `log(|FFT| + eps)` into log-polar coordinates.
///
/// Returns the image with width = angle and height = log radius, plus the
/// per-sample steps used to convert pixel shifts back to rotation and log
/// scale.
fn log_polar_magnitude(
    image: &[f32],
    width: usize,
    height: usize,
    high_pass_sigma: f32,
    radial_samples: usize,
    angular_samples: usize,
    min_radius_fraction: f64,
) -> (Vec<f32>, f64, f64) {
    let fft_size = width.max(height).next_power_of_two().max(8);
    let mut spectrum = vec![Complex::ZERO; fft_size * fft_size];

    // Use a radial Hann window instead of a separable one. A rotationally
    // symmetric window rotates with the content, so it does not add a fixed
    // orientation-dependent pattern that would bias the correlation toward
    // zero rotation. The high-pass runs after the window so the window's own
    // low-frequency lobe, which is identical in both frames, is removed.
    let center_x = (width as f64 - 1.0) * 0.5;
    let center_y = (height as f64 - 1.0) * 0.5;
    let window_radius = (center_x * center_x + center_y * center_y).sqrt();
    let mut windowed = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let dx = x as f64 - center_x;
            let dy = y as f64 - center_y;
            let window = if window_radius > 0.0 {
                0.5 + 0.5 * (PI * dx.hypot(dy) / window_radius).cos()
            } else {
                1.0
            };
            windowed[y * width + x] = image[y * width + x] * window as f32;
        }
    }
    let filtered = high_pass(&windowed, width, height, high_pass_sigma);
    for y in 0..height {
        for x in 0..width {
            spectrum[y * fft_size + x] = Complex::new(f64::from(filtered[y * width + x]), 0.0);
        }
    }
    fft2d(&mut spectrum, fft_size, fft_size, false);

    let magnitude: Vec<f64> = spectrum.iter().map(|value| value.norm()).collect();
    let max_radius = fft_size as f64 * 0.5;
    let min_radius = (fft_size as f64 * min_radius_fraction)
        .max(1.0)
        .min(max_radius * 0.5);
    let radial_step = (max_radius / min_radius).ln() / radial_samples as f64;
    let angular_step = TAU / angular_samples as f64;

    let mut polar = vec![0.0f32; radial_samples * angular_samples];
    for radial in 0..radial_samples {
        let radius = min_radius * ((radial as f64 + 0.5) * radial_step).exp();
        for angular in 0..angular_samples {
            let theta = angular as f64 * angular_step;
            let fx = radius * theta.cos();
            let fy = radius * theta.sin();
            let value = sample_magnitude(&magnitude, fft_size, fx, fy);
            polar[radial * angular_samples + angular] = (value + 1e-12).ln() as f32;
        }
    }

    (polar, radial_step, angular_step)
}

/// Bilinear sample the uncentred FFT magnitude at signed frequency bins.
fn sample_magnitude(magnitude: &[f64], size: usize, fx: f64, fy: f64) -> f64 {
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let x0 = wrap_index(x0 as i64, size);
    let y0 = wrap_index(y0 as i64, size);
    let x1 = wrap_index(x0 as i64 + 1, size);
    let y1 = wrap_index(y0 as i64 + 1, size);

    let top = magnitude[y0 * size + x0] * (1.0 - tx) + magnitude[y0 * size + x1] * tx;
    let bottom = magnitude[y1 * size + x0] * (1.0 - tx) + magnitude[y1 * size + x1] * tx;
    top * (1.0 - ty) + bottom * ty
}

fn wrap_index(index: i64, size: usize) -> usize {
    index.rem_euclid(size as i64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_to_half_pi_folds_180_degree_ambiguity() {
        assert!((wrap_to_half_pi(2.0_f64.to_radians()) - 2.0_f64.to_radians()).abs() < 1e-12);
        assert!((wrap_to_half_pi(182.0_f64.to_radians()) - 2.0_f64.to_radians()).abs() < 1e-12);
        assert!((wrap_to_half_pi(-182.0_f64.to_radians()) + 2.0_f64.to_radians()).abs() < 1e-12);
    }

    #[test]
    fn rejects_empty_and_mismatched_buffers() {
        let params = FourierMellinParams::default();
        assert_eq!(
            estimate_rotation_scale(&[], &[], 0, 8, &params).unwrap_err(),
            FourierMellinError::EmptyInput
        );
        assert_eq!(
            estimate_rotation_scale(&[0.0; 3], &[0.0; 4], 2, 2, &params).unwrap_err(),
            FourierMellinError::LengthMismatch {
                expected: 4,
                source: 3,
                reference: 4,
            }
        );
    }

    #[test]
    fn rejects_invalid_parameters() {
        let params = FourierMellinParams {
            min_scale: 1.0,
            max_scale: 0.5,
            ..FourierMellinParams::default()
        };
        assert_eq!(
            estimate_rotation_scale(&[0.0; 64], &[0.0; 64], 8, 8, &params).unwrap_err(),
            FourierMellinError::InvalidParameters
        );
    }
}
