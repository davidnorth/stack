//! JSON run report.
//!
//! Implements the `alignment` object from `docs/outputs.md` §4. The writer is
//! hand-rolled so the crate needs no JSON dependency; field order is stable so
//! reports stay byte-identical between runs.

use std::fmt::Write as _;

use crate::Similarity;
use crate::align::diagnostics::{AlignmentDiagnostics, ResidualStats};

/// Alignment record for one frame.
#[derive(Debug, Clone, PartialEq)]
pub struct FrameAlignment {
    /// Transform mapping output coordinates to this frame's source coordinates.
    pub transform: Similarity,
    /// Phase-correlation peak confidence (`docs/alignment.md` §8).
    pub peak_confidence: f64,
    /// Final ECC normalized correlation (`docs/alignment.md` §8).
    pub correlation: f64,
}

impl FrameAlignment {
    /// Build one frame record.
    #[must_use]
    pub const fn new(transform: Similarity, peak_confidence: f64, correlation: f64) -> Self {
        Self {
            transform,
            peak_confidence,
            correlation,
        }
    }
}

/// The `alignment` object of the JSON run report.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignmentReport {
    /// Index of the reference frame.
    pub reference_index: usize,
    /// Per-frame transforms and correlations, in input order.
    pub frames: Vec<FrameAlignment>,
    /// Median and p95 per-tile residual.
    pub residual_px: ResidualStats,
    /// Advisory alignment warnings.
    pub warnings: Vec<String>,
}

impl AlignmentReport {
    /// Build a report from per-frame records and a diagnostics result.
    #[must_use]
    pub fn new(
        reference_index: usize,
        frames: Vec<FrameAlignment>,
        diagnostics: &AlignmentDiagnostics,
    ) -> Self {
        Self {
            reference_index,
            frames,
            residual_px: diagnostics.residual_px,
            warnings: diagnostics.warnings.clone(),
        }
    }

    /// Transforms in input order.
    #[must_use]
    pub fn transforms(&self) -> Vec<Similarity> {
        self.frames
            .iter()
            .map(|frame| frame.transform.clone())
            .collect()
    }

    /// ECC correlations in input order.
    #[must_use]
    pub fn correlations(&self) -> Vec<f64> {
        self.frames.iter().map(|frame| frame.correlation).collect()
    }

    /// Phase-correlation peak confidences in input order.
    #[must_use]
    pub fn peak_confidences(&self) -> Vec<f64> {
        self.frames
            .iter()
            .map(|frame| frame.peak_confidence)
            .collect()
    }

    /// Serialize the alignment object as a JSON value.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut json = String::new();
        json.push_str("{\n");
        let _ = writeln!(json, "  \"reference_index\": {},", self.reference_index);
        json.push_str("  \"transforms\": [");
        for (index, frame) in self.frames.iter().enumerate() {
            if index > 0 {
                json.push_str(", ");
            }
            let _ = write!(
                json,
                "{{ \"a\": {}, \"b\": {}, \"tx\": {}, \"ty\": {} }}",
                json_number(frame.transform.a),
                json_number(frame.transform.b),
                json_number(frame.transform.tx),
                json_number(frame.transform.ty)
            );
        }
        json.push_str("],\n");
        let _ = writeln!(
            json,
            "  \"residual_px\": {{ \"median\": {}, \"p95\": {} }},",
            json_number(self.residual_px.median),
            json_number(self.residual_px.p95)
        );
        json.push_str("  \"correlation\": [");
        for (index, frame) in self.frames.iter().enumerate() {
            if index > 0 {
                json.push_str(", ");
            }
            json.push_str(&json_number(frame.correlation));
        }
        json.push_str("],\n");
        json.push_str("  \"peak_confidence\": [");
        for (index, frame) in self.frames.iter().enumerate() {
            if index > 0 {
                json.push_str(", ");
            }
            json.push_str(&json_number(frame.peak_confidence));
        }
        json.push_str("],\n");
        json.push_str("  \"warnings\": [");
        for (index, warning) in self.warnings.iter().enumerate() {
            if index > 0 {
                json.push_str(", ");
            }
            json.push_str(&json_string(warning));
        }
        json.push_str("]\n");
        json.push('}');
        json
    }
}

fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::align::diagnostics::{AlignmentDiagnostics, DiagnosticsParams, diagnose_alignment};

    fn transform() -> Similarity {
        Similarity {
            a: 1.000_01,
            b: -0.000_02,
            tx: 0.42,
            ty: -1.13,
        }
    }

    fn diagnostics() -> AlignmentDiagnostics {
        AlignmentDiagnostics {
            tiles: Vec::new(),
            residual_px: ResidualStats {
                median: 0.04,
                p95: 0.11,
            },
            correlation: 0.997,
            warnings: Vec::new(),
            failed: false,
        }
    }

    #[test]
    fn alignment_fields_match_the_document() {
        let report = AlignmentReport::new(
            0,
            vec![
                FrameAlignment::new(transform(), 4.2, 1.0),
                FrameAlignment::new(transform(), 3.8, 0.998),
            ],
            &diagnostics(),
        );

        let json = report.to_json();
        assert!(json.contains("\"reference_index\": 0"));
        assert!(json.contains("\"transforms\": ["));
        assert!(json.contains("\"residual_px\": { \"median\": 0.04, \"p95\": 0.11 }"));
        assert!(json.contains("\"correlation\": [1, 0.998]"));
        assert!(json.contains("\"peak_confidence\": [4.2, 3.8]"));
        assert!(json.contains("\"warnings\": []"));
    }

    #[test]
    fn warnings_are_escaped_and_non_finite_numbers_become_null() {
        let mut diagnostics = diagnostics();
        diagnostics.warnings = vec!["residual p95 \"too high\"\n".to_string()];
        let mut frame = FrameAlignment::new(transform(), f64::NAN, f64::INFINITY);
        frame.transform.tx = f64::NAN;
        let report = AlignmentReport::new(0, vec![frame], &diagnostics);

        let json = report.to_json();
        assert!(json.contains("\"tx\": null"));
        assert!(json.contains("\"correlation\": [null]"));
        assert!(json.contains("\"peak_confidence\": [null]"));
        assert!(json.contains("residual p95 \\\"too high\\\"\\n"));
    }

    #[test]
    fn report_uses_diagnostics_residual_and_warnings() {
        let reference = vec![0.5f32; 128 * 128];
        let source = vec![0.5f32; 128 * 128];
        let diagnostics =
            diagnose_alignment(&source, &reference, 128, 128, &DiagnosticsParams::default())
                .expect("diagnostics");
        let report = AlignmentReport::new(0, Vec::new(), &diagnostics);

        assert_eq!(report.residual_px, diagnostics.residual_px);
        assert_eq!(report.warnings, diagnostics.warnings);
        assert!(report.to_json().contains("residual_px"));
    }
}
