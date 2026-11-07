//! Per-level noise sigma and floors.
//!
//! Implements `docs/focus-measure.md` §3. Each pyramid level gets a robust
//! noise scale from the median absolute deviation (MAD) of `Lap_l` over a
//! low-gradient mask. The mask is built from the 20th percentile of
//! `|grad G_l|`, so textured pixels do not contribute to the estimate.
//!
//! The MAD is taken around the median of the selected Laplacian samples. For
//! the band-pass levels the Laplacian is zero-mean, so this matches
//! `1.4826 * median(|Lap_l|)`. Centring also keeps the coarsest residual level
//! (`Lap_L = G_L`) from reporting its brightness as noise.

use super::pyramid::{Pyramid, PyramidLevel};

/// Default noise multiplier `k` for `tau_l = k * sigma_l^2` (§3, §7).
pub const DEFAULT_NOISE_K: f32 = 3.0;
/// Scale from median absolute deviation to standard deviation for Gaussian data.
pub const MAD_TO_SIGMA: f32 = 1.4826;
/// Fraction of lowest gradients kept by the low-gradient mask (§3).
pub const LOW_GRADIENT_PERCENTILE: f32 = 0.20;
/// Lower bound substituted for a zero sigma on clean synthetic input (§3).
pub const SIGMA_EPSILON: f32 = 1.0e-6;

/// Noise estimate for one pyramid level.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NoiseEstimate {
    /// Robust per-level noise standard deviation.
    pub sigma: f32,
    /// Noise floor `tau = k * sigma^2`, subtracted from energies in §4.
    pub tau: f32,
    /// Gradient threshold used by the low-gradient mask.
    pub gradient_threshold: f32,
    /// Number of Laplacian samples that contributed to the estimate.
    pub samples: usize,
}

impl NoiseEstimate {
    /// Estimate for an empty level.
    fn empty() -> Self {
        let sigma = SIGMA_EPSILON;
        Self {
            sigma,
            tau: noise_floor(sigma, DEFAULT_NOISE_K),
            gradient_threshold: 0.0,
            samples: 0,
        }
    }
}

/// Per-level noise estimates for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NoiseModel {
    /// Estimates from the finest level to the coarsest residual.
    pub levels: Vec<NoiseEstimate>,
}

impl NoiseModel {
    /// Estimate for level `index`, if present.
    #[must_use]
    pub fn level(&self, index: usize) -> Option<&NoiseEstimate> {
        self.levels.get(index)
    }

    /// Per-level sigmas, finest level first.
    pub fn sigmas(&self) -> impl Iterator<Item = f32> + '_ {
        self.levels.iter().map(|estimate| estimate.sigma)
    }

    /// Per-level noise floors, finest level first.
    pub fn taus(&self) -> impl Iterator<Item = f32> + '_ {
        self.levels.iter().map(|estimate| estimate.tau)
    }
}

/// Estimate noise for every level of a built pyramid.
#[must_use]
pub fn estimate(pyramid: &Pyramid, k: f32) -> NoiseModel {
    estimate_levels(pyramid.levels(), k)
}

/// Estimate noise for a slice of levels.
#[must_use]
pub fn estimate_levels(levels: &[PyramidLevel], k: f32) -> NoiseModel {
    NoiseModel {
        levels: levels
            .iter()
            .map(|level| estimate_level(level, k))
            .collect(),
    }
}

