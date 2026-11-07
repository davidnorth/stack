//! Margin confidence and guided smoothing.
//!
//! Implements `docs/focus-measure.md` §5. The best and second-best per-pixel
//! energies determine a margin `(E1 - E2) / (E1 + eps)`; the margin is clamped
//! to `[0, 1]` and smoothed with a guided filter whose guide is encoded luma
//! `Y'`. After smoothing, the confidence is zeroed where the winning frame is
//! not trustworthy (coverage floor) or saturated.
//!
//! Per-frame calibration derives the noise scale and energy statistics that
//! the coverage gate and reports need. Calibration deliberately does **not**
//! rescale energies: §4 forbids equalizing frames with a global scalar because
//! that hides real focus differences. Exposure differences are matched before
//! the focus measure.

use super::energy::{aggregation_weight, box_filter};
use super::noise::SIGMA_EPSILON;

/// Additive constant in the margin denominator.
pub const MARGIN_EPSILON: f32 = 1.0e-6;
/// Guided-filter radius from §5 (`r = 8`).
pub const DEFAULT_GUIDE_RADIUS: u32 = 8;
/// Guided-filter regularization from §5 (`eps = 1e-3`).
pub const DEFAULT_GUIDE_EPSILON: f32 = 1.0e-3;
/// Coverage multiplier `tau_cov` from §5.
pub const DEFAULT_COVERAGE_TAU: f32 = 4.0;
/// Any channel at or above this value marks the winning frame as saturated.
pub const SATURATION_THRESHOLD: f32 = 0.995;

/// Options for [`ConfidenceMap::compute`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConfidenceOptions {
    /// Additive constant in `(E1 - E2) / (E1 + eps)`.
    pub margin_epsilon: f32,
    /// Guided-filter radius in pixels.
    pub guide_radius: u32,
    /// Guided-filter regularization.
    pub guide_epsilon: f32,
    /// Coverage multiplier in `E1 < tau_cov * sigma^2`.
    pub coverage_tau: f32,
}

impl Default for ConfidenceOptions {
    fn default() -> Self {
        Self {
            margin_epsilon: MARGIN_EPSILON,
            guide_radius: DEFAULT_GUIDE_RADIUS,
            guide_epsilon: DEFAULT_GUIDE_EPSILON,
            coverage_tau: DEFAULT_COVERAGE_TAU,
        }
    }
}

/// Distribution of a frame's aggregated energy map.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct EnergyStatistics {
    /// Smallest finite energy.
    pub min: f32,
    /// Largest finite energy.
    pub max: f32,
    /// Arithmetic mean of the finite energies.
    pub mean: f32,
    /// Median of the finite energies.
    pub median: f32,
    /// Nearest-rank 95th percentile of the finite energies.
    pub p95: f32,
}

impl EnergyStatistics {
    /// Summarize a frame's energy map. Non-finite samples are ignored.
    #[must_use]
    pub fn from_slice(energy: &[f32]) -> Self {
        let mut values: Vec<f32> = energy
            .iter()
            .copied()
            .filter(|value| value.is_finite())
            .collect();
        if values.is_empty() {
            return Self::default();
        }

        let min = values.iter().copied().fold(f32::INFINITY, f32::min);
        let max = values.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        values.sort_by(f32::total_cmp);

        let median = if values.len() % 2 == 1 {
            values[values.len() / 2]
        } else {
            let upper = values[values.len() / 2];
            let lower = values[values.len() / 2 - 1];
            0.5 * (lower + upper)
        };
        let p95_rank = ((values.len() - 1) as f32 * 0.95).round() as usize;

        Self {
            min,
            max,
            mean,
            median,
            p95: values[p95_rank],
        }
    }
}

/// Per-frame calibration used by the confidence and coverage gates.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FrameCalibration {
    /// Aggregated noise standard deviation over the §6 band levels.
    pub sigma: f32,
    /// Statistics of the frame's aggregated energy map.
    pub stats: EnergyStatistics,
}

impl FrameCalibration {
    /// Calibrate `energy` against per-level noise sigmas.
    ///
    /// `sigmas` is finest level first and may include the coarsest residual;
    /// that residual is excluded from the aggregate, matching
    /// [`super::energy::EnergyMaps::aggregate`].
    #[must_use]
    pub fn new(energy: &[f32], sigmas: &[f32]) -> Self {
        Self {
            sigma: aggregate_sigma(sigmas),
            stats: EnergyStatistics::from_slice(energy),
        }
    }

