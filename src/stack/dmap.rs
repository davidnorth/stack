//! Cost volume, depth solve, and interpolation.

use crate::DepthField;
use crate::focus::coverage::COVERAGE_TAU;
use crate::focus::noise::{DEFAULT_NOISE_K, SIGMA_EPSILON};

/// Additive epsilon for energy normalizations.
pub const COST_EPSILON: f32 = 1.0e-6;

/// Relative best-vs-second margin at or below which the argmax is ambiguous.
///
/// The automatic contrast threshold in `docs/stacking.md` §3.2 refines this
/// gate; §3.1 only needs to identify clearly tied pixels.
pub const FLAT_MARGIN: f32 = 1.0e-3;

/// The threshold is capped at this fraction of the 99th percentile of `Emax`.
pub const THRESHOLD_CAP_FRACTION: f32 = 0.10;

/// Upper edge of the confidence ramp as a multiple of the threshold (`1.5*t`).
pub const CONFIDENCE_RAMP_END: f32 = 1.5;

/// Number of bins in the Otsu histogram over `log(Emax + eps)`.
pub const OTSU_BINS: usize = 256;

/// Per-pixel, per-frame focus evidence.
///
/// `energies` is frame-major: `energies[frame * count + pixel]`. Values are
/// aggregated energy maps from [`crate::focus::energy::EnergyMaps::aggregate`],
/// so a larger value means stronger focus evidence. The equivalent cost is
/// `Emax - E`, available through [`Self::cost`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostVolume {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Number of frames.
    pub frames: usize,
    /// Aggregated energy per frame and pixel, frame-major.
    pub energies: Vec<f32>,
}

impl CostVolume {
    /// Build a volume from one aggregated energy map per frame.
    ///
    /// # Panics
    ///
    /// Panics if dimensions are zero, no frames are supplied, or a map has the
    /// wrong length.
    #[must_use]
    pub fn new(aggregates: &[Vec<f32>], width: u32, height: u32) -> Self {
        assert!(width > 0 && height > 0, "image dimensions must be non-zero");
        assert!(!aggregates.is_empty(), "at least one frame is required");
        let count = (width as usize) * (height as usize);
        for energy in aggregates {
            assert_eq!(energy.len(), count, "energy buffer length mismatch");
        }

        let mut energies = Vec::with_capacity(count * aggregates.len());
        for energy in aggregates {
            energies.extend(energy.iter().map(|&value| sanitize_energy(value)));
        }

        Self {
            width,
            height,
            frames: aggregates.len(),
            energies,
        }
    }

    /// Number of pixels in the volume.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.width as usize) * (self.height as usize)
    }

    /// Whether the volume has no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Aggregated energy map for `frame`.
    #[must_use]
    pub fn frame(&self, frame: usize) -> Option<&[f32]> {
        let count = self.len();
        let start = frame.checked_mul(count)?;
        let end = start.checked_add(count)?;
        self.energies.get(start..end)
    }

    /// Energy at `frame` and `pixel`.
    ///
    /// # Panics
    ///
    /// Panics if either index is out of range.
    #[must_use]
    pub fn energy(&self, frame: usize, pixel: usize) -> f32 {
        self.energies[frame * self.len() + pixel]
    }

    /// Cost of assigning `pixel` to `frame`: `Emax - E`.
    ///
    /// The winning frame has zero cost; all costs are non-negative.
    ///
    /// # Panics
    ///
    /// Panics if either index is out of range.
    #[must_use]
    pub fn cost(&self, frame: usize, pixel: usize) -> f32 {
        let (_, best) = self.argmax(pixel);
        best - self.energy(frame, pixel)
    }

    /// Winning frame index and its energy at `pixel` from `d0 = argmax_i E_i`.
    ///
    /// Ties resolve to the lowest frame index.
    ///
    /// # Panics
    ///
    /// Panics if `pixel` is out of range.
    #[must_use]
    pub fn argmax(&self, pixel: usize) -> (u32, f32) {
        let (frame, best, _) = self.best_and_second(pixel);
        (frame, best)
    }

    fn best_and_second(&self, pixel: usize) -> (u32, f32, f32) {
        let count = self.len();
        assert!(pixel < count, "pixel index out of range");

        let mut frame = 0u32;
        let mut best = f32::NEG_INFINITY;
        let mut second = f32::NEG_INFINITY;
        for index in 0..self.frames {
            let value = self.energies[index * count + pixel];
            if value > best {
                second = best;
                best = value;
                frame = index as u32;
            } else if value > second {
                second = value;
            }
        }

        // A single-frame volume has no runner-up.
        (frame, best, second.max(0.0))
    }
}

