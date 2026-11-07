//! JSON run report.
//!
//! Implements the report from `docs/outputs.md` §4. The writer is hand-rolled
//! so the crate needs no JSON dependency; field order is stable so reports
//! stay byte-identical between runs. Timings and timestamps are optional:
//! with both off (the default), the same inputs and parameters produce the
//! same bytes.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::Similarity;
use crate::align::diagnostics::{AlignmentDiagnostics, ResidualStats};

/// Report schema version written by this module.
pub const SCHEMA_VERSION: u32 = 1;

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

impl Default for AlignmentReport {
    fn default() -> Self {
        Self {
            reference_index: 0,
            frames: Vec::new(),
            residual_px: ResidualStats {
                median: 0.0,
                p95: 0.0,
            },
            warnings: Vec::new(),
        }
    }
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
        render(&self.json_value())
    }

    fn json_value(&self) -> JsonValue {
        let transforms = self
            .frames
            .iter()
            .map(|frame| {
                JsonValue::Object(vec![
                    ("a", JsonValue::Float64(frame.transform.a)),
                    ("b", JsonValue::Float64(frame.transform.b)),
                    ("tx", JsonValue::Float64(frame.transform.tx)),
                    ("ty", JsonValue::Float64(frame.transform.ty)),
                ])
            })
            .collect();
        let correlation = self
            .frames
            .iter()
            .map(|frame| JsonValue::Float64(frame.correlation))
            .collect();
        let peak_confidence = self
            .frames
            .iter()
            .map(|frame| JsonValue::Float64(frame.peak_confidence))
            .collect();

        JsonValue::Object(vec![
            (
                "reference_index",
                JsonValue::Unsigned(self.reference_index as u64),
            ),
            ("transforms", JsonValue::Array(transforms)),
            (
                "residual_px",
                JsonValue::Object(vec![
                    ("median", JsonValue::Float64(self.residual_px.median)),
                    ("p95", JsonValue::Float64(self.residual_px.p95)),
                ]),
            ),
            ("correlation", JsonValue::Array(correlation)),
            ("peak_confidence", JsonValue::Array(peak_confidence)),
            (
                "warnings",
                JsonValue::Array(
                    self.warnings
                        .iter()
                        .map(|warning| JsonValue::String(warning.clone()))
                        .collect(),
                ),
            ),
        ])
    }
}

/// Tool identity recorded at the top of the report.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolInfo {
    /// Executable name.
    pub name: String,
    /// Crate version.
    pub version: String,
    /// Source revision, when the build provides one.
    pub git: Option<String>,
}

impl Default for ToolInfo {
    fn default() -> Self {
        Self {
            name: "focus-stack".to_string(),
            version: crate::VERSION.to_string(),
            git: None,
        }
    }
}

impl ToolInfo {
    /// Build a tool record with the given name and version.
    #[must_use]
    pub fn new(name: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            version: version.into(),
            git: None,
        }
    }

    /// Record the source revision.
    #[must_use]
    pub fn with_git(mut self, git: impl Into<String>) -> Self {
        self.git = Some(git.into());
        self
    }

    fn json_value(&self) -> JsonValue {
        JsonValue::Object(vec![
            ("name", JsonValue::String(self.name.clone())),
            ("version", JsonValue::String(self.version.clone())),
            (
                "git",
                self.git
                    .as_ref()
                    .map_or(JsonValue::Null, |git| JsonValue::String(git.clone())),
            ),
        ])
    }
}

/// One input frame in the report.
#[derive(Debug, Clone, PartialEq)]
pub struct InputReport {
    /// Path exactly as given on the command line.
    pub path: String,
    /// Lowercase hex SHA-256 of the file bytes.
    pub sha256: String,
    /// Pixel width.
    pub width: u32,
    /// Pixel height.
    pub height: u32,
    /// Source bit depth.
    pub bits: u8,
    /// EXIF orientation tag.
    pub orientation: u16,
    /// ICC profile description, when present.
    pub profile: Option<String>,
}

