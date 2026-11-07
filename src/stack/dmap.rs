//! Cost volume, depth solve, and interpolation.

use crate::DepthField;

/// Additive epsilon for energy normalizations.
pub const COST_EPSILON: f32 = 1.0e-6;

/// Relative best-vs-second margin at or below which the argmax is ambiguous.
///
/// The automatic contrast threshold in `docs/stacking.md` §3.2 refines this
/// gate; §3.1 only needs to identify clearly tied pixels.
pub const FLAT_MARGIN: f32 = 1.0e-3;

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
