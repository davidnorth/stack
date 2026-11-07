//! Command-line definitions, validation, and exit codes.

use std::path::PathBuf;
use std::str::FromStr;

use clap::{Parser, ValueEnum};

/// Focus stacking for macOS Apple Silicon.
#[derive(Debug, Clone, Parser)]
#[command(
    name = "focus-stack",
    version = crate::VERSION,
    about = "Professional focus stacking for macOS Apple Silicon"
)]
pub struct Cli {
    /// Input TIFF frames; one performs an identity pass, two or more stack.
    #[arg(value_name = "INPUT", required = true, num_args = 1..)]
    pub inputs: Vec<PathBuf>,

    /// Main TIFF output path.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: PathBuf,

    // Core
    /// Stacking method; auto selects hybrid unless confidence is low.
    #[arg(long, value_enum, default_value_t = Method::Auto)]
    pub method: Method,
    /// Alignment model; auto selects similarity.
    #[arg(long, value_enum, default_value_t = Align::Similarity)]
    pub align: Align,
    /// Reference frame: auto (max gradient energy) or a zero-based index.
    #[arg(long, value_name = "auto|INDEX", default_value = "auto", value_parser = parse_reference)]
    pub reference: Reference,
    /// Maximum rotation in degrees during alignment.
    #[arg(long, value_name = "DEG", default_value_t = 3.0)]
    pub max_rotation: f64,
    /// Focus-breathing scale clamp as MIN,MAX.
    #[arg(
        long,
        value_name = "MIN,MAX",
        default_value = "0.97,1.03",
        value_parser = parse_scale_range
    )]
    pub scale_range: ScaleRange,
    /// Alignment residual fail threshold in pixels; warns above 0.3.
    #[arg(long, value_name = "PX", default_value_t = 1.0)]
    pub max_residual: f64,
    /// Continue after an alignment failure.
    #[arg(long)]
    pub force: bool,
    /// Disable cropping and edge-clamp instead.
    #[arg(long)]
    pub keep_borders: bool,

    // Focus and stacking
    /// Focus estimation radius in pixels; auto uses 12-32.
    #[arg(long, value_name = "PX")]
    pub estimation_radius: Option<u32>,
    /// DMap confidence override, in percent.
    #[arg(long, value_name = "PERCENT")]
    pub keep_detail: Option<f32>,
    /// Hybrid PMax detail strength.
    #[arg(long, value_name = "FLOAT", default_value_t = 0.7)]
    pub detail_transfer: f32,
    /// Exposure normalisation applied before measuring focus.
    #[arg(long, value_enum, default_value_t = ExposureMatch::Off)]
    pub exposure_match: ExposureMatch,
    /// PMax beta (debug).
    #[arg(long, value_name = "FLOAT", default_value_t = 6.0)]
    pub pmax_beta: f32,
    /// DMap lambda (debug).
    #[arg(long, value_name = "FLOAT", default_value_t = 1.0)]
    pub dmap_lambda: f32,
    /// Normalise local contrast (debug).
    #[arg(long)]
    pub contrast_normalize: bool,

    // Outputs
    /// Write a 16-bit grayscale depth map to this path.
    #[arg(long, value_name = "PATH")]
    pub depth_map: Option<PathBuf>,
    /// Alias for the depth map output in v1.
    #[arg(long, value_name = "PATH")]
    pub source_map: Option<PathBuf>,
    /// Write a JSON quality report to this path.
    #[arg(long, value_name = "PATH")]
    pub report: Option<PathBuf>,
    /// Write an 8-bit sRGB JPEG preview to this path.
    #[arg(long, value_name = "PATH")]
    pub preview: Option<PathBuf>,
    /// Preview long edge in pixels.
    #[arg(long, value_name = "PX", default_value_t = 2048)]
    pub preview_size: u32,
    /// Output TIFF compression.
    #[arg(long, value_enum, default_value_t = Compression::Lzw)]
    pub compression: Compression,
    /// Output TIFF bit depth.
    #[arg(long, value_enum, default_value_t = Bits::Sixteen)]
    pub bits: Bits,
    /// Output colour profile.
    #[arg(long, value_enum, default_value_t = OutputProfile::Same)]
    pub output_profile: OutputProfile,
    /// ICC profile to assume for untagged or unknown inputs.
    #[arg(long, value_name = "PATH")]
    pub assume_profile: Option<PathBuf>,
    /// Accept inputs with different colour profiles.
    #[arg(long)]
    pub allow_profile_mismatch: bool,
    /// Keep only ICC, orientation, resolution, and the software tag.
    #[arg(long)]
    pub strip_metadata: bool,
    /// Add a timestamp to the report (breaks determinism).
    #[arg(long)]
    pub stamp: bool,
    /// Add stage timings to the report (breaks determinism).
    #[arg(long)]
    pub timings: bool,

    // Runtime
    /// Worker thread count; defaults to all cores.
    #[arg(long, value_name = "N", value_parser = parse_threads)]
    pub threads: Option<usize>,
    /// Memory budget such as 4G or 4096M; defaults to unlimited.
    #[arg(long, value_name = "SIZE", value_parser = parse_memory_size)]
    pub max_memory: Option<MemorySize>,
    /// Seed for synthetic and retouch determinism.
    #[arg(long, value_name = "N", default_value_t = 0)]
    pub seed: u64,
    /// Turn warnings into exit code 5.
    #[arg(long)]
    pub strict: bool,
    /// Emit debug logging.
    #[arg(short = 'v', long)]
    pub verbose: bool,
    /// Disable progress output.
    #[arg(short = 'q', long)]
    pub quiet: bool,
}

