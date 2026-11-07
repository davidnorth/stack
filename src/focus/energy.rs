//! Box energy and normalization.
//!
//! Implements `docs/focus-measure.md` §4 and the aggregation weights from §6.
//! At each pyramid level the energy is the local mean of `Lap_l^2`, computed
//! with a summed-area table, then reduced by the per-level noise floor
//! `tau_l`. An optional contrast normalization divides by the local mean of
//! `Y'^2`. The radius schedule follows `r_l = max(2, round(r0 / 2^l))`.
//!
//! [`EnergyMaps::aggregate`] combines band levels into a single full-resolution
//! per-frame map for DMap. The coarsest residual level (`Lap_L = G_L`) is not
//! aggregated: it carries brightness rather than focus detail, and §6 weights
//! only the band levels `l = 0..L-1`.

use rayon::prelude::*;

use super::noise::{NoiseModel, subtract_noise_floor};
use super::pyramid::Pyramid;

/// Auto estimation-radius scale from `docs/focus-measure.md` §4.
pub const AUTO_RADIUS_SCALE: f32 = 0.0027;
/// Lower bound for the auto estimation radius, in pixels.
pub const AUTO_RADIUS_MIN: u32 = 12;
/// Upper bound for the auto estimation radius, in pixels.
pub const AUTO_RADIUS_MAX: u32 = 32;
/// Minimum box radius, from `r_l = max(2, ...)` in §4.
pub const MIN_RADIUS: u32 = 2;
/// Additive constant in the optional contrast normalization (§4).
pub const CONTRAST_EPSILON: f32 = 1.0e-4;

/// Cross-level aggregation weights `w_l` from `docs/focus-measure.md` §6.
pub const AGGREGATION_WEIGHTS: [f32; 8] = [1.0, 0.9, 0.8, 0.6, 0.5, 0.4, 0.3, 0.2];

/// Options for [`EnergyMaps::compute`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EnergyOptions {
    /// Estimation radius `r0`. `None` selects the auto radius from §4.
    pub radius: Option<u32>,
    /// Divide `E'` by the local mean of `Y'^2` (§4, off by default).
    pub contrast_normalize: bool,
}

/// Energy map for one pyramid level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnergyMap {
    /// Level width in pixels.
    pub width: u32,
    /// Level height in pixels.
    pub height: u32,
    /// Box radius used for this level.
    pub radius: u32,
    /// Noise-subtracted energy, row-major.
    pub energy: Vec<f32>,
}

impl EnergyMap {
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

/// Per-frame energy maps, finest level first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EnergyMaps {
    levels: Vec<EnergyMap>,
    radius0: u32,
}

impl EnergyMaps {
    /// Compute one energy map per pyramid level.
    ///
    /// `noise` normally comes from [`super::noise::estimate`] for the same
    /// pyramid. Levels without a noise estimate are not floor-subtracted.
    #[must_use]
    pub fn compute(pyramid: &Pyramid, noise: &NoiseModel, options: EnergyOptions) -> Self {
        let radius0 = options
            .radius
            .unwrap_or_else(|| auto_radius(pyramid.width(), pyramid.height()));

        let levels = pyramid
            .levels()
            .iter()
            .enumerate()
            .map(|(index, level)| {
                let radius = level_radius(radius0, index);
                let tau = noise.level(index).map_or(0.0, |estimate| estimate.tau);
                energy_map(level, radius, tau, options.contrast_normalize)
            })
            .collect();

        Self { levels, radius0 }
    }

    /// Energy maps from the finest level to the coarsest residual.
    #[must_use]
    pub fn levels(&self) -> &[EnergyMap] {
        &self.levels
    }

    /// Energy map for `index`, if present.
    #[must_use]
    pub fn level(&self, index: usize) -> Option<&EnergyMap> {
        self.levels.get(index)
    }

