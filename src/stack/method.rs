//! Automatic engine selection.
//!
//! Implements `docs/stacking.md` §4.4. Auto runs DMap first and chooses DMap
//! alone when fewer than 40% of pixels are confidently textured; otherwise it
//! selects hybrid. Explicit methods always win and are reported as such.

use std::fmt;
use std::str::FromStr;

use crate::stack::hybrid::{
    DetailOptions, HaloOptions, HybridHalo, MaskOptions, hybrid_mask, stack_hybrid_halo_masked,
};

/// Auto selects DMap below this confident fraction (`docs/stacking.md` §4.4).
pub const AUTO_CONFIDENT_MIN_FRACTION: f32 = 0.40;

/// Stacking method.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Method {
    /// Pick DMap or hybrid from the DMap confidence statistics.
    #[default]
    Auto,
    /// Pyramid maximum contrast.
    Pmax,
    /// Depth map only.
    Dmap,
    /// DMap tone and chroma with masked PMax detail.
    Hybrid,
}

impl Method {
    /// Lowercase CLI/report spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Pmax => "pmax",
            Self::Dmap => "dmap",
            Self::Hybrid => "hybrid",
        }
    }

    /// Whether this request still needs the automatic selection step.
    #[must_use]
    pub const fn is_auto(self) -> bool {
        matches!(self, Self::Auto)
    }
}