/// Initial `d0` field and flat-region flags from [`CostVolume::argmax`].
#[derive(Debug, Clone, PartialEq)]
pub struct InitialDepth {
    /// Continuous frame index at each pixel plus per-pixel confidence.
    pub field: DepthField,
    /// Winning energy `Emax` at each pixel.
    pub best_energy: Vec<f32>,
    /// Relative best-vs-second margin before confidence gating, in `[0, 1]`.
    pub margin: Vec<f32>,
    /// Pixels with no trustworthy depth evidence.
    pub flat: Vec<bool>,
}

/// Options for the automatic contrast threshold from `docs/stacking.md` §3.2.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ThresholdOptions {
    /// Aggregated noise standard deviation `sigma` from the focus module.
    ///
    /// The threshold uses the floor `k * sigma^2` with the focus module's
    /// default `k`.
    pub noise_sigma: f32,
    /// `--keep-detail PERCENT` override, in percent.
    ///
    /// When present, the threshold is the `(1 - PERCENT)` quantile of `Emax`
    /// instead of `max(k * sigma^2, otsu)`.
    pub keep_detail: Option<f32>,
}

impl Default for ThresholdOptions {
    fn default() -> Self {
        Self {
            noise_sigma: SIGMA_EPSILON,
            keep_detail: None,
        }
    }
}

/// Chosen DMap contrast threshold and the statistics the report needs.
///
/// The value is `t` from `docs/stacking.md` §3.2. Apply it to an
/// [`InitialDepth`] with [`ContrastThreshold::apply_confidence`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ContrastThreshold {
    /// Final threshold `t` applied to `Emax`.
    pub threshold: f32,
    /// Otsu split of `log(Emax + eps)` before the noise floor and cap.
    pub otsu: f32,
    /// Noise floor `k * sigma^2` supplied by the focus module.
    pub noise_floor: f32,
    /// Cap `0.1 * p99(Emax)`.
    pub cap: f32,
    /// `--keep-detail` percentage when the override was used.
    pub keep_detail: Option<f32>,
    /// Pixels with positive ramped confidence, in percent.
    pub confident_pct: f32,
    /// Pixels covered by at least one frame, in percent.
    pub coverage_pct: f32,
}

impl ContrastThreshold {
    /// Compute the automatic threshold for `volume`.
    #[must_use]
    pub fn compute(volume: &CostVolume, options: ThresholdOptions) -> Self {
        let initial = argmax_depth(volume, None);
        Self::from_initial(&initial, options)
    }

    /// Compute the threshold from an existing argmax result.
    ///
    /// # Panics
    ///
    /// Panics if the best-energy and margin buffers disagree in length.
    #[must_use]
    pub fn from_initial(initial: &InitialDepth, options: ThresholdOptions) -> Self {
        let count = initial.best_energy.len();
        assert_eq!(initial.margin.len(), count, "margin buffer length mismatch");

        let sigma = sanitize_sigma(options.noise_sigma);
        let noise_floor = DEFAULT_NOISE_K * sigma * sigma;
        let cap = THRESHOLD_CAP_FRACTION * quantile(&initial.best_energy, 0.99);
        let otsu = otsu_threshold(&initial.best_energy);
        let keep_detail = sanitize_keep_detail(options.keep_detail);

        let candidate = match keep_detail {
            Some(percent) => quantile(&initial.best_energy, 1.0 - percent / 100.0),
            None => otsu.max(noise_floor),
        };
        let threshold = sanitize_non_negative(candidate.min(cap));

        let coverage_floor = COVERAGE_TAU * sigma * sigma;
        let mut confident = 0usize;
        let mut covered = 0usize;
        for index in 0..count {
            let ramp = ramp(threshold, initial.best_energy[index]);
            if ramp * initial.margin[index] > 0.0 {
                confident += 1;
            }
            if initial.best_energy[index] >= coverage_floor {
                covered += 1;
            }
        }

        Self {
            threshold,
            otsu,
            noise_floor,
            cap,
            keep_detail,
            confident_pct: percentage(confident, count),
            coverage_pct: percentage(covered, count),
        }
    }

