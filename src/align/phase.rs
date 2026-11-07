//! Phase correlation and subpixel translation estimation.
//!
//! Implements translation estimation from `docs/alignment.md` §3. The
//! cross-power spectrum is normalised to unit magnitude, so the inverse FFT is
//! a correlation surface with a sharp peak at the translation.
//!
//! Shift convention: [`phase_correlate`] takes a `source` and a `reference`
//! and returns the translation `(dx, dy)` such that
//! `source(x + dx, y + dy) ≈ reference(x, y)`. This is the same convention as
//! [`crate::Similarity`], whose `tx`/`ty` map output coordinates to source
//! coordinates.
//!
//! Sizes are zero-padded to the next power of two per axis before the FFT.
//! Subpixel refinement uses the separable parabolic fit from the doc. The
//! optional Guizar-Sicairos upsampled DFT is deferred until the parabolic fit
//! is measured against the S1.2 gate.
//!
//! The 2D FFT helper is crate-visible so the Fourier–Mellin stage (S1.3) can
//! share it.

use std::f64::consts::PI;
use std::fmt;
use std::ops::{Add, Mul, Sub};

/// Peak-to-second-peak ratio below which the estimate is unreliable.
pub const MIN_CONFIDENCE: f64 = 1.5;

/// Translation estimate from phase correlation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseCorrelation {
    /// Horizontal translation in pixels:
    /// `source(x + dx, y + dy) ≈ reference(x, y)`.
    pub dx: f64,
    /// Vertical translation in pixels.
    pub dy: f64,
    /// Peak value of the correlation surface, in `0..=1` for normalised
    /// cross-power.
    pub peak: f64,
    /// Peak divided by the strongest peak outside the 5×5 neighbourhood of the
    /// main peak. Values below [`MIN_CONFIDENCE`] are unreliable.
    pub confidence: f64,
}

impl PhaseCorrelation {
    /// Whether the peak is strong enough to trust (`docs/alignment.md` §3).
    #[must_use]
    pub fn is_reliable(&self) -> bool {
        self.confidence >= MIN_CONFIDENCE
    }
}

/// Invalid input to [`phase_correlate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseError {
    /// One of the image dimensions is zero.
    EmptyInput,
    /// A buffer length does not match `width * height`.
    LengthMismatch {
        expected: usize,
        source: usize,
        reference: usize,
    },
}

impl fmt::Display for PhaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(formatter, "phase correlation needs non-zero dimensions"),
            Self::LengthMismatch {
                expected,
                source,
                reference,
            } => write!(
                formatter,
                "phase correlation buffer length mismatch: expected {expected}, \
                 source {source}, reference {reference}"
            ),
        }
    }
}

impl std::error::Error for PhaseError {}

/// Relative floor for the cross-power normalisation. Spectrum bins weaker
/// than this fraction of the strongest bin are treated as noise and zeroed,
/// so f32 rounding noise does not dominate the unit-magnitude spectrum.
const NORMALIZATION_FLOOR: f64 = 1e-6;

/// Estimate the translation of `source` relative to `reference`.
///
/// Both buffers are row-major and must contain `width * height` samples.
///
/// # Errors
///
/// Returns [`PhaseError`] when a dimension is zero or a buffer length does not
/// match `width * height`.
pub fn phase_correlate(
    source: &[f32],
    reference: &[f32],
    width: u32,
    height: u32,
) -> Result<PhaseCorrelation, PhaseError> {
    if width == 0 || height == 0 {
        return Err(PhaseError::EmptyInput);
    }

    let expected = width as usize * height as usize;
    if source.len() != expected || reference.len() != expected {
        return Err(PhaseError::LengthMismatch {
            expected,
            source: source.len(),
            reference: reference.len(),
        });
    }

    let fft_width = (width as usize).next_power_of_two();
    let fft_height = (height as usize).next_power_of_two();
    let mut source_spectrum = vec![Complex::ZERO; fft_width * fft_height];
    let mut reference_spectrum = vec![Complex::ZERO; fft_width * fft_height];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let padded = y * fft_width + x;
            let unpadded = y * width as usize + x;
            source_spectrum[padded] = Complex::new(f64::from(source[unpadded]), 0.0);
            reference_spectrum[padded] = Complex::new(f64::from(reference[unpadded]), 0.0);
        }
    }

    fft2d(&mut source_spectrum, fft_width, fft_height, false);
    fft2d(&mut reference_spectrum, fft_width, fft_height, false);

    // R = F_source * conj(F_reference) / (|...| + eps), with a relative floor
    // so near-zero bins do not turn into unit-magnitude noise.
    let mut max_magnitude = 0.0f64;
    for (index, value) in source_spectrum.iter_mut().enumerate() {
        let cross = *value * reference_spectrum[index].conj();
        max_magnitude = max_magnitude.max(cross.norm());
        *value = cross;
    }

    let floor = max_magnitude * NORMALIZATION_FLOOR;
    for value in &mut source_spectrum {
        let magnitude = value.norm();
        *value = if magnitude > floor {
            value.scale(1.0 / (magnitude + f64::EPSILON))
        } else {
            Complex::ZERO
        };
    }
    drop(reference_spectrum);

    let mut surface = source_spectrum;
    fft2d(&mut surface, fft_width, fft_height, true);

    let (peak_index, peak_value) = find_peak(&surface);
    let peak_x = peak_index % fft_width;
    let peak_y = peak_index / fft_width;

    let confidence = peak_confidence(&surface, fft_width, fft_height, peak_x, peak_y, peak_value);

    let left = surface[peak_y * fft_width + neighbor(peak_x, -1, fft_width)].re;
    let right = surface[peak_y * fft_width + neighbor(peak_x, 1, fft_width)].re;
    let up = surface[neighbor(peak_y, -1, fft_height) * fft_width + peak_x].re;
    let down = surface[neighbor(peak_y, 1, fft_height) * fft_width + peak_x].re;

    let offset_x = parabolic_offset(left, peak_value, right);
    let offset_y = parabolic_offset(up, peak_value, down);

    Ok(PhaseCorrelation {
        dx: signed_index(peak_x, fft_width) + offset_x,
        dy: signed_index(peak_y, fft_height) + offset_y,
        peak: peak_value,
        confidence,
    })
}

