//! Similarity refinement with ECC.
//!
//! Implements the robust pyramidal inverse-compositional Lucas–Kanade stage
//! from `docs/alignment.md` §5. The transform maps reference (output)
//! coordinates to source coordinates, matching [`crate::Similarity`]:
//!
//! ```text
//! x_source = a*x - b*y + tx
//! y_source = b*x + a*y + ty
//! ```
//!
//! ECC minimises `sum w(x) * rho(I_source(W(x; p)) - I_reference(x))`, with
//! `rho` a Huber loss on encoded luma. The inverse-compositional formulation
//! precomputes the template gradients and steepest-descent images per pyramid
//! level; robust IRLS weights are recomputed on every iteration.
//!
//! Tiles whose mean absolute residual exceeds three times the median tile
//! residual are excluded from the normal equations, so a moving branch or a
//! specular patch cannot pull the estimate. This is independent of the
//! per-pixel Huber weighting, which handles scattered outliers.

use std::fmt;

use crate::Similarity;

/// Default pyramid levels (`docs/alignment.md` §5).
pub const DEFAULT_LEVELS: usize = 5;
/// Default iterations per level.
pub const DEFAULT_MAX_ITERATIONS: usize = 50;
/// Default Huber threshold on normalized encoded luma.
pub const DEFAULT_HUBER_DELTA: f64 = 0.05;
/// Default convergence threshold, in pixels of the current level.
pub const DEFAULT_CONVERGENCE: f64 = 1e-4;
/// Normalized correlation below which the result is flagged unreliable.
pub const MIN_CORRELATION: f64 = 0.3;

/// Tile residual ratio above which a tile is rejected.
const TILE_RESIDUAL_RATIO: f64 = 3.0;
/// Tile edge in pixels at the current level.
const TILE_SIZE: usize = 16;
/// Coarsest pyramid level edge; smaller levels carry too little information.
const MIN_LEVEL_EDGE: usize = 16;

/// Parameters for [`refine_similarity`].
#[derive(Debug, Clone, PartialEq)]
pub struct EccParams {
    /// Requested pyramid levels, clamped so the coarsest level stays usable.
    pub levels: usize,
    /// Maximum iterations per level.
    pub max_iterations: usize,
    /// Huber threshold on normalized encoded luma.
    pub huber_delta: f64,
    /// Stop when the update moves no corner by more than this many pixels.
    pub convergence: f64,
    /// Correlation below this value sets [`EccResult::correlation_warns`].
    pub min_correlation: f64,
}

impl Default for EccParams {
    fn default() -> Self {
        Self {
            levels: DEFAULT_LEVELS,
            max_iterations: DEFAULT_MAX_ITERATIONS,
            huber_delta: DEFAULT_HUBER_DELTA,
            convergence: DEFAULT_CONVERGENCE,
            min_correlation: MIN_CORRELATION,
        }
    }
}

/// Result of a similarity refinement.
#[derive(Debug, Clone, PartialEq)]
pub struct EccResult {
    /// Refined transform in full-resolution coordinates.
    pub transform: Similarity,
    /// Pearson correlation between the warped source and the reference over
    /// masked, in-bounds pixels.
    pub correlation: f64,
    /// Mean absolute residual over the same pixels.
    pub residual: f64,
    /// Total iterations across all pyramid levels.
    pub iterations: usize,
    /// Pyramid levels actually used.
    pub levels: usize,
    /// Whether the finest level met the convergence threshold.
    pub converged: bool,
    /// Whether [`EccParams::min_correlation`] flagged the result.
    pub correlation_warns: bool,
}

impl EccResult {
    /// Whether the final correlation clears the configured threshold.
    #[must_use]
    pub fn is_reliable(&self) -> bool {
        !self.correlation_warns
    }
}

/// Invalid input to [`refine_similarity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EccError {
    /// One of the image dimensions is zero.
    EmptyInput,
    /// A source or reference buffer length does not match `width * height`.
    LengthMismatch {
        expected: usize,
        source: usize,
        reference: usize,
    },
    /// The mask length does not match `width * height`.
    MaskLength { expected: usize, actual: usize },
    /// A parameter is outside its meaningful range.
    InvalidParameters,
    /// The initial transform is not finite or is degenerate.
    InvalidInitial,
}