    /// Replace `initial`'s confidence with the §3.2 ramp and update flat flags.
    ///
    /// The ramp is `smoothstep(t, 1.5*t, Emax) * margin_confidence`. Pixels
    /// below the threshold get `c = 0` so the depth solve can fill them from
    /// trustworthy neighbours instead of assigning noise.
    ///
    /// # Panics
    ///
    /// Panics if the best-energy, margin, and confidence buffers disagree in
    /// length.
    pub fn apply_confidence(&self, initial: &mut InitialDepth) {
        let count = initial.best_energy.len();
        assert_eq!(initial.margin.len(), count, "margin buffer length mismatch");
        assert_eq!(
            initial.field.confidence.len(),
            count,
            "confidence buffer length mismatch"
        );

        for index in 0..count {
            let confidence =
                ramp(self.threshold, initial.best_energy[index]) * initial.margin[index];
            initial.field.confidence[index] = confidence;
            initial.flat[index] |= confidence <= 0.0;
        }
    }
}

/// Compute `d0 = argmax_i E_i`, apply the automatic contrast threshold, and
/// return the depth together with the threshold report.
///
/// This is the `docs/stacking.md` §3.2 entry point for the DMap solve.
#[must_use]
pub fn threshold_depth(
    volume: &CostVolume,
    options: ThresholdOptions,
) -> (InitialDepth, ContrastThreshold) {
    let mut initial = argmax_depth(volume, None);
    let threshold = ContrastThreshold::from_initial(&initial, options);
    threshold.apply_confidence(&mut initial);
    (initial, threshold)
}

/// Compute `d0 = argmax_i E_i` and flag flat regions.
///
/// `confidence`, when present, is the per-pixel confidence from
/// [`crate::focus::confidence::ConfidenceMap`]. It is carried into
/// [`InitialDepth::field`] and gates [`InitialDepth::flat`]; without it the
/// best-vs-second margin is used as confidence. A pixel is flat when its
/// winning energy is negligible, the top two energies are tied within
/// [`FLAT_MARGIN`], or its confidence is zero.
///
/// # Panics
///
/// Panics if `confidence` has the wrong length.
#[must_use]
pub fn argmax_depth(volume: &CostVolume, confidence: Option<&[f32]>) -> InitialDepth {
    let count = volume.len();
    if let Some(values) = confidence {
        assert_eq!(values.len(), count, "confidence buffer length mismatch");
    }

    let mut d = vec![0.0f32; count];
    let mut confidence_out = vec![0.0f32; count];
    let mut best_energy = vec![0.0f32; count];
    let mut margin_out = vec![0.0f32; count];
    let mut flat = vec![false; count];

    for pixel in 0..count {
        let (frame, best, second) = volume.best_and_second(pixel);
        let margin = if best <= COST_EPSILON {
            0.0
        } else {
            ((best - second) / (best + COST_EPSILON)).clamp(0.0, 1.0)
        };
        let value = confidence.map_or(margin, |values| sanitize_confidence(values[pixel]));

        d[pixel] = frame as f32;
        confidence_out[pixel] = value;
        best_energy[pixel] = best;
        margin_out[pixel] = margin;
        flat[pixel] = best <= COST_EPSILON || margin <= FLAT_MARGIN || value <= 0.0;
    }

    InitialDepth {
        field: DepthField {
            d,
            confidence: confidence_out,
        },
        best_energy,
        margin: margin_out,
        flat,
    }
}

