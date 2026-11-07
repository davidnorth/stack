//! Residual estimates and alignment warnings.
//!
//! Implements `docs/alignment.md` §8. The source and reference luma are tiled
//! at the diagnostic resolution (half resolution by default); each tile gets a
//! local phase correlation whose displacement is scaled to full-resolution
//! pixels. Tiles also report normalized cross-correlation after removing that
//! displacement, so a well-aligned but low-contrast tile is distinguishable
//! from a genuinely shifted one.
//!
//! The report exposes the median and 95th percentile residual. `p95 > 0.3 px`
//! warns; `p95 > 1.0 px` fails unless the caller opts into `force`.

use std::fmt;

use super::phase::phase_correlate;

/// Default tile edge, in diagnostic-resolution pixels (`docs/alignment.md` §8).
pub const DEFAULT_TILE_SIZE: usize = 128;
/// Default diagnostic-to-full-resolution scale: diagnostics run at half
/// resolution.
pub const DEFAULT_FULL_RES_SCALE: f64 = 2.0;
/// Residual p95 above which the alignment warns, in full-resolution pixels.
pub const DEFAULT_WARN_P95: f64 = 0.3;
/// Residual p95 above which the alignment fails, in full-resolution pixels.
pub const DEFAULT_FAIL_P95: f64 = 1.0;
/// Tile correlation below which the result is considered unreliable
/// (`docs/alignment.md` §5).
pub const MIN_CORRELATION: f64 = 0.3;

/// Smallest tile edge worth phase-correlating.
const MIN_TILE_EDGE: usize = 8;

/// Parameters for [`diagnose_alignment`].
#[derive(Debug, Clone, PartialEq)]
pub struct DiagnosticsParams {
    /// Tile edge in diagnostic-resolution pixels.
    pub tile_size: usize,
    /// Multiplier converting diagnostic displacement to full-resolution pixels.
    pub full_res_scale: f64,
    /// Residual p95 that triggers a warning, in full-resolution pixels.
    pub warn_p95: f64,
    /// Residual p95 that aborts the run, in full-resolution pixels.
    pub fail_p95: f64,
    /// Continue despite a failure threshold breach.
    pub force: bool,
}

impl Default for DiagnosticsParams {
    fn default() -> Self {
        Self {
            tile_size: DEFAULT_TILE_SIZE,
            full_res_scale: DEFAULT_FULL_RES_SCALE,
            warn_p95: DEFAULT_WARN_P95,
            fail_p95: DEFAULT_FAIL_P95,
            force: false,
        }
    }
}

/// Diagnostic result for one tile.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TileDiagnostic {
    /// Tile origin in diagnostic-resolution pixels.
    pub x: u32,
    /// Tile origin in diagnostic-resolution pixels.
    pub y: u32,
    /// Tile width in diagnostic-resolution pixels.
    pub width: u32,
    /// Tile height in diagnostic-resolution pixels.
    pub height: u32,
    /// Displacement magnitude in full-resolution pixels.
    pub residual_px: f64,
    /// Pearson correlation after removing the measured displacement.
    pub correlation: f64,
    /// Phase-correlation peak confidence.
    pub confidence: f64,
}

impl TileDiagnostic {
    /// Whether the phase-correlation peak was strong enough to trust.
    #[must_use]
    pub fn is_reliable(&self) -> bool {
        self.confidence >= super::phase::MIN_CONFIDENCE
    }
}

/// Median and 95th percentile of a set of residuals.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ResidualStats {
    /// Median residual, in full-resolution pixels.
    pub median: f64,
    /// 95th percentile residual, in full-resolution pixels.
    pub p95: f64,
}

/// Aggregate alignment diagnostics for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignmentDiagnostics {
    /// Per-tile results, row-major.
    pub tiles: Vec<TileDiagnostic>,
    /// Statistics over reliable tiles.
    pub residual_px: ResidualStats,
    /// Median tile correlation over reliable tiles.
    pub correlation: f64,
    /// Advisory warnings, formatted for the report.
    pub warnings: Vec<String>,
    /// Whether the failure threshold was crossed without `force`.
    pub failed: bool,
}

/// Invalid input to [`diagnose_alignment`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticsError {
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
}

impl fmt::Display for DiagnosticsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyInput => write!(formatter, "diagnostics need non-zero dimensions"),
            Self::LengthMismatch {
                expected,
                source,
                reference,
            } => write!(
                formatter,
                "diagnostics buffer length mismatch: expected {expected}, \
                 source {source}, reference {reference}"
            ),
            Self::InvalidParameters => write!(formatter, "invalid diagnostics parameters"),
        }
    }
}