impl fmt::Display for EccError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(formatter, "ECC needs non-zero dimensions"),
            Self::LengthMismatch {
                expected,
                source,
                reference,
            } => write!(
                formatter,
                "ECC buffer length mismatch: expected {expected}, \
                 source {source}, reference {reference}"
            ),
            Self::MaskLength { expected, actual } => write!(
                formatter,
                "ECC mask length mismatch: expected {expected}, got {actual}"
            ),
            Self::InvalidParameters => write!(formatter, "invalid ECC parameters"),
            Self::InvalidInitial => write!(formatter, "invalid ECC initial transform"),
        }
    }
}

impl std::error::Error for EccError {}

/// Refine a similarity with robust pyramidal ECC.
///
/// `source` and `reference` are row-major encoded luma with `width * height`
/// samples, normally in `0..=1`. `initial` is a coarse estimate in
/// full-resolution coordinates (for example the Fourier–Mellin result);
/// `mask` is the reference alignment mask from
/// [`super::preprocess::AlignmentLevel::mask`]. `None` weights every pixel
/// equally.
///
/// # Errors
///
/// Returns [`EccError`] for empty or mismatched inputs, an invalid mask length,
/// invalid parameters, or a degenerate initial transform.
pub fn refine_similarity(
    source: &[f32],
    reference: &[f32],
    width: u32,
    height: u32,
    initial: &Similarity,
    mask: Option<&[f32]>,
    params: &EccParams,
) -> Result<EccResult, EccError> {
    if width == 0 || height == 0 {
        return Err(EccError::EmptyInput);
    }

    let expected = width as usize * height as usize;
    if source.len() != expected || reference.len() != expected {
        return Err(EccError::LengthMismatch {
            expected,
            source: source.len(),
            reference: reference.len(),
        });
    }
    if params.levels == 0
        || params.max_iterations == 0
        || !params.huber_delta.is_finite()
        || params.huber_delta <= 0.0
        || !params.convergence.is_finite()
        || params.convergence < 0.0
        || !params.min_correlation.is_finite()
    {
        return Err(EccError::InvalidParameters);
    }
    if !initial.a.is_finite()
        || !initial.b.is_finite()
        || !initial.tx.is_finite()
        || !initial.ty.is_finite()
        || initial.a.hypot(initial.b) <= 1e-12
    {
        return Err(EccError::InvalidInitial);
    }

    let mut mask_buffer = Vec::new();
    let mask = if let Some(mask) = mask {
        if mask.len() != expected {
            return Err(EccError::MaskLength {
                expected,
                actual: mask.len(),
            });
        }
        mask
    } else {
        mask_buffer.resize(expected, 1.0);
        &mask_buffer
    };

    let source_pyramid = build_pyramid(source, width, height, params.levels);
    let reference_pyramid = build_pyramid(reference, width, height, params.levels);
    let mask_pyramid = build_pyramid(mask, width, height, params.levels);
    let levels = source_pyramid.len().min(reference_pyramid.len());

    let mut transform = initial.clone();
    let mut iterations = 0usize;
    let mut converged = false;

    for level in (0..levels).rev() {
        let level_transform = transform_at_level(&transform, level);
        let outcome = refine_level(
            &reference_pyramid[level],
            &source_pyramid[level],
            &mask_pyramid[level],
            level_transform,
            params,
        );
        iterations += outcome.iterations;
        transform = transform_from_level(&outcome.transform, level);
        converged = outcome.converged;
    }

    let (correlation, residual) = correlation_and_residual(
        source,
        reference,
        mask,
        width as usize,
        height as usize,
        &transform,
    );

    Ok(EccResult {
        transform,
        correlation,
        residual,
        iterations,
        levels,
        converged,
        correlation_warns: correlation < params.min_correlation,
    })
}

struct PixelLevel {
    width: usize,
    height: usize,
    pixels: Vec<f32>,
}

fn build_pyramid(image: &[f32], width: u32, height: u32, max_levels: usize) -> Vec<PixelLevel> {
    let mut levels = Vec::with_capacity(max_levels.max(1));
    levels.push(PixelLevel {
        width: width as usize,
        height: height as usize,
        pixels: image.to_vec(),
    });

    while levels.len() < max_levels {
        let current = levels.last().expect("at least one level");
        let next_width = current.width.div_ceil(2);
        let next_height = current.height.div_ceil(2);
        if next_width < MIN_LEVEL_EDGE || next_height < MIN_LEVEL_EDGE {
            break;
        }
        levels.push(downsample2(current));
    }

    levels
}