impl InputReport {
    /// Build an input record.
    #[must_use]
    pub fn new(
        path: impl Into<String>,
        sha256: impl Into<String>,
        width: u32,
        height: u32,
        bits: u8,
        orientation: u16,
        profile: Option<String>,
    ) -> Self {
        Self {
            path: path.into(),
            sha256: sha256.into(),
            width,
            height,
            bits,
            orientation,
            profile,
        }
    }

    fn json_value(&self) -> JsonValue {
        JsonValue::Object(vec![
            ("path", JsonValue::String(self.path.clone())),
            ("sha256", JsonValue::String(self.sha256.clone())),
            ("width", JsonValue::Unsigned(u64::from(self.width))),
            ("height", JsonValue::Unsigned(u64::from(self.height))),
            ("bits", JsonValue::Unsigned(u64::from(self.bits))),
            (
                "orientation",
                JsonValue::Unsigned(u64::from(self.orientation)),
            ),
            (
                "profile",
                self.profile.as_ref().map_or(JsonValue::Null, |profile| {
                    JsonValue::String(profile.clone())
                }),
            ),
        ])
    }
}

/// Effective parameters of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamsReport {
    /// Requested stacking method.
    pub method: String,
    /// Alignment model.
    pub align: String,
    /// Reference selector: `auto` or a zero-based index.
    pub reference: String,
    /// Explicit focus estimation radius, if any.
    pub estimation_radius: Option<u32>,
    /// DMap confidence override, if any.
    pub keep_detail: Option<f32>,
    /// Hybrid detail transfer strength.
    pub detail_transfer: f32,
    /// Exposure matching mode.
    pub exposure_match: String,
    /// TIFF compression.
    pub compression: String,
    /// Output ICC profile policy.
    pub output_profile: String,
    /// Deterministic seed.
    pub seed: u64,
    /// Explicit worker thread count; `None` means the default pool.
    pub threads: Option<usize>,
}

impl Default for ParamsReport {
    fn default() -> Self {
        Self {
            method: "auto".to_string(),
            align: "similarity".to_string(),
            reference: "auto".to_string(),
            estimation_radius: None,
            keep_detail: None,
            detail_transfer: 0.7,
            exposure_match: "off".to_string(),
            compression: "lzw".to_string(),
            output_profile: "same".to_string(),
            seed: 0,
            threads: None,
        }
    }
}

impl ParamsReport {
    fn json_value(&self) -> JsonValue {
        JsonValue::Object(vec![
            ("method", JsonValue::String(self.method.clone())),
            ("align", JsonValue::String(self.align.clone())),
            ("reference", JsonValue::String(self.reference.clone())),
            (
                "estimation_radius",
                self.estimation_radius.map_or(JsonValue::Null, |radius| {
                    JsonValue::Unsigned(u64::from(radius))
                }),
            ),
            (
                "keep_detail",
                self.keep_detail.map_or(JsonValue::Null, JsonValue::Float32),
            ),
            ("detail_transfer", JsonValue::Float32(self.detail_transfer)),
            (
                "exposure_match",
                JsonValue::String(self.exposure_match.clone()),
            ),
            ("compression", JsonValue::String(self.compression.clone())),
            (
                "output_profile",
                JsonValue::String(self.output_profile.clone()),
            ),
            ("seed", JsonValue::Unsigned(self.seed)),
            (
                "threads",
                self.threads.map_or(JsonValue::Null, |threads| {
                    JsonValue::Unsigned(threads as u64)
                }),
            ),
        ])
    }
}

/// Bounding box of a coverage gap region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoverageBox {
    /// Left edge, inclusive.
    pub x: u32,
    /// Top edge, inclusive.
    pub y: u32,
    /// Box width in pixels.
    pub width: u32,
    /// Box height in pixels.
    pub height: u32,
}