    /// Estimation radius `r0` used for this frame.
    #[must_use]
    pub fn radius0(&self) -> u32 {
        self.radius0
    }

    /// Aggregate band levels into a full-resolution per-frame energy map.
    ///
    /// Uses the §6 weights and excludes the coarsest residual level. Coarser
    /// maps are bilinearly upsampled with the same `x / 2` mapping as the
    /// pyramid.
    #[must_use]
    pub fn aggregate(&self) -> Vec<f32> {
        aggregate_levels(&self.levels)
    }
}

/// Auto estimation radius `clamp(0.0027 * min(W, H), 12, 32)` from §4.
#[must_use]
pub fn auto_radius(width: u32, height: u32) -> u32 {
    let min_side = width.min(height) as f32;
    (AUTO_RADIUS_SCALE * min_side)
        .round()
        .clamp(AUTO_RADIUS_MIN as f32, AUTO_RADIUS_MAX as f32) as u32
}

/// Radius schedule `r_l = max(2, round(r0 / 2^l))` from §4.
#[must_use]
pub fn level_radius(radius0: u32, level: usize) -> u32 {
    let shift = level.min(31) as u32;
    let divisor = 1u32 << shift;
    ((radius0 + divisor / 2) / divisor).max(MIN_RADIUS)
}

/// Weight `w_l` from §6, or `None` past the documented level range.
#[must_use]
pub fn aggregation_weight(level: usize) -> Option<f32> {
    AGGREGATION_WEIGHTS.get(level).copied()
}

/// Box filter with a clamped, truncated window and a summed-area table.
///
/// `input` is a row-major `f32` image of `width * height` samples. Border
/// windows are clipped to the image and divided by their actual area.
///
/// # Panics
///
/// Panics if `input` is not `width * height` samples long, or if either
/// dimension is zero.
#[must_use]
pub fn box_filter(input: &[f32], width: u32, height: u32, radius: u32) -> Vec<f32> {
    let (w, h) = checked_dimensions(input, width, height);
    let integral = summed_area(input, w, h, false);
    box_mean(&integral, w, h, radius as usize)
}

/// Aggregate band levels into a full-resolution map.
///
/// The last level is treated as the coarsest residual and excluded. If only
/// one level is supplied, it is copied unchanged.
#[must_use]
pub fn aggregate_levels(levels: &[EnergyMap]) -> Vec<f32> {
    let Some(base) = levels.first() else {
        return Vec::new();
    };
    if base.is_empty() {
        return Vec::new();
    }
    let mut out = vec![0.0f32; base.len()];

    let band_count = if levels.len() > 1 {
        levels.len() - 1
    } else {
        1
    };

    // Rows are independent, so the bilinear upsample of each level runs in
    // parallel over the destination.
    for (index, level) in levels.iter().take(band_count).enumerate() {
        let Some(weight) = aggregation_weight(index) else {
            break;
        };
        accumulate_upsampled(
            &level.energy,
            level.width,
            level.height,
            &mut out,
            base.width,
            base.height,
            weight,
        );
    }

    out
}

/// Compute the noise-subtracted energy map for one pyramid level.
///
/// `tau` is the noise floor for this level; pass zero to skip subtraction.
/// When `contrast_normalize` is set, `E'` is divided by
/// `box_filter(Y'^2, radius) + 1e-4` from §4.
///
/// # Panics
///
/// Panics if the level buffers do not match `width * height`.
#[must_use]
pub fn energy_map(
    level: &super::pyramid::PyramidLevel,
    radius: u32,
    tau: f32,
    contrast_normalize: bool,
) -> EnergyMap {
    let (w, h) = checked_dimensions(&level.laplacian, level.width, level.height);
    assert_eq!(
        level.gaussian.len(),
        w * h,
        "Gaussian buffer length mismatch"
    );

    let integral = summed_area(&level.laplacian, w, h, true);
    let mut energy = box_mean(&integral, w, h, radius as usize);
    for value in &mut energy {
        *value = value.max(0.0);
    }
    subtract_noise_floor(&mut energy, tau);

    if contrast_normalize {
        let contrast = summed_area(&level.gaussian, w, h, true);
        let contrast = box_mean(&contrast, w, h, radius as usize);
        for (value, denominator) in energy.iter_mut().zip(contrast) {
            *value /= denominator.max(0.0) + CONTRAST_EPSILON;
        }
    }

    EnergyMap {
        width: level.width,
        height: level.height,
        radius,
        energy,
    }
}