fn find_peak(surface: &[Complex]) -> (usize, f64) {
    let mut peak_index = 0;
    let mut peak_value = f64::NEG_INFINITY;
    for (index, value) in surface.iter().enumerate() {
        if value.re.is_finite() && value.re > peak_value {
            peak_index = index;
            peak_value = value.re;
        }
    }
    (peak_index, peak_value)
}

fn peak_confidence(
    surface: &[Complex],
    width: usize,
    height: usize,
    peak_x: usize,
    peak_y: usize,
    peak: f64,
) -> f64 {
    if peak <= 0.0 || peak.is_nan() {
        return 0.0;
    }

    let mut second = 0.0f64;
    for y in 0..height {
        for x in 0..width {
            if wrapped_delta(x, peak_x, width).abs() <= 2
                && wrapped_delta(y, peak_y, height).abs() <= 2
            {
                continue;
            }
            let value = surface[y * width + x].re;
            if value.is_finite() && value > second {
                second = value;
            }
        }
    }

    peak / second.max(1e-12)
}

/// Separable parabolic fit. Returns an offset in `-0.5..=0.5`.
fn parabolic_offset(left: f64, center: f64, right: f64) -> f64 {
    let denominator = left - 2.0 * center + right;
    if denominator.abs() < 1e-12 {
        return 0.0;
    }
    let offset = 0.5 * (left - right) / denominator;
    if offset.is_finite() {
        offset.clamp(-0.5, 0.5)
    } else {
        0.0
    }
}

fn neighbor(index: usize, offset: isize, len: usize) -> usize {
    (index as isize + offset).rem_euclid(len as isize) as usize
}

fn wrapped_delta(a: usize, b: usize, len: usize) -> isize {
    let half = (len / 2) as isize;
    let mut delta = a as isize - b as isize;
    if delta > half {
        delta -= len as isize;
    } else if delta < -half {
        delta += len as isize;
    }
    delta
}

fn signed_index(index: usize, len: usize) -> f64 {
    let half = (len / 2) as isize;
    let index = index as isize;
    if index > half {
        (index - len as isize) as f64
    } else {
        index as f64
    }
}

/// In-place 2D complex FFT. Each dimension must be a power of two.
///
/// Crate-visible so the Fourier–Mellin stage (S1.3) can share it.
pub(crate) fn fft2d(buffer: &mut [Complex], width: usize, height: usize, inverse: bool) {
    debug_assert!(width.is_power_of_two());
    debug_assert!(height.is_power_of_two());
    debug_assert_eq!(buffer.len(), width * height);

    let mut row = vec![Complex::ZERO; width];
    for y in 0..height {
        let start = y * width;
        row.copy_from_slice(&buffer[start..start + width]);
        fft1d(&mut row, inverse);
        buffer[start..start + width].copy_from_slice(&row);
    }

    let mut column = vec![Complex::ZERO; height];
    for x in 0..width {
        for (y, value) in column.iter_mut().enumerate() {
            *value = buffer[y * width + x];
        }
        fft1d(&mut column, inverse);
        for (y, value) in column.iter().enumerate() {
            buffer[y * width + x] = *value;
        }
    }
}