/// Default data weight `alpha` in the §3.3 energy.
pub const DEFAULT_ALPHA: f32 = 1.0;
/// Default smoothness weight `lambda` in the §3.3 energy.
pub const DEFAULT_LAMBDA: f32 = 1.0;
/// Guide-luma scale `sigma_g` for the edge-aware neighbour weights.
pub const GUIDE_SIGMA: f32 = 0.08;
/// Huber threshold `delta` in frame units.
pub const HUBER_DELTA: f32 = 1.0;
/// Jacobi iteration cap per pyramid level.
pub const JACOBI_ITERATIONS: usize = 30;
/// Convergence threshold on the maximum per-iteration update, in frames.
pub const JACOBI_TOLERANCE: f32 = 1.0e-3;
/// Default post-solve Gaussian radius, in pixels.
pub const DEFAULT_SMOOTHING_RADIUS: f32 = 1.0;

/// Number of coarse-to-fine levels: 1/8, 1/4, 1/1.
const SOLVE_LEVELS: usize = 3;

/// Resolution of the edge-weight lookup table.
const WEIGHT_LUT_SIZE: usize = 1024;
/// Largest exponent stored in the edge-weight lookup table.
const WEIGHT_LUT_MAX: f32 = 16.0;

/// Options for the edge-aware depth solve from `docs/stacking.md` §3.3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SolveOptions {
    /// Data-term weight `alpha`.
    pub alpha: f32,
    /// Smoothness weight `lambda`. Larger values give smoother depth.
    pub lambda: f32,
    /// Guide-luma scale `sigma_g` in the edge-aware weight.
    pub guide_sigma: f32,
    /// Huber threshold `delta` in frame units.
    pub huber_delta: f32,
    /// Maximum Jacobi iterations per level.
    pub iterations: usize,
    /// Stop a level when the largest update falls below this, in frames.
    pub tolerance: f32,
    /// Post-solve Gaussian radius in pixels. Zero disables the post-smooth.
    ///
    /// The Gaussian standard deviation is half the radius, matching Zerene's
    /// "half the estimation radius" relationship. The default is half a small
    /// estimation radius.
    pub smoothing_radius: f32,
}

impl Default for SolveOptions {
    fn default() -> Self {
        Self {
            alpha: DEFAULT_ALPHA,
            lambda: DEFAULT_LAMBDA,
            guide_sigma: GUIDE_SIGMA,
            huber_delta: HUBER_DELTA,
            iterations: JACOBI_ITERATIONS,
            tolerance: JACOBI_TOLERANCE,
            smoothing_radius: DEFAULT_SMOOTHING_RADIUS,
        }
    }
}

/// Result of [`solve_depth`], including the convergence evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct SolvedDepth {
    /// Solved continuous depth and the original confidence.
    pub field: DepthField,
    /// Total Jacobi iterations across all levels.
    pub iterations: usize,
    /// Largest update seen in the final level, in frames.
    pub max_update: f32,
    /// Whether every level met [`SolveOptions::tolerance`].
    pub converged: bool,
}

#[derive(Debug, Clone)]
struct SolveLevel {
    width: usize,
    height: usize,
    depth: Vec<f32>,
    confidence: Vec<f32>,
    guide: Vec<f32>,
}

#[derive(Debug, Clone, Copy)]
struct LevelSolve {
    iterations: usize,
    max_update: f32,
    converged: bool,
}