impl CoverageBox {
    /// Build a coverage box.
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn json_value(self) -> JsonValue {
        JsonValue::Object(vec![
            ("x", JsonValue::Unsigned(u64::from(self.x))),
            ("y", JsonValue::Unsigned(u64::from(self.y))),
            ("width", JsonValue::Unsigned(u64::from(self.width))),
            ("height", JsonValue::Unsigned(u64::from(self.height))),
        ])
    }
}

/// The `focus` object of the JSON run report.
#[derive(Debug, Clone, PartialEq)]
pub struct FocusReport {
    /// Number of pyramid levels.
    pub levels: usize,
    /// Per-frame per-level noise sigma, finest level first.
    pub noise_sigma: Vec<Vec<f32>>,
    /// Contrast threshold used by DMap.
    pub threshold: f32,
    /// Confident, textured fraction in `0..1`.
    pub confident_pct: f32,
    /// Fraction of pixels with no confidently sharp frame.
    pub coverage_gap_pct: f32,
    /// Bounding boxes of coverage gap regions.
    pub coverage_boxes: Vec<CoverageBox>,
    /// Advisory focus warnings.
    pub warnings: Vec<String>,
}

impl Default for FocusReport {
    fn default() -> Self {
        Self {
            levels: 0,
            noise_sigma: Vec::new(),
            threshold: 0.0,
            confident_pct: 0.0,
            coverage_gap_pct: 0.0,
            coverage_boxes: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

impl FocusReport {
    fn json_value(&self) -> JsonValue {
        let noise_sigma = self
            .noise_sigma
            .iter()
            .map(|frame| JsonValue::Array(frame.iter().copied().map(JsonValue::Float32).collect()))
            .collect();

        JsonValue::Object(vec![
            ("levels", JsonValue::Unsigned(self.levels as u64)),
            ("noise_sigma", JsonValue::Array(noise_sigma)),
            ("threshold", JsonValue::Float32(self.threshold)),
            ("confident_pct", JsonValue::Float32(self.confident_pct)),
            (
                "coverage_gap_pct",
                JsonValue::Float32(self.coverage_gap_pct),
            ),
            (
                "coverage_boxes",
                JsonValue::Array(
                    self.coverage_boxes
                        .iter()
                        .copied()
                        .map(CoverageBox::json_value)
                        .collect(),
                ),
            ),
            (
                "warnings",
                JsonValue::Array(
                    self.warnings
                        .iter()
                        .map(|warning| JsonValue::String(warning.clone()))
                        .collect(),
                ),
            ),
        ])
    }
}

/// The `method` object of the JSON run report.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodReport {
    /// Selected engine.
    pub chosen: String,
    /// Human-readable selection reason.
    pub reason: String,
}

impl Default for MethodReport {
    fn default() -> Self {
        Self {
            chosen: "dmap".to_string(),
            reason: String::new(),
        }
    }
}

impl MethodReport {
    fn json_value(&self) -> JsonValue {
        JsonValue::Object(vec![
            ("chosen", JsonValue::String(self.chosen.clone())),
            ("reason", JsonValue::String(self.reason.clone())),
        ])
    }
}

/// The `output` object of the JSON run report.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputReport {
    /// Path exactly as given on the command line.
    pub path: String,
    /// Lowercase hex SHA-256 of the output file bytes.
    pub sha256: String,
    /// Fraction of output pixels clipped, in `0..1`.
    pub clipped_pct: f64,
    /// Output ICC profile description, when present.
    pub profile: Option<String>,
}

impl Default for OutputReport {
    fn default() -> Self {
        Self {
            path: String::new(),
            sha256: String::new(),
            clipped_pct: 0.0,
            profile: None,
        }
    }
}

impl OutputReport {
    fn json_value(&self) -> JsonValue {
        JsonValue::Object(vec![
            ("path", JsonValue::String(self.path.clone())),
            ("sha256", JsonValue::String(self.sha256.clone())),
            ("clipped_pct", JsonValue::Float64(self.clipped_pct)),
            (
                "profile",
                self.profile.as_ref().map_or(JsonValue::Null, |profile| {
                    JsonValue::String(profile.clone())
                }),
            ),
        ])
    }
}

/// Stage timings in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TimingReport {
    /// Frame loading and decoding.
    pub load: u64,
    /// Alignment.
    pub align: u64,
    /// Focus measurement.
    pub focus: u64,
    /// Stacking.
    pub stack: u64,
    /// Encoding and writing.
    pub encode: u64,
}

