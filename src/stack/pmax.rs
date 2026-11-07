//! Pyramid maximum contrast blending.

use crate::focus::confidence::ConfidenceMap;
use crate::focus::energy::EnergyMaps;

/// Default PMax temperature `beta` from `docs/stacking.md` §2.1.
pub const DEFAULT_BETA: f32 = 6.0;
/// Additive constant in the per-level energy normalization.
pub const WEIGHT_EPSILON: f32 = 1.0e-6;
/// Percentile used to normalize energies across frames and levels.
pub const ENERGY_PERCENTILE: f32 = 0.99;
/// Chroma weights are mixed this far toward uniform (§2.1).
pub const CHROMA_MIX: f32 = 0.5;

/// Number of bins in the streaming percentile histogram.
const PERCENTILE_BINS: usize = 4096;

/// Options for [`LevelWeights::compute`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WeightOptions {
    /// Softmax temperature. Higher is closer to a hard max.
    pub beta: f32,
}

impl Default for WeightOptions {
    fn default() -> Self {
        Self { beta: DEFAULT_BETA }
    }
}

/// Per-frame weights for one pyramid level, frame-major.
///
/// `weights[frame * width * height + pixel]` holds the luma weight and
/// [`Self::chroma`] the corresponding smoother chroma weight.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WeightLevel {
    /// Level width in pixels.
    pub width: u32,
    /// Level height in pixels.
    pub height: u32,
    /// Number of frames `N`.
    pub frames: usize,
    /// 99th percentile of the level's energies, before normalization.
    pub p99: f32,
    /// Softmax weights after confidence gating, frame-major.
    pub weights: Vec<f32>,
    /// Chroma weights `0.5 * w + 0.5 / N`, frame-major.
    pub chroma: Vec<f32>,
}

impl WeightLevel {
    /// Number of pixels in the level.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.width * self.height) as usize
    }

    /// Whether the level has no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Luma weights for one frame, if `frame` is in range.
    #[must_use]
    pub fn frame_weights(&self, frame: usize) -> Option<&[f32]> {
        if frame >= self.frames {
            return None;
        }
        let count = self.len();
        self.weights.get(frame * count..(frame + 1) * count)
    }

    /// Chroma weights for one frame, if `frame` is in range.
    #[must_use]
    pub fn frame_chroma(&self, frame: usize) -> Option<&[f32]> {
        if frame >= self.frames {
            return None;
        }
        let count = self.len();
        self.chroma.get(frame * count..(frame + 1) * count)
    }
}

/// PMax weights for every pyramid level, finest level first.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LevelWeights {
    levels: Vec<WeightLevel>,
}

impl LevelWeights {
    /// Compute weights for all levels from per-frame energy maps.
    ///
    /// `energies` holds one [`EnergyMaps`] per frame; all frames must have the
    /// same level count and dimensions. `confidence` is the full-resolution
    /// confidence map from `docs/focus-measure.md` §5. Coarser levels sample the
    /// nearest full-resolution confidence pixel (`x * 2^l`, `y * 2^l`).
    ///
    /// # Panics
    ///
    /// Panics if `energies` is empty, if the frame level counts or dimensions
    /// differ, or if the finest level does not match the confidence map.
    #[must_use]
    pub fn compute(
        energies: &[EnergyMaps],
        confidence: &ConfidenceMap,
        options: WeightOptions,
    ) -> Self {
        assert!(!energies.is_empty(), "at least one frame is required");
        let level_count = energies[0].levels().len();
        assert!(level_count > 0, "at least one pyramid level is required");
        assert!(confidence.width > 0 && confidence.height > 0);
        let finest = &energies[0].levels()[0];
        assert_eq!(finest.width, confidence.width, "confidence width mismatch");
        assert_eq!(
            finest.height, confidence.height,
            "confidence height mismatch"
        );
        let confidence_len = confidence.len();
        assert_eq!(
            confidence.confidence.len(),
            confidence_len,
            "confidence buffer length mismatch"
        );

        let beta = sanitize_beta(options.beta);
        let levels = (0..level_count)
            .map(|level_index| {
                let reference = &energies[0].levels()[level_index];
                let (width, height) = (reference.width, reference.height);
                let count = (width * height) as usize;
                assert!(count > 0, "level dimensions must be non-zero");

                let mut maps = Vec::with_capacity(energies.len());
                for maps_for_frame in energies {
                    let level = maps_for_frame
                        .level(level_index)
                        .expect("frame level count mismatch");
                    assert_eq!(
                        (level.width, level.height),
                        (width, height),
                        "frame level dimension mismatch"
                    );
                    assert_eq!(level.energy.len(), count, "energy buffer length mismatch");
                    maps.push(level.energy.as_slice());
                }

                let sampled = sample_confidence(
                    &confidence.confidence,
                    confidence.width,
                    confidence.height,
                    width,
                    height,
                    level_index,
                );
                level_weights(&maps, width, height, &sampled, beta)
            })
            .collect();

        Self { levels }
    }

    /// Weight maps from the finest level to the coarsest residual.
    #[must_use]
    pub fn levels(&self) -> &[WeightLevel] {
        &self.levels
    }

    /// Weight map for `index`, if present.
    #[must_use]
    pub fn level(&self, index: usize) -> Option<&WeightLevel> {
        self.levels.get(index)
    }