impl std::error::Error for DiagnosticsError {}

/// Measure the per-tile residual between an aligned source and its reference.
///
/// `source` and `reference` are row-major encoded luma at the diagnostic
/// resolution with `width * height` samples. `source` must already be warped
/// into the reference frame. Displacements are multiplied by
/// [`DiagnosticsParams::full_res_scale`] before percentile aggregation.
///
/// Tiles whose phase-correlation confidence falls below
/// [`super::phase::MIN_CONFIDENCE`] are reported but excluded from the
/// statistics.
///
/// # Errors
///
/// Returns [`DiagnosticsError`] for empty or mismatched inputs and for
/// non-positive parameters.
pub fn diagnose_alignment(
    source: &[f32],
    reference: &[f32],
    width: u32,
    height: u32,
    params: &DiagnosticsParams,
) -> Result<AlignmentDiagnostics, DiagnosticsError> {
    if width == 0 || height == 0 {
        return Err(DiagnosticsError::EmptyInput);
    }
    let expected = width as usize * height as usize;
    if source.len() != expected || reference.len() != expected {
        return Err(DiagnosticsError::LengthMismatch {
            expected,
            source: source.len(),
            reference: reference.len(),
        });
    }
    if params.tile_size < MIN_TILE_EDGE
        || !params.full_res_scale.is_finite()
        || params.full_res_scale <= 0.0
        || !params.warn_p95.is_finite()
        || !params.fail_p95.is_finite()
        || params.warn_p95 <= 0.0
        || params.fail_p95 < params.warn_p95
    {
        return Err(DiagnosticsError::InvalidParameters);
    }

    let width_usize = width as usize;
    let height_usize = height as usize;
    let tile_size = params.tile_size.min(width_usize).min(height_usize);

    let mut tiles = Vec::new();
    let mut reliable = Vec::new();
    let mut correlations = Vec::new();

    let mut y = 0;
    while y < height_usize {
        let tile_height = tile_size.min(height_usize - y);
        let mut x = 0;
        while x < width_usize {
            let tile_width = tile_size.min(width_usize - x);
            if tile_width >= MIN_TILE_EDGE
                && tile_height >= MIN_TILE_EDGE
                && let Some(tile) = diagnose_tile(
                    source,
                    reference,
                    width_usize,
                    x,
                    y,
                    tile_width,
                    tile_height,
                    params.full_res_scale,
                )
            {
                if tile.is_reliable() {
                    reliable.push(tile.residual_px);
                    if tile.correlation.is_finite() {
                        correlations.push(tile.correlation);
                    }
                }
                tiles.push(tile);
            }
            x += tile_size;
        }
        y += tile_size;
    }

    reliable.sort_by(f64::total_cmp);
    let residual_px = ResidualStats {
        median: quantile(&reliable, 0.5),
        p95: quantile(&reliable, 0.95),
    };
    correlations.sort_by(f64::total_cmp);
    let correlation = quantile(&correlations, 0.5);

    let mut warnings = Vec::new();
    let failed = if residual_px.p95 > params.fail_p95 {
        if params.force {
            warnings.push(format!(
                "alignment residual p95 {:.3} px exceeds the {:.3} px failure threshold; \
                 continuing because force is set",
                residual_px.p95, params.fail_p95
            ));
            false
        } else {
            warnings.push(format!(
                "alignment residual p95 {:.3} px exceeds the {:.3} px failure threshold; \
                 pass --force to continue",
                residual_px.p95, params.fail_p95
            ));
            true
        }
    } else {
        if residual_px.p95 > params.warn_p95 {
            warnings.push(format!(
                "alignment residual p95 {:.3} px exceeds the {:.3} px warning threshold",
                residual_px.p95, params.warn_p95
            ));
        }
        false
    };

    if reliable.is_empty() {
        warnings.push("alignment diagnostics found no reliable tiles".to_string());
    } else if correlation < MIN_CORRELATION {
        warnings.push(format!(
            "alignment tile correlation {correlation:.3} is below {MIN_CORRELATION:.3}"
        ));
    }

    Ok(AlignmentDiagnostics {
        tiles,
        residual_px,
        correlation,
        warnings,
        failed,
    })
}