    /// Coverage floor `tau_cov * sigma^2` for this frame.
    #[must_use]
    pub fn coverage_floor(&self, tau_cov: f32) -> f32 {
        tau_cov.max(0.0) * self.sigma * self.sigma
    }
}

/// Weighted noise scale used by the aggregate energy map.
///
/// The §6 aggregation sums `w_l * E_l`, so the matching noise variance is
/// `sum_l w_l * sigma_l^2` over the band levels (the residual is excluded).
/// Returns [`SIGMA_EPSILON`] when no usable sigma is supplied.
#[must_use]
pub fn aggregate_sigma(sigmas: &[f32]) -> f32 {
    let band_count = if sigmas.len() > 1 {
        sigmas.len() - 1
    } else {
        sigmas.len()
    };
    let mut variance = 0.0f32;
    for (level, &sigma) in sigmas.iter().take(band_count).enumerate() {
        let Some(weight) = aggregation_weight(level) else {
            break;
        };
        if sigma.is_finite() {
            variance += weight * sigma.max(0.0) * sigma.max(0.0);
        }
    }
    variance.sqrt().max(SIGMA_EPSILON)
}

/// Per-pixel best frame, best-vs-second margin, and gated confidence.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ConfidenceMap {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Winning frame index at each pixel.
    pub best_frame: Vec<u32>,
    /// Best energy `E1` at each pixel.
    pub best_energy: Vec<f32>,
    /// Raw best-vs-second margin before smoothing or gating.
    pub margin: Vec<f32>,
    /// Smoothed confidence in `[0, 1]` after coverage and saturation gates.
    pub confidence: Vec<f32>,
}

impl ConfidenceMap {
    /// Compute best-frame selection and confidence.
    ///
    /// `energies` holds one aggregated full-resolution map per frame, all
    /// `width * height` samples. `guide` is encoded luma `Y'`. `calibrations`
    /// is indexed by frame; a missing entry disables the coverage gate for that
    /// frame. `saturation`, when present, holds one mask per frame with `true`
    /// where any channel is at or above [`SATURATION_THRESHOLD`].
    ///
    /// # Panics
    ///
    /// Panics if dimensions are zero, the guide or a map has the wrong length,
    /// or when `energies` is empty.
    #[must_use]
    pub fn compute(
        energies: &[Vec<f32>],
        guide: &[f32],
        width: u32,
        height: u32,
        calibrations: &[FrameCalibration],
        saturation: Option<&[Vec<bool>]>,
        options: ConfidenceOptions,
    ) -> Self {
        assert!(width > 0 && height > 0, "image dimensions must be non-zero");
        assert!(!energies.is_empty(), "at least one frame is required");
        let count = (width as usize) * (height as usize);
        assert_eq!(guide.len(), count, "guide buffer length mismatch");
        for energy in energies {
            assert_eq!(energy.len(), count, "energy buffer length mismatch");
        }
        if let Some(masks) = saturation {
            assert_eq!(
                masks.len(),
                energies.len(),
                "saturation frame count mismatch"
            );
            for mask in masks {
                assert_eq!(mask.len(), count, "saturation buffer length mismatch");
            }
        }

        let epsilon = sanitize_epsilon(options.margin_epsilon);
        let mut best_frame = vec![0u32; count];
        let mut best_energy = vec![0.0f32; count];
        let mut margin = vec![0.0f32; count];

        for index in 0..count {
            let (frame, best, second) = best_and_second(energies, index);
            best_frame[index] = frame;
            best_energy[index] = best;
            margin[index] = ((best - second) / (best + epsilon)).clamp(0.0, 1.0);
        }

        let mut confidence = guided_filter(
            guide,
            &margin,
            width,
            height,
            options.guide_radius,
            options.guide_epsilon,
        );

        let tau_cov = options.coverage_tau.max(0.0);
        for (index, value) in confidence.iter_mut().enumerate() {
            let frame = best_frame[index] as usize;
            let floor = calibrations
                .get(frame)
                .map_or(0.0, |calibration| calibration.coverage_floor(tau_cov));
            let saturated = saturation
                .and_then(|masks| masks.get(frame))
                .is_some_and(|mask| mask[index]);
            if best_energy[index] < floor || saturated {
                *value = 0.0;
            } else {
                *value = value.clamp(0.0, 1.0);
            }
        }

        Self {
            width,
            height,
            best_frame,
            best_energy,
            margin,
            confidence,
        }
    }