/// Solve the edge-aware depth energy from `docs/stacking.md` §3.3.
///
/// `initial` supplies `d0` and confidence; `guide` is the same-size luma image
/// used for the edge-aware neighbour weights. The solve runs coarse-to-fine at
/// 1/8, 1/4, and 1/1, then applies a Gaussian post-smooth and re-anchors every
/// pixel with positive confidence to its initial `d0`. Flat, unconfident
/// regions therefore become smooth while textured edges stay sharp.
///
/// # Panics
///
/// Panics if dimensions are zero or the initial/guide buffers have the wrong
/// length.
#[must_use]
pub fn solve_depth(
    initial: &InitialDepth,
    guide: &[f32],
    width: u32,
    height: u32,
    options: SolveOptions,
) -> SolvedDepth {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(initial.field.d.len(), count, "depth buffer length mismatch");
    assert_eq!(
        initial.field.confidence.len(),
        count,
        "confidence buffer length mismatch"
    );
    assert_eq!(guide.len(), count, "guide buffer length mismatch");

    let options = sanitize_solve_options(options);
    let levels = build_solve_levels(initial, guide, width as usize, height as usize);
    let weights = build_weight_table();

    let mut current: Option<Vec<f32>> = None;
    let mut current_width = 0usize;
    let mut current_height = 0usize;
    let mut iterations = 0usize;
    let mut max_update = 0.0f32;
    let mut converged = true;

    for level in levels.iter().rev() {
        let depth = match &current {
            Some(coarse) => upsample_bilinear(
                coarse,
                current_width,
                current_height,
                level.width,
                level.height,
            ),
            None => level.depth.clone(),
        };
        let (solved, report) = solve_level(level, depth, options, &weights);
        iterations += report.iterations;
        max_update = report.max_update;
        converged &= report.converged;
        current = Some(solved);
        current_width = level.width;
        current_height = level.height;
    }

    let mut depth = current.expect("at least one solve level");
    let has_flat = initial
        .field
        .confidence
        .iter()
        .any(|&confidence| sanitize_confidence(confidence) <= 0.0);
    if options.smoothing_radius > 0.0 && has_flat {
        depth = gaussian_smooth(
            &depth,
            width as usize,
            height as usize,
            options.smoothing_radius,
        );
    }

    // Re-anchor trusted pixels once so the post-smooth cannot soften an edge.
    for (value, (&d0, &confidence)) in depth
        .iter_mut()
        .zip(initial.field.d.iter().zip(&initial.field.confidence))
    {
        if sanitize_confidence(confidence) > 0.0 {
            *value = d0;
        }
    }

    SolvedDepth {
        field: DepthField {
            d: depth,
            confidence: initial.field.confidence.clone(),
        },
        iterations,
        max_update,
        converged,
    }
}

fn build_solve_levels(
    initial: &InitialDepth,
    guide: &[f32],
    width: usize,
    height: usize,
) -> Vec<SolveLevel> {
    let mut levels = Vec::with_capacity(SOLVE_LEVELS);
    levels.push(SolveLevel {
        width,
        height,
        depth: initial.field.d.clone(),
        confidence: initial.field.confidence.clone(),
        guide: guide.to_vec(),
    });

    while levels.len() < SOLVE_LEVELS {
        let last = levels.last().expect("at least one level");
        if last.width == 1 && last.height == 1 {
            break;
        }
        let next = downsample_level(last);
        levels.push(next);
    }

    levels
}

fn downsample_level(level: &SolveLevel) -> SolveLevel {
    let width = level.width.div_ceil(2).max(1);
    let height = level.height.div_ceil(2).max(1);
    let mut depth = vec![0.0f32; width * height];
    let mut confidence = vec![0.0f32; width * height];
    let mut guide = vec![0.0f32; width * height];

    for y in 0..height {
        for x in 0..width {
            let mut confidence_sum = 0.0f32;
            let mut weighted_depth = 0.0f32;
            let mut guide_sum = 0.0f32;
            let mut samples = 0u32;

            for dy in 0..2 {
                let sy = y * 2 + dy;
                if sy >= level.height {
                    break;
                }
                for dx in 0..2 {
                    let sx = x * 2 + dx;
                    if sx >= level.width {
                        continue;
                    }
                    let index = sy * level.width + sx;
                    let c = sanitize_confidence(level.confidence[index]);
                    confidence_sum += c;
                    weighted_depth += c * sanitize_depth(level.depth[index]);
                    guide_sum += sanitize_guide(level.guide[index]);
                    samples += 1;
                }
            }

            let index = y * width + x;
            let samples = samples.max(1) as f32;
            confidence[index] = confidence_sum / samples;
            depth[index] = if confidence_sum > COST_EPSILON {
                weighted_depth / confidence_sum
            } else {
                0.0
            };
            guide[index] = guide_sum / samples;
        }
    }

    SolveLevel {
        width,
        height,
        depth,
        confidence,
        guide,
    }
}