/// Build a summed-area table in `f64`, optionally summing squared samples.
fn summed_area(input: &[f32], width: usize, height: usize, square: bool) -> Vec<f64> {
    let stride = width + 1;
    let mut integral = vec![0.0f64; stride * (height + 1)];

    for y in 0..height {
        let mut row_sum = 0.0f64;
        let previous = y * stride;
        let current = (y + 1) * stride;
        for (x, &sample) in input[y * width..(y + 1) * width].iter().enumerate() {
            let sample = f64::from(sample);
            row_sum += if square { sample * sample } else { sample };
            integral[current + x + 1] = integral[previous + x + 1] + row_sum;
        }
    }

    integral
}

/// Evaluate a box mean from a summed-area table.
fn box_mean(integral: &[f64], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let stride = width + 1;
    let mut out = vec![0.0f32; width * height];

    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(height);
        let top = y0 * stride;
        let bottom = y1 * stride;
        let area_y = (y1 - y0) as f64;

        for (x, value) in row.iter_mut().enumerate() {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(width);
            let sum = integral[bottom + x1] - integral[bottom + x0] - integral[top + x1]
                + integral[top + x0];
            let area = area_y * (x1 - x0) as f64;
            *value = (sum / area) as f32;
        }
    });

    out
}

/// Add a weighted, bilinearly upsampled map to `out`.
#[allow(clippy::too_many_arguments)]
fn accumulate_upsampled(
    src: &[f32],
    src_width: u32,
    src_height: u32,
    out: &mut [f32],
    out_width: u32,
    out_height: u32,
    weight: f32,
) {
    let sw = src_width as usize;
    let sh = src_height as usize;
    let ow = out_width as usize;
    let oh = out_height as usize;

    if weight == 0.0 || sw == 0 || sh == 0 {
        return;
    }

    if sw == ow && sh == oh {
        for (value, &sample) in out.iter_mut().zip(src) {
            *value += weight * sample;
        }
        return;
    }

    // Each level is a power-of-two reduction of the finest map; use its actual
    // source-to-destination ratio rather than assuming a single halving.
    let scale_x = sw as f32 / ow as f32;
    let scale_y = sh as f32 / oh as f32;

    out.par_chunks_mut(ow).enumerate().for_each(|(dy, row)| {
        let fy = dy as f32 * scale_y;
        let y0 = (fy.floor() as usize).min(sh - 1);
        let ty = fy - y0 as f32;
        let y1 = (y0 + 1).min(sh - 1);

        for (dx, value) in row.iter_mut().enumerate() {
            let fx = dx as f32 * scale_x;
            let x0 = (fx.floor() as usize).min(sw - 1);
            let tx = fx - x0 as f32;
            let x1 = (x0 + 1).min(sw - 1);

            let top = src[y0 * sw + x0] * (1.0 - tx) + src[y0 * sw + x1] * tx;
            let bottom = src[y1 * sw + x0] * (1.0 - tx) + src[y1 * sw + x1] * tx;
            *value += weight * (top * (1.0 - ty) + bottom * ty);
        }
    });
}

/// Validate non-zero dimensions and return them as `usize`.
fn checked_dimensions(input: &[f32], width: u32, height: u32) -> (usize, usize) {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let w = width as usize;
    let h = height as usize;
    assert_eq!(input.len(), w * h, "buffer length mismatch");
    (w, h)
}