impl TimingReport {
    fn json_value(self) -> JsonValue {
        JsonValue::Object(vec![
            ("load", JsonValue::Unsigned(self.load)),
            ("align", JsonValue::Unsigned(self.align)),
            ("focus", JsonValue::Unsigned(self.focus)),
            ("stack", JsonValue::Unsigned(self.stack)),
            ("encode", JsonValue::Unsigned(self.encode)),
        ])
    }
}

/// The complete JSON run report from `docs/outputs.md` §4.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    /// Report schema version.
    pub schema_version: u32,
    /// Tool identity.
    pub tool: ToolInfo,
    /// Inputs in command-line order.
    pub inputs: Vec<InputReport>,
    /// Effective parameters.
    pub params: ParamsReport,
    /// Alignment diagnostics.
    pub alignment: AlignmentReport,
    /// Focus diagnostics.
    pub focus: FocusReport,
    /// Method selection.
    pub method: MethodReport,
    /// Output image identity.
    pub output: OutputReport,
    /// Stage timings; only present with `--timings`.
    pub timing_ms: Option<TimingReport>,
    /// Creation timestamp; only present with `--stamp`.
    pub created_at: Option<String>,
    /// Run-level warnings not belonging to a stage.
    pub warnings: Vec<String>,
}

impl Default for Report {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tool: ToolInfo::default(),
            inputs: Vec::new(),
            params: ParamsReport::default(),
            alignment: AlignmentReport::default(),
            focus: FocusReport::default(),
            method: MethodReport::default(),
            output: OutputReport::default(),
            timing_ms: None,
            created_at: None,
            warnings: Vec::new(),
        }
    }
}

impl Report {
    /// Top-level warnings: the union of run-level, alignment, and focus
    /// warnings, in first-seen order.
    #[must_use]
    pub fn warnings(&self) -> Vec<String> {
        let mut all = Vec::new();
        for warning in self
            .warnings
            .iter()
            .chain(self.alignment.warnings.iter())
            .chain(self.focus.warnings.iter())
        {
            if !all.contains(warning) {
                all.push(warning.clone());
            }
        }
        all
    }

    /// Serialize the complete report.
    ///
    /// The output is deterministic when `timing_ms` and `created_at` are
    /// `None`: field order is fixed and no hash maps are involved.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut fields = vec![
            (
                "schema_version",
                JsonValue::Unsigned(u64::from(self.schema_version)),
            ),
            ("tool", self.tool.json_value()),
            (
                "inputs",
                JsonValue::Array(self.inputs.iter().map(InputReport::json_value).collect()),
            ),
            ("params", self.params.json_value()),
            ("alignment", self.alignment.json_value()),
            ("focus", self.focus.json_value()),
            ("method", self.method.json_value()),
            ("output", self.output.json_value()),
        ];
        if let Some(timing) = self.timing_ms {
            fields.push(("timing_ms", timing.json_value()));
        }
        if let Some(created_at) = &self.created_at {
            fields.push(("created_at", JsonValue::String(created_at.clone())));
        }
        fields.push((
            "warnings",
            JsonValue::Array(self.warnings().into_iter().map(JsonValue::String).collect()),
        ));

        render(&JsonValue::Object(fields))
    }
}

/// SHA-256 of an in-memory buffer, as lowercase hex.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_digest(&hasher.finalize())
}

/// Human-readable ICC profile description, or `None` when absent or malformed.
///
/// Reports use the embedded description verbatim (`docs/outputs.md` §4).
#[must_use]
pub fn icc_description(icc: Option<&[u8]>) -> Option<String> {
    let profile = lcms2::Profile::new_icc(icc?).ok()?;
    profile.info(lcms2::InfoType::Description, lcms2::Locale::none())
}