/// Stacking method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Method {
    Auto,
    Pmax,
    Dmap,
    Hybrid,
}

/// Alignment model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Align {
    Auto,
    Translation,
    Similarity,
    Off,
}

/// Exposure normalisation mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExposureMatch {
    Off,
    Luma,
    Rgb,
}

/// Output TIFF compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Compression {
    None,
    Lzw,
    Deflate,
}

/// Output TIFF bit depth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Bits {
    #[value(name = "8")]
    Eight,
    #[value(name = "16")]
    Sixteen,
}

impl Bits {
    /// Numeric bit depth used by the TIFF writer.
    #[must_use]
    pub const fn value(self) -> u8 {
        match self {
            Self::Eight => 8,
            Self::Sixteen => 16,
        }
    }
}

/// Output colour profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OutputProfile {
    Same,
    Srgb,
    Linear,
}

/// Reference-frame selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reference {
    /// Select the frame with maximum gradient energy.
    Auto,
    /// Zero-based input index.
    Index(usize),
}

/// Focus-breathing scale clamp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScaleRange {
    pub min: f64,
    pub max: f64,
}

/// Parsed memory budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemorySize {
    bytes: u64,
}

impl MemorySize {
    /// Budget in bytes.
    #[must_use]
    pub const fn bytes(self) -> u64 {
        self.bytes
    }
}

fn parse_reference(value: &str) -> Result<Reference, String> {
    if value == "auto" {
        return Ok(Reference::Auto);
    }
    value
        .parse::<usize>()
        .map(Reference::Index)
        .map_err(|_| format!("expected 'auto' or a zero-based index, got '{value}'"))
}

fn parse_scale_range(value: &str) -> Result<ScaleRange, String> {
    let (min, max) = value
        .split_once(',')
        .ok_or_else(|| format!("expected MIN,MAX, got '{value}'"))?;
    let min = f64::from_str(min).map_err(|_| format!("invalid minimum scale '{min}'"))?;
    let max = f64::from_str(max).map_err(|_| format!("invalid maximum scale '{max}'"))?;
    if !min.is_finite() || !max.is_finite() || min <= 0.0 || max < min {
        return Err(format!(
            "scale range must satisfy 0 < MIN <= MAX, got '{value}'"
        ));
    }
    Ok(ScaleRange { min, max })
}

fn parse_threads(value: &str) -> Result<usize, String> {
    let threads = value
        .parse::<usize>()
        .map_err(|_| format!("invalid thread count '{value}'"))?;
    if threads == 0 {
        return Err("thread count must be at least 1".to_string());
    }
    Ok(threads)
}

fn parse_memory_size(value: &str) -> Result<MemorySize, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err("memory size is empty".to_string());
    }
    let (digits, multiplier) = match trimmed.as_bytes().last().copied() {
        Some(b'k' | b'K') => (&trimmed[..trimmed.len() - 1], 1024_u64),
        Some(b'm' | b'M') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(2)),
        Some(b'g' | b'G') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(3)),
        Some(b't' | b'T') => (&trimmed[..trimmed.len() - 1], 1024_u64.pow(4)),
        _ => (trimmed, 1),
    };
    let amount = digits
        .parse::<u64>()
        .map_err(|_| format!("invalid memory size '{value}'"))?;
    let bytes = amount
        .checked_mul(multiplier)
        .ok_or_else(|| format!("memory size '{value}' is too large"))?;
    Ok(MemorySize { bytes })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_reference() {
        assert_eq!(parse_reference("auto").unwrap(), Reference::Auto);
        assert_eq!(parse_reference("0").unwrap(), Reference::Index(0));
        assert!(parse_reference("-1").is_err());
    }

    #[test]
    fn parses_scale_range() {
        assert_eq!(
            parse_scale_range("0.97,1.03").unwrap(),
            ScaleRange {
                min: 0.97,
                max: 1.03
            }
        );
        assert!(parse_scale_range("1.03,0.97").is_err());
    }

    #[test]
    fn parses_memory_size() {
        assert_eq!(parse_memory_size("4096M").unwrap().bytes(), 4096 << 20);
        assert_eq!(parse_memory_size("4G").unwrap().bytes(), 4 << 30);
        assert!(parse_memory_size("lots").is_err());
    }
}