/// 2×2 box downsample. A coarse pixel covers fine pixels `2x, 2x+1` on each
/// axis (clamped at odd edges), so its centre is at fine coordinate
/// `2x + 0.5`.
fn downsample2(level: &PixelLevel) -> PixelLevel {
    let width = level.width.div_ceil(2);
    let height = level.height.div_ceil(2);
    let mut pixels = vec![0.0f32; width * height];

    for y in 0..height {
        let y0 = (2 * y).min(level.height - 1);
        let y1 = (2 * y + 1).min(level.height - 1);
        for x in 0..width {
            let x0 = (2 * x).min(level.width - 1);
            let x1 = (2 * x + 1).min(level.width - 1);
            let sum = level.pixels[y0 * level.width + x0]
                + level.pixels[y0 * level.width + x1]
                + level.pixels[y1 * level.width + x0]
                + level.pixels[y1 * level.width + x1];
            pixels[y * width + x] = sum * 0.25;
        }
    }

    PixelLevel {
        width,
        height,
        pixels,
    }
}

/// Map a full-resolution transform into `level` coordinates.
///
/// One downsample maps fine coordinate `f` to `(f - 0.5) / 2`, because a
/// coarse pixel centre sits between the four fine samples it averages.
fn transform_at_level(transform: &Similarity, level: usize) -> Similarity {
    let scale = 2f64.powi(-(level as i32));
    let factor = 2f64.powi(level as i32);
    let offset = -(factor - 1.0) / (2.0 * factor);
    let row_x = transform.a - transform.b;
    let row_y = transform.b + transform.a;

    Similarity {
        a: transform.a,
        b: transform.b,
        tx: scale * transform.tx + offset * (1.0 - row_x),
        ty: scale * transform.ty + offset * (1.0 - row_y),
    }
}

/// Inverse of [`transform_at_level`].
fn transform_from_level(transform: &Similarity, level: usize) -> Similarity {
    let scale = 2f64.powi(-(level as i32));
    let factor = 2f64.powi(level as i32);
    let offset = -(factor - 1.0) / (2.0 * factor);
    let row_x = transform.a - transform.b;
    let row_y = transform.b + transform.a;

    Similarity {
        a: transform.a,
        b: transform.b,
        tx: (transform.tx - offset * (1.0 - row_x)) / scale,
        ty: (transform.ty - offset * (1.0 - row_y)) / scale,
    }
}

struct LevelOutcome {
    transform: Similarity,
    iterations: usize,
    converged: bool,
}