#[allow(clippy::too_many_arguments)]
fn diagnose_tile(
    source: &[f32],
    reference: &[f32],
    width: usize,
    x: usize,
    y: usize,
    tile_width: usize,
    tile_height: usize,
    full_res_scale: f64,
) -> Option<TileDiagnostic> {
    let mut source_tile = Vec::with_capacity(tile_width * tile_height);
    let mut reference_tile = Vec::with_capacity(tile_width * tile_height);
    for row in 0..tile_height {
        let start = (y + row) * width + x;
        source_tile.extend_from_slice(&source[start..start + tile_width]);
        reference_tile.extend_from_slice(&reference[start..start + tile_width]);
    }

    let correlation = phase_correlate(
        &source_tile,
        &reference_tile,
        tile_width as u32,
        tile_height as u32,
    )
    .ok()?;

    let residual_px = correlation.dx.hypot(correlation.dy) * full_res_scale;
    let ncc = shifted_correlation(
        &source_tile,
        &reference_tile,
        tile_width,
        tile_height,
        correlation.dx,
        correlation.dy,
    );

    Some(TileDiagnostic {
        x: x as u32,
        y: y as u32,
        width: tile_width as u32,
        height: tile_height as u32,
        residual_px,
        correlation: ncc,
        confidence: correlation.confidence,
    })
}

/// Pearson correlation of `reference(x, y)` against
/// `source(x + dx, y + dy)`, matching the phase-correlation convention.
fn shifted_correlation(
    source: &[f32],
    reference: &[f32],
    width: usize,
    height: usize,
    dx: f64,
    dy: f64,
) -> f64 {
    if !dx.is_finite() || !dy.is_finite() {
        return 0.0;
    }

    let mut count = 0usize;
    let mut sum_source = 0.0f64;
    let mut sum_reference = 0.0f64;
    let mut sum_source_sq = 0.0f64;
    let mut sum_reference_sq = 0.0f64;
    let mut sum_product = 0.0f64;

    for row in 0..height {
        let source_y = row as f64 + dy;
        if source_y < 0.0 || source_y > (height - 1) as f64 {
            continue;
        }
        for column in 0..width {
            let source_x = column as f64 + dx;
            if source_x < 0.0 || source_x > (width - 1) as f64 {
                continue;
            }
            let source_value = f64::from(bilinear(source, width, height, source_x, source_y));
            let reference_value = f64::from(reference[row * width + column]);
            count += 1;
            sum_source += source_value;
            sum_reference += reference_value;
            sum_source_sq += source_value * source_value;
            sum_reference_sq += reference_value * reference_value;
            sum_product += source_value * reference_value;
        }
    }

    if count < MIN_TILE_EDGE {
        return 0.0;
    }
    let count = count as f64;
    let covariance = sum_product - sum_source * sum_reference / count;
    let variance_source = sum_source_sq - sum_source * sum_source / count;
    let variance_reference = sum_reference_sq - sum_reference * sum_reference / count;
    let denominator = (variance_source * variance_reference).sqrt();
    if denominator <= f64::EPSILON {
        0.0
    } else {
        (covariance / denominator).clamp(-1.0, 1.0)
    }
}

fn bilinear(image: &[f32], width: usize, height: usize, x: f64, y: f64) -> f32 {
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;

    let top = image[y0 * width + x0] * (1.0 - fx) + image[y0 * width + x1] * fx;
    let bottom = image[y1 * width + x0] * (1.0 - fx) + image[y1 * width + x1] * fx;
    top * (1.0 - fy) + bottom * fy
}