/// In-place iterative radix-2 Cooley–Tukey FFT.
fn fft1d(buffer: &mut [Complex], inverse: bool) {
    let len = buffer.len();
    debug_assert!(len.is_power_of_two());

    let mut reversed = 0usize;
    for index in 1..len {
        let mut bit = len >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed |= bit;
        if index < reversed {
            buffer.swap(index, reversed);
        }
    }

    let sign = if inverse { 1.0 } else { -1.0 };
    let mut stage = 2;
    while stage <= len {
        let angle = sign * 2.0 * PI / stage as f64;
        let twiddle_step = Complex::new(angle.cos(), angle.sin());
        for start in (0..len).step_by(stage) {
            let mut twiddle = Complex::new(1.0, 0.0);
            for offset in 0..stage / 2 {
                let even = buffer[start + offset];
                let odd = buffer[start + offset + stage / 2] * twiddle;
                buffer[start + offset] = even + odd;
                buffer[start + offset + stage / 2] = even - odd;
                twiddle = twiddle * twiddle_step;
            }
        }
        stage <<= 1;
    }

    if inverse {
        let scale = 1.0 / len as f64;
        for value in buffer.iter_mut() {
            *value = value.scale(scale);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Complex {
    pub(crate) re: f64,
    pub(crate) im: f64,
}

impl Complex {
    pub(crate) const ZERO: Self = Self { re: 0.0, im: 0.0 };

    pub(crate) const fn new(re: f64, im: f64) -> Self {
        Self { re, im }
    }

    pub(crate) fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }

    pub(crate) fn norm(self) -> f64 {
        self.re.hypot(self.im)
    }

    pub(crate) fn scale(self, factor: f64) -> Self {
        Self::new(self.re * factor, self.im * factor)
    }
}

impl Add for Complex {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(self.re + other.re, self.im + other.im)
    }
}

impl Sub for Complex {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(self.re - other.re, self.im - other.im)
    }
}

impl Mul for Complex {
    type Output = Self;

    fn mul(self, other: Self) -> Self {
        Self::new(
            self.re * other.re - self.im * other.im,
            self.re * other.im + self.im * other.re,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fft_round_trips() {
        let mut values: Vec<Complex> = (0..32)
            .map(|index| Complex::new((index as f64 * 0.37).sin(), (index as f64 * 0.11).cos()))
            .collect();
        let original = values.clone();
        fft1d(&mut values, false);
        fft1d(&mut values, true);
        for (actual, expected) in values.iter().zip(original) {
            assert!((actual.re - expected.re).abs() < 1e-12);
            assert!((actual.im - expected.im).abs() < 1e-12);
        }
    }

    #[test]
    fn fft_of_impulse_is_flat() {
        let mut values = vec![Complex::ZERO; 8];
        values[0] = Complex::new(1.0, 0.0);
        fft1d(&mut values, false);
        for value in values {
            assert!((value.re - 1.0).abs() < 1e-12);
            assert!(value.im.abs() < 1e-12);
        }
    }

    #[test]
    fn fft_of_shifted_impulse_has_phase_ramp() {
        let mut values = vec![Complex::ZERO; 8];
        values[1] = Complex::new(1.0, 0.0);
        fft1d(&mut values, false);
        for (index, value) in values.iter().enumerate() {
            let angle = -2.0 * PI * index as f64 / 8.0;
            assert!((value.re - angle.cos()).abs() < 1e-12);
            assert!((value.im - angle.sin()).abs() < 1e-12);
        }
    }

    #[test]
    fn fft2d_of_shifted_impulse_has_phase_ramp() {
        let width = 8;
        let height = 8;
        let mut values = vec![Complex::ZERO; width * height];
        values[1] = Complex::new(1.0, 0.0);
        fft2d(&mut values, width, height, false);
        for y in 0..height {
            for x in 0..width {
                let angle = -2.0 * PI * x as f64 / width as f64;
                let value = values[y * width + x];
                assert!((value.re - angle.cos()).abs() < 1e-12, "{x},{y}");
                assert!((value.im - angle.sin()).abs() < 1e-12, "{x},{y}");
            }
        }
    }

    #[test]
    fn fft2d_round_trips() {
        let width = 8;
        let height = 4;
        let mut values: Vec<Complex> = (0..width * height)
            .map(|index| Complex::new((index as f64 * 0.37).sin(), (index as f64 * 0.11).cos()))
            .collect();
        let original = values.clone();
        fft2d(&mut values, width, height, false);
        fft2d(&mut values, width, height, true);
        for (actual, expected) in values.iter().zip(original) {
            assert!((actual.re - expected.re).abs() < 1e-12);
            assert!((actual.im - expected.im).abs() < 1e-12);
        }
    }

    #[test]
    fn fft_of_cosine_is_sparse() {
        let len = 64;
        let mut values: Vec<Complex> = (0..len)
            .map(|index| Complex::new((2.0 * PI * 3.0 * index as f64 / len as f64).cos(), 0.0))
            .collect();
        fft1d(&mut values, false);
        let nonzero = values.iter().filter(|value| value.norm() > 1e-6).count();
        assert_eq!(nonzero, 2, "{values:?}");
    }
}