fn solve_level(
    level: &SolveLevel,
    mut depth: Vec<f32>,
    options: SolveOptions,
    weight_table: &[f32],
) -> (Vec<f32>, LevelSolve) {
    let width = level.width;
    let height = level.height;
    let count = width * height;
    let alpha = options.alpha;
    let lambda = options.lambda;
    let delta = options.huber_delta;
    let inv_two_sigma_sq = 1.0 / (2.0 * options.guide_sigma * options.guide_sigma);

    // Edge weights are fixed for the level: horizontal (`right`) and vertical
    // (`down`) neighbour weights, shared by the opposite direction.
    let mut right = vec![0.0f32; count];
    let mut down = vec![0.0f32; count];
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if x + 1 < width {
                let diff = level.guide[index + 1] - level.guide[index];
                right[index] = edge_weight(diff, inv_two_sigma_sq, weight_table);
            }
            if y + 1 < height {
                let diff = level.guide[index + width] - level.guide[index];
                down[index] = edge_weight(diff, inv_two_sigma_sq, weight_table);
            }
        }
    }

    // The linear part of the Jacobi update is constant across iterations:
    // `data = alpha*c*d0`, `weight = alpha*c`, and `1/(weight + lambda*W)`.
    let mut data = vec![0.0f32; count];
    let mut weight = vec![0.0f32; count];
    let mut inv_denominator = vec![0.0f32; count];
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let c = sanitize_confidence(level.confidence[index]);
            let left = if x > 0 { right[index - 1] } else { 0.0 };
            let up = if y > 0 { down[index - width] } else { 0.0 };
            let edge_sum = right[index] + down[index] + left + up;
            let data_weight = alpha * c;
            let denominator = data_weight + lambda * edge_sum;

            data[index] = data_weight * sanitize_depth(level.depth[index]);
            weight[index] = data_weight;
            inv_denominator[index] = if denominator > COST_EPSILON {
                1.0 / denominator
            } else {
                0.0
            };
        }
    }

    let mut next = vec![0.0f32; count];
    let mut max_update = 0.0f32;
    let mut converged = false;
    let mut iterations = 0usize;

    for _ in 0..options.iterations {
        let mut iteration_update = 0.0f32;
        for y in 0..height {
            let row = y * width;
            let has_up = y > 0;
            let has_down = y + 1 < height;
            for x in 0..width {
                let index = row + x;
                let old = depth[index];
                let mut smooth = 0.0f32;
                if x > 0 {
                    smooth += right[index - 1] * (depth[index - 1] - old).clamp(-delta, delta);
                }
                if x + 1 < width {
                    smooth += right[index] * (depth[index + 1] - old).clamp(-delta, delta);
                }
                if has_up {
                    smooth +=
                        down[index - width] * (depth[index - width] - old).clamp(-delta, delta);
                }
                if has_down {
                    smooth += down[index] * (depth[index + width] - old).clamp(-delta, delta);
                }

                let inv = inv_denominator[index];
                let value = old + inv * (data[index] - weight[index] * old + lambda * smooth);
                next[index] = value;
                iteration_update = iteration_update.max((value - old).abs());
            }
        }

        std::mem::swap(&mut depth, &mut next);
        iterations += 1;
        max_update = iteration_update;
        if iteration_update < options.tolerance {
            converged = true;
            break;
        }
    }

    (
        depth,
        LevelSolve {
            iterations,
            max_update,
            converged,
        },
    )
}