/// Current UTC time as an ISO 8601 timestamp with second precision.
///
/// Only used when `--stamp` is given; the default report has no timestamp so
/// runs stay byte-identical (`docs/outputs.md` §7).
#[must_use]
pub fn utc_timestamp_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let days = (seconds / 86_400) as i64;
    let remaining = seconds % 86_400;
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remaining / 3_600,
        (remaining % 3_600) / 60,
        remaining % 60
    )
}

/// Convert days since 1970-01-01 to a proleptic Gregorian date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
}

/// SHA-256 of a file's bytes, as lowercase hex.
///
/// Inputs and outputs are identified by the hash of the file bytes
/// (`docs/outputs.md` §4). The file is read in chunks so large TIFFs do not
/// need a second full-size buffer.
pub fn sha256_file(path: impl AsRef<Path>) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_digest(&hasher.finalize()))
}

fn hex_digest(digest: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    for byte in digest {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    length: u64,
}

impl Sha256 {
    fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
                0x5be0cd19,
            ],
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);

        if self.buffered > 0 {
            let take = (64 - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];
            if self.buffered == 64 {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }

        while data.len() >= 64 {
            let mut block = [0u8; 64];
            block.copy_from_slice(&data[..64]);
            self.compress(&block);
            data = &data[64..];
        }

        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    fn finalize(mut self) -> [u8; 32] {
        let bit_length = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buffered != 56 {
            self.update(&[0]);
        }
        self.update(&bit_length.to_be_bytes());

        let mut digest = [0u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut words = [0u32; 64];
        for (index, word) in words.iter_mut().take(16).enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                block[offset],
                block[offset + 1],
                block[offset + 2],
                block[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(SHA256_K[index])
                .wrapping_add(words[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

enum JsonValue {
    Null,
    Unsigned(u64),
    Float32(f32),
    Float64(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(&'static str, JsonValue)>),
}

fn render(value: &JsonValue) -> String {
    let mut out = String::new();
    write_json(value, &mut out, 0);
    out
}

fn write_json(value: &JsonValue, out: &mut String, indent: usize) {
    match value {
        JsonValue::Null => out.push_str("null"),
        JsonValue::Unsigned(number) => {
            let _ = write!(out, "{number}");
        }
        JsonValue::Float32(number) => out.push_str(&finite32(*number)),
        JsonValue::Float64(number) => out.push_str(&finite64(*number)),
        JsonValue::String(string) => out.push_str(&json_string(string)),
        JsonValue::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push_str("[\n");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, indent + 2);
                write_json(item, out, indent + 2);
            }
            out.push('\n');
            push_indent(out, indent);
            out.push(']');
        }
        JsonValue::Object(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{\n");
            for (index, (key, field)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push_str(",\n");
                }
                push_indent(out, indent + 2);
                out.push_str(&json_string(key));
                out.push_str(": ");
                write_json(field, out, indent + 2);
            }
            out.push('\n');
            push_indent(out, indent);
            out.push('}');
        }
    }
}

fn push_indent(out: &mut String, count: usize) {
    for _ in 0..count {
        out.push(' ');
    }
}

fn finite32(value: f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "null".to_string()
    }
}

fn finite64(value: f64) -> String {
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
        assert!(json.contains("\"median\": 0.04"));
        assert!(json.contains("\"p95\": 0.11"));
        assert!(json.contains("\"correlation\": ["));
        assert!(json.contains("0.998"));
        assert!(json.contains("\"peak_confidence\": ["));
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
        assert!(json.contains("\"correlation\": ["));
        assert!(json.contains("null"));
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

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn report_omits_optional_sections_by_default() {
        let report = Report::default();
        let json = report.to_json();
        assert_eq!(report.schema_version, SCHEMA_VERSION);
        assert!(!json.contains("timing_ms"));
        assert!(!json.contains("created_at"));
    }
}