fn refine_level(
    reference: &PixelLevel,
    source: &PixelLevel,
    mask: &PixelLevel,
    initial: Similarity,
    params: &EccParams,
) -> LevelOutcome {
    let width = reference.width;
    let height = reference.height;
    let pixels = width * height;

    let (gradient_x, gradient_y) = central_gradient(reference);
    let mut steepest_a = vec![0.0f32; pixels];
    let mut steepest_b = vec![0.0f32; pixels];
    let mut steepest_tx = vec![0.0f32; pixels];
    let mut steepest_ty = vec![0.0f32; pixels];

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let fx = x as f64;
            let fy = y as f64;
            let gx = f64::from(gradient_x[index]);
            let gy = f64::from(gradient_y[index]);
            steepest_a[index] = (gx * fx + gy * fy) as f32;
            steepest_b[index] = (-gx * fy + gy * fx) as f32;
            steepest_tx[index] = gx as f32;
            steepest_ty[index] = gy as f32;
        }
    }

    let tiles_x = width.div_ceil(TILE_SIZE);
    let tiles_y = height.div_ceil(TILE_SIZE);
    let mut residual = vec![0.0f32; pixels];
    let mut rejected = vec![false; tiles_x * tiles_y];

    let mut transform = initial;
    let mut iterations = 0usize;
    let mut converged = false;

    for _ in 0..params.max_iterations {
        for y in 0..height {
            for x in 0..width {
                let index = y * width + x;
                residual[index] = f32::NAN;
                if mask.pixels[index] <= 0.0 {
                    continue;
                }
                let (u, v) = apply(&transform, x as f64, y as f64);
                if let Some(value) = sample_bilinear(source, u, v) {
                    residual[index] = value - reference.pixels[index];
                }
            }
        }

        rejected.fill(false);
        reject_outlier_tiles(
            &residual,
            &mask.pixels,
            width,
            height,
            tiles_x,
            tiles_y,
            &mut rejected,
        );

        let mut hessian = [[0.0f64; 4]; 4];
        let mut gradient = [0.0f64; 4];
        let mut samples = 0usize;

        for y in 0..height {
            let tile_row = (y / TILE_SIZE) * tiles_x;
            for x in 0..width {
                let index = y * width + x;
                let value = residual[index];
                if !value.is_finite() || mask.pixels[index] <= 0.0 {
                    continue;
                }
                if rejected[tile_row + x / TILE_SIZE] {
                    continue;
                }

                let weight = f64::from(mask.pixels[index])
                    * huber_weight(f64::from(value), params.huber_delta);
                if weight <= 0.0 {
                    continue;
                }

                let jacobian = [
                    f64::from(steepest_a[index]),
                    f64::from(steepest_b[index]),
                    f64::from(steepest_tx[index]),
                    f64::from(steepest_ty[index]),
                ];
                let error = f64::from(value);
                for (row, &jr) in jacobian.iter().enumerate() {
                    gradient[row] += weight * jr * error;
                    for (column, &jc) in jacobian.iter().enumerate() {
                        hessian[row][column] += weight * jr * jc;
                    }
                }
                samples += 1;
            }
        }

        if samples < 6 {
            break;
        }

        let trace: f64 = (0..4).map(|index| hessian[index][index]).sum();
        if !trace.is_finite() || trace <= 1e-12 {
            break;
        }
        let damping = trace * 1e-10;
        for (index, row) in hessian.iter_mut().enumerate() {
            row[index] += damping;
        }

        let Some(update) = solve4(hessian, gradient) else {
            break;
        };
        // The solve returns additive increments to `(a, b, tx, ty)` at the
        // identity. The group element applied to the template is
        // `(1 + da, db, dtx, dty)`.
        let update = Similarity {
            a: 1.0 + update[0],
            b: update[1],
            tx: update[2],
            ty: update[3],
        };
        if !update.a.is_finite()
            || !update.b.is_finite()
            || !update.tx.is_finite()
            || !update.ty.is_finite()
        {
            break;
        }

        transform = compose_inverse(&transform, &update);
        iterations += 1;

        if !transform.a.is_finite()
            || !transform.b.is_finite()
            || !transform.tx.is_finite()
            || !transform.ty.is_finite()
        {
            break;
        }
        let determinant = transform.a * transform.a + transform.b * transform.b;
        if !(0.25..=4.0).contains(&determinant) {
            break;
        }

        if max_displacement(&update, width, height) < params.convergence {
            converged = true;
            break;
        }
    }

    LevelOutcome {
        transform,
        iterations,
        converged,
    }
}

fn reject_outlier_tiles(
    residual: &[f32],
    mask: &[f32],
    width: usize,
    height: usize,
    tiles_x: usize,
    tiles_y: usize,
    rejected: &mut [bool],
) {
    let tile_count = tiles_x * tiles_y;
    let mut sums = vec![0.0f64; tile_count];
    let mut counts = vec![0usize; tile_count];

    for y in 0..height {
        let tile_row = (y / TILE_SIZE) * tiles_x;
        for x in 0..width {
            let index = y * width + x;
            let value = residual[index];
            if !value.is_finite() || mask[index] <= 0.0 {
                continue;
            }
            let tile = tile_row + x / TILE_SIZE;
            sums[tile] += f64::from(value.abs());
            counts[tile] += 1;
        }
    }

    let mut means: Vec<f64> = sums
        .iter()
        .zip(&counts)
        .filter(|&(_, &count)| count > 0)
        .map(|(&sum, &count)| sum / count as f64)
        .collect();
    if means.len() < 4 {
        return;
    }

    let median = median(&mut means);
    if !median.is_finite() || median <= 1e-6 {
        return;
    }

    let threshold = TILE_RESIDUAL_RATIO * median;
    let mut rejected_count = 0usize;
    let mut considered = 0usize;
    for (tile, (&sum, &count)) in sums.iter().zip(&counts).enumerate() {
        if count == 0 {
            continue;
        }
        considered += 1;
        if sum / count as f64 > threshold {
            rejected[tile] = true;
            rejected_count += 1;
        }
    }

    // Keep the majority: a bad initial estimate raises every tile, and
    // rejecting most of the image would leave too little to solve against.
    if rejected_count * 2 >= considered {
        rejected.fill(false);
    }
}

fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let middle = values.len() / 2;
    if values.len().is_multiple_of(2) {
        0.5 * (values[middle - 1] + values[middle])
    } else {
        values[middle]
    }
}

/// Huber IRLS weight: one inside the threshold, `delta / |r|` outside.
fn huber_weight(residual: f64, delta: f64) -> f64 {
    let magnitude = residual.abs();
    if magnitude <= delta {
        1.0
    } else {
        delta / magnitude
    }
}

fn central_gradient(level: &PixelLevel) -> (Vec<f32>, Vec<f32>) {
    let width = level.width;
    let height = level.height;
    let mut gradient_x = vec![0.0f32; width * height];
    let mut gradient_y = vec![0.0f32; width * height];

    for y in 0..height {
        let row = y * width;
        let y_prev = y.saturating_sub(1) * width;
        let y_next = (y + 1).min(height - 1) * width;
        for x in 0..width {
            let x_prev = x.saturating_sub(1);
            let x_next = (x + 1).min(width - 1);
            gradient_x[row + x] = (level.pixels[row + x_next] - level.pixels[row + x_prev]) * 0.5;
            gradient_y[row + x] = (level.pixels[y_next + x] - level.pixels[y_prev + x]) * 0.5;
        }
    }

    (gradient_x, gradient_y)
}

fn apply(transform: &Similarity, x: f64, y: f64) -> (f64, f64) {
    (
        transform.a * x - transform.b * y + transform.tx,
        transform.b * x + transform.a * y + transform.ty,
    )
}

/// Bilinear sample with edge clamp. Coordinates outside the image are
/// rejected so a translated frame does not pull in repeated border pixels.
fn sample_bilinear(level: &PixelLevel, x: f64, y: f64) -> Option<f32> {
    if !x.is_finite()
        || !y.is_finite()
        || x < 0.0
        || y < 0.0
        || x > (level.width as f64 - 1.0)
        || y > (level.height as f64 - 1.0)
    {
        return None;
    }

    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(level.width - 1);
    let y1 = (y0 + 1).min(level.height - 1);
    let tx = (x - x0 as f64) as f32;
    let ty = (y - y0 as f64) as f32;

    let top =
        level.pixels[y0 * level.width + x0] * (1.0 - tx) + level.pixels[y0 * level.width + x1] * tx;
    let bottom =
        level.pixels[y1 * level.width + x0] * (1.0 - tx) + level.pixels[y1 * level.width + x1] * tx;
    Some(top * (1.0 - ty) + bottom * ty)
}

/// Compose `transform` with the inverse of `update`:
/// `W(p) <- W(p) o W(update)^-1`.
fn compose_inverse(transform: &Similarity, update: &Similarity) -> Similarity {
    let scale = update.a * update.a + update.b * update.b;
    if scale <= 1e-24 {
        return transform.clone();
    }

    let a = (transform.a * update.a + transform.b * update.b) / scale;
    let b = (transform.b * update.a - transform.a * update.b) / scale;

    let inverse_tx = (update.a * update.tx + update.b * update.ty) / scale;
    let inverse_ty = (-update.b * update.tx + update.a * update.ty) / scale;

    Similarity {
        a,
        b,
        tx: transform.tx - (transform.a * inverse_tx - transform.b * inverse_ty),
        ty: transform.ty - (transform.b * inverse_tx + transform.a * inverse_ty),
    }
}

/// Largest corner displacement caused by `update`, in pixels.
fn max_displacement(update: &Similarity, width: usize, height: usize) -> f64 {
    let corners = [
        (0.0, 0.0),
        (width as f64 - 1.0, 0.0),
        (0.0, height as f64 - 1.0),
        (width as f64 - 1.0, height as f64 - 1.0),
    ];
    corners
        .iter()
        .map(|&(x, y)| {
            let (u, v) = apply(update, x, y);
            (u - x).hypot(v - y)
        })
        .fold(0.0f64, f64::max)
}