fn build_weight_table() -> Vec<f32> {
    let mut table = Vec::with_capacity(WEIGHT_LUT_SIZE + 1);
    for index in 0..=WEIGHT_LUT_SIZE {
        let x = WEIGHT_LUT_MAX * index as f32 / WEIGHT_LUT_SIZE as f32;
        table.push((-x).exp());
    }
    table
}

/// Look up `exp(-diff^2 / (2*sigma^2))` with linear interpolation.
fn edge_weight(diff: f32, inv_two_sigma_sq: f32, table: &[f32]) -> f32 {
    let x = (diff * diff * inv_two_sigma_sq).min(WEIGHT_LUT_MAX);
    let position = x * (WEIGHT_LUT_SIZE as f32 / WEIGHT_LUT_MAX);
    let index = (position as usize).min(WEIGHT_LUT_SIZE - 1);
    let fraction = position - index as f32;
    let low = table[index];
    low + (table[index + 1] - low) * fraction
}

fn upsample_bilinear(
    source: &[f32],
    source_width: usize,
    source_height: usize,
    width: usize,
    height: usize,
) -> Vec<f32> {
    if source_width == width && source_height == height {
        return source.to_vec();
    }

    let mut out = vec![0.0f32; width * height];
    let scale_x = source_width as f32 / width as f32;
    let scale_y = source_height as f32 / height as f32;

    for y in 0..height {
        let fy = ((y as f32 + 0.5) * scale_y - 0.5).clamp(0.0, (source_height - 1) as f32);
        let y0 = fy.floor() as usize;
        let y1 = (y0 + 1).min(source_height - 1);
        let ty = fy - y0 as f32;

        for x in 0..width {
            let fx = ((x as f32 + 0.5) * scale_x - 0.5).clamp(0.0, (source_width - 1) as f32);
            let x0 = fx.floor() as usize;
            let x1 = (x0 + 1).min(source_width - 1);
            let tx = fx - x0 as f32;

            let top =
                source[y0 * source_width + x0] * (1.0 - tx) + source[y0 * source_width + x1] * tx;
            let bottom =
                source[y1 * source_width + x0] * (1.0 - tx) + source[y1 * source_width + x1] * tx;
            out[y * width + x] = top * (1.0 - ty) + bottom * ty;
        }
    }

    out
}

fn gaussian_smooth(values: &[f32], width: usize, height: usize, radius: f32) -> Vec<f32> {
    let sigma = (0.5 * radius).max(COST_EPSILON);
    let reach = radius.ceil() as isize;
    if reach <= 0 {
        return values.to_vec();
    }

    let mut kernel = Vec::with_capacity((2 * reach + 1) as usize);
    let mut total = 0.0f32;
    for offset in -reach..=reach {
        let weight = (-((offset * offset) as f32) / (2.0 * sigma * sigma)).exp();
        kernel.push(weight);
        total += weight;
    }
    for weight in &mut kernel {
        *weight /= total;
    }

    let mut horizontal = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for (offset, &weight) in (-reach..=reach).zip(&kernel) {
                let sx = (x as isize + offset).clamp(0, width as isize - 1) as usize;
                sum += weight * values[y * width + sx];
            }
            horizontal[y * width + x] = sum;
        }
    }

    let mut out = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for (offset, &weight) in (-reach..=reach).zip(&kernel) {
                let sy = (y as isize + offset).clamp(0, height as isize - 1) as usize;
                sum += weight * horizontal[sy * width + x];
            }
            out[y * width + x] = sum;
        }
    }

    out
}

fn sanitize_solve_options(options: SolveOptions) -> SolveOptions {
    SolveOptions {
        alpha: sanitize_non_negative(options.alpha),
        lambda: sanitize_non_negative(options.lambda),
        guide_sigma: sanitize_sigma(options.guide_sigma),
        huber_delta: if options.huber_delta.is_finite() {
            options.huber_delta.abs()
        } else {
            HUBER_DELTA
        },
        iterations: options.iterations,
        tolerance: sanitize_non_negative(options.tolerance),
        smoothing_radius: sanitize_non_negative(options.smoothing_radius),
    }
}