    /// Number of levels.
    #[must_use]
    pub fn len(&self) -> usize {
        self.levels.len()
    }

    /// Whether no levels are present.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }
}

/// Compute PMax weights for one level.
///
/// `energies` holds one row-major energy map per frame, all `width * height`
/// samples. `confidence` is the level-resolution confidence in `[0, 1]`.
/// Energies are normalized by their 99th percentile and softmaxed with `beta`;
/// confidence then floors the result toward uniform, and chroma weights are
/// mixed halfway toward uniform per §2.1.
///
/// # Panics
///
/// Panics if `energies` is empty, dimensions are zero, or any buffer has the
/// wrong length.
#[must_use]
pub fn level_weights(
    energies: &[&[f32]],
    width: u32,
    height: u32,
    confidence: &[f32],
    beta: f32,
) -> WeightLevel {
    assert!(!energies.is_empty(), "at least one frame is required");
    assert!(width > 0 && height > 0, "level dimensions must be non-zero");
    let count = (width * height) as usize;
    for energy in energies {
        assert_eq!(energy.len(), count, "energy buffer length mismatch");
    }
    assert_eq!(confidence.len(), count, "confidence buffer length mismatch");

    let frames = energies.len();
    let p99 = percentile_99(energies);
    let denominator = p99 + WEIGHT_EPSILON;
    let beta = sanitize_beta(beta);
    let inv_n = 1.0 / frames as f32;

    let mut weights = vec![0.0f32; frames * count];
    let mut chroma = vec![0.0f32; frames * count];

    for index in 0..count {
        let mut max_logit = f32::NEG_INFINITY;
        for energy in energies {
            let normalized = sanitize_energy(energy[index]) / denominator;
            max_logit = max_logit.max(beta * normalized);
        }

        let mut exp_sum = 0.0f32;
        for energy in energies {
            let normalized = sanitize_energy(energy[index]) / denominator;
            exp_sum += (beta * normalized - max_logit).exp();
        }
        if !exp_sum.is_finite() || exp_sum <= 0.0 {
            exp_sum = 0.0;
        }

        let c = sanitize_confidence(confidence[index]);
        let mut total = 0.0f32;
        for (frame, energy) in energies.iter().enumerate() {
            let normalized = sanitize_energy(energy[index]) / denominator;
            let softmax = if exp_sum > 0.0 {
                (beta * normalized - max_logit).exp() / exp_sum
            } else {
                inv_n
            };
            let weight = (1.0 - c) * inv_n + c * softmax;
            weights[frame * count + index] = weight;
            total += weight;
        }

        let norm = if total.is_finite() && total > 0.0 {
            1.0 / total
        } else {
            inv_n
        };
        for frame in 0..frames {
            let slot = frame * count + index;
            let weight = weights[slot] * norm;
            weights[slot] = weight;
            chroma[slot] = CHROMA_MIX * weight + (1.0 - CHROMA_MIX) * inv_n;
        }
    }

    WeightLevel {
        width,
        height,
        frames,
        p99,
        weights,
        chroma,
    }
}

/// 99th percentile of all finite, non-negative samples across frames.
fn percentile_99(energies: &[&[f32]]) -> f32 {
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    let mut count = 0usize;
    for energy in energies {
        for &raw in *energy {
            let value = sanitize_energy(raw);
            min = min.min(value);
            max = max.max(value);
            count += 1;
        }
    }
    if count == 0 {
        return 0.0;
    }
    if max <= min {
        return max.max(0.0);
    }

    let mut histogram = vec![0u32; PERCENTILE_BINS];
    let scale = (PERCENTILE_BINS as f32 - 1.0) / (max - min);
    for energy in energies {
        for &raw in *energy {
            let value = sanitize_energy(raw);
            let bin = (((value - min) * scale) as usize).min(PERCENTILE_BINS - 1);
            histogram[bin] += 1;
        }
    }

    // Nearest-rank percentile: ceil(p * n) - 1.
    let rank = ((count as f64 * f64::from(ENERGY_PERCENTILE)).ceil() as usize)
        .saturating_sub(1)
        .min(count - 1);

    let bin_width = (max - min) / (PERCENTILE_BINS as f32 - 1.0);
    let mut cumulative = 0usize;
    for (bin, &bucket) in histogram.iter().enumerate() {
        let bucket = bucket as usize;
        if cumulative + bucket > rank {
            let offset = (rank - cumulative) as f32;
            let fraction = if bucket > 0 {
                offset / bucket as f32
            } else {
                0.0
            };
            return (min + (bin as f32 + fraction) * bin_width).clamp(min, max);
        }
        cumulative += bucket;
    }
    max
}

fn sample_confidence(
    confidence: &[f32],
    full_width: u32,
    full_height: u32,
    width: u32,
    height: u32,
    level: usize,
) -> Vec<f32> {
    let step = 1usize << level.min(31);
    let mut sampled = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        let sy = (y.saturating_mul(step)).min(full_height as usize - 1);
        for x in 0..width as usize {
            let sx = (x.saturating_mul(step)).min(full_width as usize - 1);
            sampled.push(confidence[sy * full_width as usize + sx]);
        }
    }
    sampled
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

fn sanitize_beta(beta: f32) -> f32 {
    if beta.is_finite() { beta } else { DEFAULT_BETA }
}