fn correlation_and_residual(
    source: &[f32],
    reference: &[f32],
    mask: &[f32],
    width: usize,
    height: usize,
    transform: &Similarity,
) -> (f64, f64) {
    let mut sum_source = 0.0f64;
    let mut sum_reference = 0.0f64;
    let mut sum_source_squared = 0.0f64;
    let mut sum_reference_squared = 0.0f64;
    let mut sum_product = 0.0f64;
    let mut sum_absolute = 0.0f64;
    let mut count = 0usize;
    let source_level = PixelLevel {
        width,
        height,
        pixels: source.to_vec(),
    };

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if mask[index] <= 0.0 {
                continue;
            }
            let (u, v) = apply(transform, x as f64, y as f64);
            if u < 0.0 || v < 0.0 || u > (width as f64 - 1.0) || v > (height as f64 - 1.0) {
                continue;
            }
            let Some(sample) = sample_bilinear(&source_level, u, v) else {
                continue;
            };

            let a = f64::from(sample);
            let b = f64::from(reference[index]);
            sum_source += a;
            sum_reference += b;
            sum_source_squared += a * a;
            sum_reference_squared += b * b;
            sum_product += a * b;
            sum_absolute += (a - b).abs();
            count += 1;
        }
    }

    if count < 4 {
        return (0.0, 0.0);
    }

    let n = count as f64;
    let covariance = sum_product - sum_source * sum_reference / n;
    let variance_source = sum_source_squared - sum_source * sum_source / n;
    let variance_reference = sum_reference_squared - sum_reference * sum_reference / n;
    let denominator = (variance_source.max(0.0) * variance_reference.max(0.0)).sqrt();
    let correlation = if denominator <= 1e-20 {
        0.0
    } else {
        (covariance / denominator).clamp(-1.0, 1.0)
    };

    (correlation, sum_absolute / n)
}