fn sanitize_depth(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn sanitize_guide(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// Otsu split of the log-energy histogram from §3.2 step 3.
///
/// The logarithm compresses the long energy tail so the split tracks texture
/// versus flat regions rather than a fixed percentage of pixels.
fn otsu_threshold(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }

    let mut min_log = f32::INFINITY;
    let mut max_log = f32::NEG_INFINITY;
    for &value in values {
        let log = (sanitize_energy(value) + COST_EPSILON).ln();
        min_log = min_log.min(log);
        max_log = max_log.max(log);
    }

    let span = max_log - min_log;
    if !span.is_finite() || span <= f32::EPSILON {
        return values
            .iter()
            .copied()
            .fold(0.0f32, |maximum, value| maximum.max(sanitize_energy(value)));
    }

    let mut histogram = [0u64; OTSU_BINS];
    let scale = (OTSU_BINS - 1) as f32 / span;
    for &value in values {
        let log = (sanitize_energy(value) + COST_EPSILON).ln();
        let bin = (((log - min_log) * scale).round() as usize).min(OTSU_BINS - 1);
        histogram[bin] += 1;
    }

    let total = values.len() as f64;
    let mut sum_all = 0.0f64;
    for (bin, &count) in histogram.iter().enumerate() {
        sum_all += bin as f64 * count as f64;
    }

    let mut best_variance = f64::NEG_INFINITY;
    let mut best_bin = 0usize;
    let mut weight_back = 0.0f64;
    let mut sum_back = 0.0f64;
    for (bin, &count) in histogram.iter().enumerate().take(OTSU_BINS - 1) {
        weight_back += count as f64;
        sum_back += bin as f64 * count as f64;
        if weight_back <= 0.0 {
            continue;
        }
        let weight_fore = total - weight_back;
        if weight_fore <= 0.0 {
            break;
        }
        let mean_back = sum_back / weight_back;
        let mean_fore = (sum_all - sum_back) / weight_fore;
        let variance = weight_back * weight_fore * (mean_back - mean_fore).powi(2);
        if variance > best_variance {
            best_variance = variance;
            best_bin = bin;
        }
    }

    let step = span / (OTSU_BINS - 1) as f32;
    let log_threshold = min_log + (best_bin as f32 + 0.5) * step;
    sanitize_non_negative(log_threshold.exp() - COST_EPSILON)
}

/// Nearest-rank quantile of `values`, with the rank interpolated at the edges.
fn quantile(values: &[f32], fraction: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    let rank = ((sorted.len() - 1) as f32 * fraction.clamp(0.0, 1.0)).round() as usize;
    *sorted.select_nth_unstable_by(rank, f32::total_cmp).1
}

/// `smoothstep(t, 1.5*t, energy)` from §3.2 step 5.
fn ramp(threshold: f32, energy: f32) -> f32 {
    if threshold <= COST_EPSILON {
        return if energy > COST_EPSILON { 1.0 } else { 0.0 };
    }
    let high = threshold * CONFIDENCE_RAMP_END;
    if energy <= threshold {
        return 0.0;
    }
    if energy >= high {
        return 1.0;
    }
    let t = (energy - threshold) / (high - threshold);
    t * t * (3.0 - 2.0 * t)
}

fn percentage(count: usize, total: usize) -> f32 {
    if total == 0 {
        0.0
    } else {
        100.0 * count as f32 / total as f32
    }
}

fn sanitize_sigma(value: f32) -> f32 {
    if value.is_finite() {
        value.abs().max(SIGMA_EPSILON)
    } else {
        SIGMA_EPSILON
    }
}

fn sanitize_keep_detail(value: Option<f32>) -> Option<f32> {
    value
        .filter(|percent| percent.is_finite())
        .map(|percent| percent.clamp(0.0, 100.0))
}

fn sanitize_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn sanitize_energy(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

fn sanitize_confidence(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