/// Estimate noise for a single level.
///
/// `k` is the noise multiplier used for the floor `tau = k * sigma^2`; pass
/// [`DEFAULT_NOISE_K`] unless debugging.
///
/// # Panics
///
/// Panics if the level's Gaussian or Laplacian buffer length does not match
/// `width * height`.
#[must_use]
pub fn estimate_level(level: &PyramidLevel, k: f32) -> NoiseEstimate {
    let width = level.width as usize;
    let height = level.height as usize;
    let count = width * height;

    if count == 0 {
        return NoiseEstimate::empty();
    }

    assert_eq!(
        level.gaussian.len(),
        count,
        "Gaussian buffer length mismatch"
    );
    assert_eq!(
        level.laplacian.len(),
        count,
        "Laplacian buffer length mismatch"
    );

    let mut gradients = Vec::with_capacity(count);
    for y in 0..height {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(height - 1);
        for x in 0..width {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(width - 1);
            let gx = 0.5 * (level.gaussian[y * width + x1] - level.gaussian[y * width + x0]);
            let gy = 0.5 * (level.gaussian[y1 * width + x] - level.gaussian[y0 * width + x]);
            gradients.push(gx.hypot(gy));
        }
    }

    let mut finite_gradients: Vec<f32> = gradients
        .iter()
        .copied()
        .filter(|gradient| gradient.is_finite())
        .collect();
    let gradient_threshold = percentile(&mut finite_gradients, LOW_GRADIENT_PERCENTILE);

    // The strict comparison is from §3. If the percentile lands on a tie
    // (notably a flat region whose gradient is exactly zero), fall back to
    // including that tie so the mask is not empty. If every pixel is textured,
    // fall back to the whole level rather than returning no estimate.
    let mut samples = collect_laplacian(level, &gradients, gradient_threshold, false);
    if samples.is_empty() {
        samples = collect_laplacian(level, &gradients, gradient_threshold, true);
    }
    if samples.is_empty() {
        samples = level
            .laplacian
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect();
    }

    let sigma = if samples.is_empty() {
        SIGMA_EPSILON
    } else {
        let center = median(&mut samples);
        for value in &mut samples {
            *value = (*value - center).abs();
        }
        (MAD_TO_SIGMA * median(&mut samples)).max(SIGMA_EPSILON)
    };

    NoiseEstimate {
        sigma,
        tau: noise_floor(sigma, k),
        gradient_threshold,
        samples: samples.len(),
    }
}

/// Noise floor `tau = k * sigma^2` from §3.
#[must_use]
pub fn noise_floor(sigma: f32, k: f32) -> f32 {
    let sigma = if sigma.is_finite() {
        sigma.abs().max(SIGMA_EPSILON)
    } else {
        SIGMA_EPSILON
    };
    k.max(0.0) * sigma * sigma
}

/// Apply `E' = max(E - tau, 0)` from §3 to an energy buffer.
pub fn subtract_noise_floor(energy: &mut [f32], tau: f32) {
    let tau = if tau.is_finite() { tau.max(0.0) } else { 0.0 };
    for value in energy {
        *value = (*value - tau).max(0.0);
    }
}

/// Collect Laplacian samples whose gradient is below (or, for the tie
/// fallback, at most) the mask threshold.
fn collect_laplacian(
    level: &PyramidLevel,
    gradients: &[f32],
    threshold: f32,
    include_ties: bool,
) -> Vec<f32> {
    level
        .laplacian
        .iter()
        .zip(gradients)
        .filter(|(value, gradient)| {
            value.is_finite()
                && if include_ties {
                    **gradient <= threshold
                } else {
                    **gradient < threshold
                }
        })
        .map(|(&value, _)| value)
        .collect()
}

/// Nearest-rank percentile with the ranks interpolated at the edges.
fn percentile(values: &mut [f32], fraction: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let rank = ((values.len() - 1) as f32 * fraction.clamp(0.0, 1.0)).round() as usize;
    *values.select_nth_unstable_by(rank, f32::total_cmp).1
}

/// Median of a non-empty slice. Returns zero for an empty slice.
fn median(values: &mut [f32]) -> f32 {
    match values.len() {
        0 => 0.0,
        1 => values[0],
        len if len % 2 == 1 => *values.select_nth_unstable_by(len / 2, f32::total_cmp).1,
        len => {
            let (lower, upper, _) = values.select_nth_unstable_by(len / 2, f32::total_cmp);
            let lower_max = lower.iter().copied().fold(f32::NEG_INFINITY, f32::max);
            0.5 * (lower_max + *upper)
        }
    }
}
