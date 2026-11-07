//! No-sharp-frame coverage mask.
//!
//! Implements the coverage half of `docs/focus-measure.md` §5. A pixel is a
//! coverage gap when the best energy is below the winning frame's calibrated
//! floor `tau_cov * sigma^2`; there is no frame that is confidently sharp
//! there. The mask reports its fraction and the bounding boxes of its
//! connected gap regions so callers can surface focus-step warnings.

use super::confidence::{ConfidenceMap, FrameCalibration};

/// Coverage multiplier `tau_cov` from §5.
pub const COVERAGE_TAU: f32 = 4.0;

/// Axis-aligned bounding box in pixels, with exclusive `x1`/`y1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundingBox {
    /// Left edge, inclusive.
    pub x0: u32,
    /// Top edge, inclusive.
    pub y0: u32,
    /// Right edge, exclusive.
    pub x1: u32,
    /// Bottom edge, exclusive.
    pub y1: u32,
}

impl BoundingBox {
    /// Box width in pixels.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    /// Box height in pixels.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    /// Box area in pixels.
    #[must_use]
    pub fn area(&self) -> u64 {
        u64::from(self.width()) * u64::from(self.height())
    }
}

/// Per-pixel coverage gaps.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageMask {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// `true` where no frame has a trustworthy sharp pixel.
    pub gap: Vec<bool>,
}

impl CoverageMask {
    /// Build the coverage mask from a confidence map and per-frame calibration.
    ///
    /// The floor is evaluated for the winning frame at each pixel. A missing
    /// calibration contributes a zero floor.
    ///
    /// # Panics
    ///
    /// Panics if the confidence map buffers disagree in length.
    #[must_use]
    pub fn from_confidence(
        map: &ConfidenceMap,
        calibrations: &[FrameCalibration],
        tau_cov: f32,
    ) -> Self {
        let count = map.len();
        assert_eq!(
            map.best_frame.len(),
            count,
            "best-frame buffer length mismatch"
        );
        assert_eq!(
            map.best_energy.len(),
            count,
            "best-energy buffer length mismatch"
        );

        let tau_cov = sanitize_non_negative(tau_cov);
        let gap = (0..count)
            .map(|index| {
                let frame = map.best_frame[index] as usize;
                let sigma = calibrations
                    .get(frame)
                    .map_or(0.0, |calibration| sanitize_non_negative(calibration.sigma));
                map.best_energy[index] < tau_cov * sigma * sigma
            })
            .collect();

        Self {
            width: map.width,
            height: map.height,
            gap,
        }
    }

    /// Build the coverage mask with one floor shared by every pixel.
    ///
    /// # Panics
    ///
    /// Panics if `best_energy` is not `width * height` samples long.
    #[must_use]
    pub fn from_uniform_floor(best_energy: &[f32], width: u32, height: u32, floor: f32) -> Self {
        let count = (width as usize) * (height as usize);
        assert_eq!(
            best_energy.len(),
            count,
            "best-energy buffer length mismatch"
        );
        let floor = sanitize_non_negative(floor);
        Self {
            width,
            height,
            gap: best_energy.iter().map(|&energy| energy < floor).collect(),
        }
    }

    /// Number of pixels in the mask.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.width * self.height) as usize
    }

    /// Whether the mask has no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of coverage-gap pixels.
    #[must_use]
    pub fn gap_count(&self) -> usize {
        self.gap.iter().filter(|&&gap| gap).count()
    }

    /// Fraction of the image covered by gaps, in `[0, 1]`.
    #[must_use]
    pub fn gap_fraction(&self) -> f32 {
        if self.gap.is_empty() {
            0.0
        } else {
            self.gap_count() as f32 / self.gap.len() as f32
        }
    }

    /// Fraction of the image with a trustworthy sharp frame, in `[0, 1]`.
    #[must_use]
    pub fn covered_fraction(&self) -> f32 {
        1.0 - self.gap_fraction()
    }

    /// Whether the gap fraction exceeds `threshold`.
    #[must_use]
    pub fn is_significant(&self, threshold: f32) -> bool {
        self.gap_fraction() > sanitize_non_negative(threshold)
    }

    /// Bounding boxes of the 8-connected gap components, largest first.
    ///
    /// Components are ordered by area descending, then by top edge and left
    /// edge so the result is deterministic.
    #[must_use]
    pub fn gap_boxes(&self) -> Vec<BoundingBox> {
        let width = self.width as usize;
        let height = self.height as usize;
        if width == 0 || height == 0 {
            return Vec::new();
        }

        let mut visited = vec![false; width * height];
        let mut boxes = Vec::new();
        let mut stack = Vec::new();

        for start in 0..width * height {
            if !self.gap[start] || visited[start] {
                continue;
            }

            visited[start] = true;
            stack.push(start);
            let mut pixels = 0usize;
            let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);

            while let Some(index) = stack.pop() {
                let x = index % width;
                let y = index / width;
                pixels += 1;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);

                for dy in -1..=1isize {
                    for dx in -1..=1isize {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x as isize + dx;
                        let ny = y as isize + dy;
                        if nx < 0 || ny < 0 || nx >= width as isize || ny >= height as isize {
                            continue;
                        }
                        let neighbor = ny as usize * width + nx as usize;
                        if self.gap[neighbor] && !visited[neighbor] {
                            visited[neighbor] = true;
                            stack.push(neighbor);
                        }
                    }
                }
            }

            boxes.push((
                pixels,
                BoundingBox {
                    x0: x0 as u32,
                    y0: y0 as u32,
                    x1: x1 as u32,
                    y1: y1 as u32,
                },
            ));
        }

        boxes.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| a.1.y0.cmp(&b.1.y0))
                .then_with(|| a.1.x0.cmp(&b.1.x0))
        });
        boxes.into_iter().map(|(_, bounds)| bounds).collect()
    }
}

/// Clamp non-finite values to zero and negatives to zero.
fn sanitize_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