fn solve4(mut matrix: [[f64; 4]; 4], mut rhs: [f64; 4]) -> Option<[f64; 4]> {
    for column in 0..4 {
        let mut pivot = column;
        for row in column + 1..4 {
            if matrix[row][column].abs() > matrix[pivot][column].abs() {
                pivot = row;
            }
        }
        if matrix[pivot][column].abs() < 1e-12 {
            return None;
        }
        if pivot != column {
            matrix.swap(pivot, column);
            rhs.swap(pivot, column);
        }

        let diagonal = matrix[column][column];
        for row in column + 1..4 {
            let factor = matrix[row][column] / diagonal;
            if factor == 0.0 {
                continue;
            }
            let pivot_values = matrix[column];
            for (target, &source) in matrix[row].iter_mut().zip(pivot_values.iter()).skip(column) {
                *target -= factor * source;
            }
            rhs[row] -= factor * rhs[column];
        }
    }

    let mut solution = [0.0f64; 4];
    for row in (0..4).rev() {
        let mut sum = rhs[row];
        for column in row + 1..4 {
            sum -= matrix[row][column] * solution[column];
        }
        solution[row] = sum / matrix[row][row];
    }
    Some(solution)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(values: &[f32], width: usize, height: usize) -> PixelLevel {
        PixelLevel {
            width,
            height,
            pixels: values.to_vec(),
        }
    }

    #[test]
    fn level_transform_round_trips() {
        let transform = Similarity {
            a: 1.004,
            b: 0.0035,
            tx: 4.25,
            ty: -2.5,
        };
        for level_index in 0..5 {
            let coarse = transform_at_level(&transform, level_index);
            let restored = transform_from_level(&coarse, level_index);
            assert!((restored.a - transform.a).abs() < 1e-12);
            assert!((restored.b - transform.b).abs() < 1e-12);
            assert!((restored.tx - transform.tx).abs() < 1e-12);
            assert!((restored.ty - transform.ty).abs() < 1e-12);
        }
    }

    #[test]
    fn level_transform_matches_coordinate_mapping() {
        let transform = Similarity {
            a: 1.004,
            b: 0.0035,
            tx: 4.25,
            ty: -2.5,
        };
        let coarse = transform_at_level(&transform, 2);
        let scale = 0.25;
        let offset = -0.375;
        for &(x, y) in &[(0.0, 0.0), (17.0, 9.0), (63.0, 31.0)] {
            // `x, y` are level coordinates; map them to fine coordinates,
            // apply the full-resolution transform, then map the result back.
            let fine_x = (x - offset) / scale;
            let fine_y = (y - offset) / scale;
            let (u, v) = apply(&transform, fine_x, fine_y);
            let (cu, cv) = apply(&coarse, x, y);
            assert!((cu - (scale * u + offset)).abs() < 1e-12, "{cu}");
            assert!((cv - (scale * v + offset)).abs() < 1e-12, "{cv}");
        }
    }

    #[test]
    fn huber_weight_is_quadratic_then_linear() {
        assert_eq!(huber_weight(0.02, 0.05), 1.0);
        assert_eq!(huber_weight(-0.05, 0.05), 1.0);
        assert!((huber_weight(0.5, 0.05) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn solve4_solves_known_system() {
        let matrix = [
            [4.0, 1.0, 0.0, 0.0],
            [1.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, 2.0, 1.0],
            [0.0, 0.0, 1.0, 2.0],
        ];
        let expected = [1.0, -2.0, 0.5, 3.0];
        let mut rhs = [0.0f64; 4];
        for (row, value) in rhs.iter_mut().enumerate() {
            *value = (0..4)
                .map(|column| matrix[row][column] * expected[column])
                .sum();
        }
        let solution = solve4(matrix, rhs).expect("nonsingular");
        for (actual, expected) in solution.iter().zip(expected) {
            assert!((actual - expected).abs() < 1e-12);
        }
    }

    #[test]
    fn rejects_invalid_inputs() {
        let params = EccParams::default();
        let identity = Similarity {
            a: 1.0,
            b: 0.0,
            tx: 0.0,
            ty: 0.0,
        };
        assert_eq!(
            refine_similarity(&[], &[], 0, 8, &identity, None, &params).unwrap_err(),
            EccError::EmptyInput
        );
        assert_eq!(
            refine_similarity(&[0.0; 3], &[0.0; 4], 2, 2, &identity, None, &params).unwrap_err(),
            EccError::LengthMismatch {
                expected: 4,
                source: 3,
                reference: 4,
            }
        );
        assert_eq!(
            refine_similarity(
                &[0.0; 4],
                &[0.0; 4],
                2,
                2,
                &identity,
                Some(&[0.0; 3]),
                &params
            )
            .unwrap_err(),
            EccError::MaskLength {
                expected: 4,
                actual: 3,
            }
        );
        let invalid = EccParams {
            huber_delta: 0.0,
            ..EccParams::default()
        };
        assert_eq!(
            refine_similarity(&[0.0; 4], &[0.0; 4], 2, 2, &identity, None, &invalid).unwrap_err(),
            EccError::InvalidParameters
        );
        let degenerate = Similarity {
            a: 0.0,
            b: 0.0,
            tx: 0.0,
            ty: 0.0,
        };
        assert_eq!(
            refine_similarity(&[0.0; 4], &[0.0; 4], 2, 2, &degenerate, None, &params).unwrap_err(),
            EccError::InvalidInitial
        );
    }

    #[test]
    fn flat_images_do_not_diverge() {
        let params = EccParams::default();
        let identity = Similarity {
            a: 1.0,
            b: 0.0,
            tx: 0.0,
            ty: 0.0,
        };
        let image = vec![0.5f32; 64 * 64];
        let result =
            refine_similarity(&image, &image, 64, 64, &identity, None, &params).expect("valid");
        assert!((result.transform.a - 1.0).abs() < 1e-9);
        assert!((result.transform.tx).abs() < 1e-9);
        assert!(!result.converged);
    }

    #[test]
    fn downsampling_pyramid_keeps_usable_levels() {
        let image: Vec<f32> = (0..256 * 128)
            .map(|index| (index % 251) as f32 / 251.0)
            .collect();
        let pyramid = build_pyramid(&image, 256, 128, 8);
        assert!(pyramid.len() >= 2);
        assert_eq!((pyramid[0].width, pyramid[0].height), (256, 128));
        for level in &pyramid[1..] {
            assert!(level.width >= MIN_LEVEL_EDGE);
            assert!(level.height >= MIN_LEVEL_EDGE);
            assert_eq!(level.pixels.len(), level.width * level.height);
        }
    }

    #[test]
    fn bilinear_sampling_is_exact_on_grid() {
        let image = level(&[0.0, 0.25, 0.5, 0.75], 2, 2);
        assert_eq!(sample_bilinear(&image, 0.0, 0.0), Some(0.0));
        assert_eq!(sample_bilinear(&image, 1.0, 1.0), Some(0.75));
        assert_eq!(sample_bilinear(&image, 0.5, 0.5), Some(0.375));
        assert_eq!(sample_bilinear(&image, -0.1, 0.0), None);
    }
}