impl fmt::Display for Method {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Error returned when a method name is not one of `auto|pmax|dmap|hybrid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseMethodError;

impl fmt::Display for ParseMethodError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected one of auto, pmax, dmap, hybrid")
    }
}

impl std::error::Error for ParseMethodError {}

impl FromStr for Method {
    type Err = ParseMethodError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "pmax" => Ok(Self::Pmax),
            "dmap" => Ok(Self::Dmap),
            "hybrid" => Ok(Self::Hybrid),
            _ => Err(ParseMethodError),
        }
    }
}

/// Why a method was chosen, for the JSON report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionReason {
    /// The caller named the method explicitly.
    Explicit,
    /// Auto found enough confident, textured pixels for hybrid detail.
    Textured,
    /// Auto found too few confident, textured pixels and kept DMap only.
    LowConfidence,
}

impl SelectionReason {
    /// Stable lowercase spelling for reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Textured => "textured",
            Self::LowConfidence => "low-confidence",
        }
    }
}

impl fmt::Display for SelectionReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Confidence and coverage statistics from the DMap threshold pass §3.2.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SelectionStats {
    /// Pixels with positive threshold confidence, as a fraction in `[0, 1]`.
    pub confident_fraction: f32,
    /// Pixels covered by at least one frame, as a fraction in `[0, 1]`.
    pub coverage_fraction: f32,
}

impl SelectionStats {
    /// Convert the percentage values stored in
    /// [`crate::stack::dmap::ContrastThreshold`] to fractions.
    #[must_use]
    pub fn from_percentages(confident_pct: f32, coverage_pct: f32) -> Self {
        Self {
            confident_fraction: sanitize_fraction(confident_pct / 100.0),
            coverage_fraction: sanitize_fraction(coverage_pct / 100.0),
        }
    }
}

/// Inputs for the masked hybrid pipeline.
#[derive(Debug, Clone, Copy)]
pub struct HybridInputs<'a> {
    /// DMap linear RGB.
    pub dmap: &'a [f32],
    /// PMax linear RGB.
    pub pmax: &'a [f32],
    /// Per-pixel margin confidence from the focus module.
    pub confidence: &'a [f32],
    /// Per-pixel winning energy `Emax` from the DMap pass.
    pub best_energy: &'a [f32],
    /// DMap luma guide `Y_D`.
    pub guide: &'a [f32],
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// DMap contrast threshold `t`.
    pub threshold: f32,
}

/// Hybrid parameters grouped so method dispatch can pass them through.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct HybridParameters {
    /// Detail transfer and soft-clamp parameters (§4.1).
    pub detail: DetailOptions,
    /// Halo suppression parameters (§4.2).
    pub halo: HaloOptions,
    /// Confidence/texture mask parameters (§4.3).
    pub mask: MaskOptions,
}

impl HybridParameters {
    /// Build the §4.3 mask with these parameters.
    ///
    /// # Panics
    ///
    /// Panics if dimensions are zero or a buffer has the wrong length.
    #[must_use]
    pub fn compute_mask(
        self,
        confidence: &[f32],
        best_energy: &[f32],
        guide: &[f32],
        width: u32,
        height: u32,
        threshold: f32,
    ) -> Vec<f32> {
        hybrid_mask(
            confidence,
            best_energy,
            guide,
            width,
            height,
            threshold,
            self.mask,
        )
    }

    /// Build the mask and run the masked hybrid pipeline with these parameters.
    ///
    /// # Panics
    ///
    /// Panics if dimensions are zero or a buffer has the wrong length.
    #[must_use]
    pub fn stack(self, inputs: HybridInputs<'_>) -> HybridHalo {
        let mask = self.compute_mask(
            inputs.confidence,
            inputs.best_energy,
            inputs.guide,
            inputs.width,
            inputs.height,
            inputs.threshold,
        );
        stack_hybrid_halo_masked(
            inputs.dmap,
            inputs.pmax,
            &mask,
            inputs.width,
            inputs.height,
            self.detail,
            self.halo,
        )
    }
}

/// Options for automatic selection and hybrid execution.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MethodOptions {
    /// Requested method; [`Method::Auto`] enables the §4.4 decision.
    pub method: Method,
    /// Auto selects DMap below this confident fraction.
    pub auto_confident_min_fraction: f32,
    /// Parameters forwarded to the hybrid engine.
    pub hybrid: HybridParameters,
}

impl Default for MethodOptions {
    fn default() -> Self {
        Self {
            method: Method::Auto,
            auto_confident_min_fraction: AUTO_CONFIDENT_MIN_FRACTION,
            hybrid: HybridParameters::default(),
        }
    }
}

impl MethodOptions {
    /// Select the method for the given DMap statistics.
    #[must_use]
    pub fn select(self, stats: SelectionStats) -> MethodSelection {
        select_method_with(self, stats)
    }
}

/// Chosen method and the reason reported to the user.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MethodSelection {
    /// Method to execute; never [`Method::Auto`].
    pub method: Method,
    /// Why this method was chosen.
    pub reason: SelectionReason,
    /// Confident fraction used by the auto decision.
    pub confident_fraction: f32,
    /// Covered fraction recorded alongside the choice.
    pub coverage_fraction: f32,
    /// Confident fraction below which auto selects DMap.
    pub auto_confident_min_fraction: f32,
}

impl MethodSelection {
    /// Whether the selected method needs the hybrid engine.
    #[must_use]
    pub const fn is_hybrid(self) -> bool {
        matches!(self.method, Method::Hybrid)
    }
}

/// Select a method using the default auto threshold.
///
/// `confident_fraction` is the DMap `confident_pct` from §3.2 divided by 100.
/// Explicit methods always win; [`Method::Auto`] selects DMap below
/// [`AUTO_CONFIDENT_MIN_FRACTION`] and hybrid otherwise.
#[must_use]
pub fn select_method(method: Method, confident_fraction: f32) -> MethodSelection {
    select_method_with(
        MethodOptions {
            method,
            ..MethodOptions::default()
        },
        SelectionStats {
            confident_fraction,
            coverage_fraction: 0.0,
        },
    )
}

/// Select a method with explicit options and recorded coverage.
#[must_use]
pub fn select_method_with(options: MethodOptions, stats: SelectionStats) -> MethodSelection {
    let confident_fraction = sanitize_fraction(stats.confident_fraction);
    let coverage_fraction = sanitize_fraction(stats.coverage_fraction);
    let auto_confident_min_fraction = sanitize_fraction(options.auto_confident_min_fraction);

    let (method, reason) = match options.method {
        Method::Auto if confident_fraction < auto_confident_min_fraction => {
            (Method::Dmap, SelectionReason::LowConfidence)
        }
        Method::Auto => (Method::Hybrid, SelectionReason::Textured),
        explicit => (explicit, SelectionReason::Explicit),
    };

    MethodSelection {
        method,
        reason,
        confident_fraction,
        coverage_fraction,
        auto_confident_min_fraction,
    }
}

fn sanitize_fraction(value: f32) -> f32 {
    if value.is_finite() {
        value.clamp(0.0, 1.0)
    } else {
        0.0
    }
}