/// Linear-interpolated quantile of an ascending slice.
fn quantile(sorted: &[f64], quantile: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let position = quantile.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    if lower == upper {
        sorted[lower]
    } else {
        let fraction = position - lower as f64;
        sorted[lower] + (sorted[upper] - sorted[lower]) * fraction
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SIZE: usize = 128;

    #[derive(Clone, Copy)]
    struct Mode {
        frequency_x: f64,
        frequency_y: f64,
        phase: f64,
        amplitude: f64,
    }

    /// Deterministic band-limited, periodic test scene (same construction as
    /// the phase-correlation tests). Sampling it at shifted coordinates is an
    /// exact translation of the underlying continuous signal.
    fn modes() -> Vec<Mode> {
        const BAND: i32 = 12;
        let mut state = 0x1234_5678u32;
        let mut next = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };

        let mut modes = Vec::new();
        for frequency_x in -BAND..=BAND {
            for frequency_y in -BAND..=BAND {
                if frequency_x == 0 && frequency_y == 0 {
                    continue;
                }
                let amplitude = 0.5 + 0.5 * f64::from(next() % 1_000) / 1_000.0;
                let phase = f64::from(next() % 10_000) / 10_000.0 * TAU;
                modes.push(Mode {
                    frequency_x: f64::from(frequency_x),
                    frequency_y: f64::from(frequency_y),
                    phase,
                    amplitude,
                });
            }
        }
        modes
    }

    fn scene(x: f64, y: f64, modes: &[Mode]) -> f32 {
        let mut value = 0.0f64;
        for mode in modes {
            let u = mode.frequency_x * x / SIZE as f64;
            let v = mode.frequency_y * y / SIZE as f64;
            value += mode.amplitude * (TAU * (u + v) + mode.phase).cos();
        }
        (value / (modes.len() as f64).sqrt()) as f32
    }

    /// `source(x + dx, y + dy) == reference(x, y)` for the returned frame.
    fn render(modes: &[Mode], dx: f64, dy: f64) -> Vec<f32> {
        let mut frame = Vec::with_capacity(SIZE * SIZE);
        for y in 0..SIZE {
            for x in 0..SIZE {
                frame.push(scene(x as f64 - dx, y as f64 - dy, modes));
            }
        }
        frame
    }

    #[test]
    fn known_synthetic_residual_is_reported() {
        let modes = modes();
        let reference = render(&modes, 0.0, 0.0);
        let source = render(&modes, 0.2, 0.0);
        let params = DiagnosticsParams::default();

        let result = diagnose_alignment(&source, &reference, SIZE as u32, SIZE as u32, &params)
            .expect("diagnostics");

        // Half-resolution diagnostics scale by two: 0.2 px -> 0.4 full-res px.
        assert!(
            (result.residual_px.median - 0.4).abs() < 0.03,
            "median {:.4}",
            result.residual_px.median
        );
        assert!(
            (result.residual_px.p95 - 0.4).abs() < 0.03,
            "p95 {:.4}",
            result.residual_px.p95
        );
        assert_eq!(result.tiles.len(), 1);
        assert!(result.correlation > 0.9, "NCC {:.4}", result.correlation);
        assert!(!result.failed);
    }

    #[test]
    fn p95_above_warning_threshold_warns() {
        let modes = modes();
        let reference = render(&modes, 0.0, 0.0);
        let source = render(&modes, 0.2, 0.0);
        let params = DiagnosticsParams::default();

        let result = diagnose_alignment(&source, &reference, SIZE as u32, SIZE as u32, &params)
            .expect("diagnostics");

        assert!(result.residual_px.p95 > 0.3);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("warning threshold"));
        assert!(!result.failed);
    }

    #[test]
    fn failure_threshold_aborts_unless_forced() {
        let modes = modes();
        let reference = render(&modes, 0.0, 0.0);
        let source = render(&modes, 0.6, 0.0);
        let mut params = DiagnosticsParams::default();

        let failed = diagnose_alignment(&source, &reference, SIZE as u32, SIZE as u32, &params)
            .expect("diagnostics");
        assert!(failed.residual_px.p95 > 1.0);
        assert!(failed.failed);
        assert!(failed.warnings[0].contains("failure threshold"));

        params.force = true;
        let forced = diagnose_alignment(&source, &reference, SIZE as u32, SIZE as u32, &params)
            .expect("diagnostics");
        assert!(!forced.failed);
        assert!(forced.warnings[0].contains("force is set"));
    }

    #[test]
    fn flat_images_have_no_reliable_tiles() {
        let source = vec![0.5f32; SIZE * SIZE];
        let reference = vec![0.5f32; SIZE * SIZE];
        let result = diagnose_alignment(
            &source,
            &reference,
            SIZE as u32,
            SIZE as u32,
            &DiagnosticsParams::default(),
        )
        .expect("diagnostics");

        assert!(result.residual_px.p95.abs() < f64::EPSILON);
        assert!(
            result
                .warnings
                .iter()
                .any(|warning| warning.contains("no reliable tiles"))
        );
    }

    #[test]
    fn quantile_interpolates() {
        assert_eq!(quantile(&[], 0.5), 0.0);
        assert_eq!(quantile(&[1.0, 2.0, 3.0], 0.5), 2.0);
        assert!((quantile(&[0.0, 1.0], 0.95) - 0.95).abs() < 1e-12);
    }

    #[test]
    fn invalid_inputs_are_rejected() {
        assert_eq!(
            diagnose_alignment(&[], &[], 0, 0, &DiagnosticsParams::default()),
            Err(DiagnosticsError::EmptyInput)
        );
        assert!(matches!(
            diagnose_alignment(&[0.0], &[0.0, 1.0], 1, 1, &DiagnosticsParams::default()),
            Err(DiagnosticsError::LengthMismatch { .. })
        ));
        let params = DiagnosticsParams {
            tile_size: 1,
            ..DiagnosticsParams::default()
        };
        assert!(matches!(
            diagnose_alignment(&[0.0], &[0.0], 1, 1, &params),
            Err(DiagnosticsError::InvalidParameters)
        ));
    }
}