    /// Number of pixels in the map.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.width * self.height) as usize
    }

    /// Whether the map has no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Standard O(1) guided filter with box filters.
///
/// `guide` is `Y'`, `input` is the confidence to smooth, `radius` is the box
/// radius, and `eps` regularizes the local linear fit. The return value is not
/// clamped; callers that need a probability should clamp it.
///
/// # Panics
///
/// Panics if dimensions are zero or either buffer has the wrong length.
#[must_use]
pub fn guided_filter(
    guide: &[f32],
    input: &[f32],
    width: u32,
    height: u32,
    radius: u32,
    eps: f32,
) -> Vec<f32> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(guide.len(), count, "guide buffer length mismatch");
    assert_eq!(input.len(), count, "input buffer length mismatch");

    let eps = if eps.is_finite() {
        eps.max(f32::EPSILON)
    } else {
        DEFAULT_GUIDE_EPSILON
    };

    let mean_guide = box_filter(guide, width, height, radius);
    let mean_input = box_filter(input, width, height, radius);
    let guide_squared: Vec<f32> = guide.iter().map(|value| value * value).collect();
    let guide_input: Vec<f32> = guide
        .iter()
        .zip(input)
        .map(|(guide, input)| guide * input)
        .collect();
    let corr_guide = box_filter(&guide_squared, width, height, radius);
    let corr_input = box_filter(&guide_input, width, height, radius);

    let mut scale = vec![0.0f32; count];
    let mut offset = vec![0.0f32; count];
    for index in 0..count {
        let variance = (corr_guide[index] - mean_guide[index] * mean_guide[index]).max(0.0);
        let covariance = corr_input[index] - mean_guide[index] * mean_input[index];
        let a = covariance / (variance + eps);
        scale[index] = a;
        offset[index] = mean_input[index] - a * mean_guide[index];
    }

    let mean_scale = box_filter(&scale, width, height, radius);
    let mean_offset = box_filter(&offset, width, height, radius);
    mean_scale
        .iter()
        .zip(&mean_offset)
        .zip(guide)
        .map(|((&a, &b), &guide)| a * guide + b)
        .collect()
}

/// Build a saturation mask from interleaved encoded RGB.
///
/// Returns `true` at pixels where any channel is at or above
/// [`SATURATION_THRESHOLD`].
///
/// # Panics
///
/// Panics if `channels` is zero or `rgb` is not `width * height * channels`.
#[must_use]
pub fn saturation_mask(rgb: &[f32], width: u32, height: u32, channels: usize) -> Vec<bool> {
    saturation_mask_with_threshold(rgb, width, height, channels, SATURATION_THRESHOLD)
}

/// Build a saturation mask with an explicit threshold.
///
/// # Panics
///
/// Panics if `channels` is zero or `rgb` is not `width * height * channels`.
#[must_use]
pub fn saturation_mask_with_threshold(
    rgb: &[f32],
    width: u32,
    height: u32,
    channels: usize,
    threshold: f32,
) -> Vec<bool> {
    assert!(channels > 0, "at least one channel is required");
    let count = (width as usize) * (height as usize);
    assert_eq!(rgb.len(), count * channels, "RGB buffer length mismatch");

    rgb.chunks_exact(channels)
        .map(|pixel| pixel.iter().any(|&value| value >= threshold))
        .collect()
}

/// Find the winning frame and the runner-up energy at `index`.
fn best_and_second(energies: &[Vec<f32>], index: usize) -> (u32, f32, f32) {
    let mut best_frame = 0u32;
    let mut best = sanitize_energy(energies[0][index]);
    let mut second = 0.0f32;

    for (frame, energy) in energies.iter().enumerate().skip(1) {
        let value = sanitize_energy(energy[index]);
        if value > best {
            second = best;
            best = value;
            best_frame = frame as u32;
        } else if value > second {
            second = value;
        }
    }

    (best_frame, best, second)
}

/// Treat non-finite or negative energies as zero.
fn sanitize_energy(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

/// Keep the margin denominator strictly positive and finite.
fn sanitize_epsilon(value: f32) -> f32 {
    if value.is_finite() {
        value.max(f32::EPSILON)
    } else {
        MARGIN_EPSILON
    }
}
