//! Load -> align -> measure -> stack -> write orchestration.

use std::fmt;
use std::fmt::Write as _;
use std::io::{self, IsTerminal, Write};
use std::path::Path;

use crate::cli::{Cli, Compression, Method, Reference};
use crate::color::luma::encoded_luma_buffer;
use crate::color::transfer::srgb_decode;
use crate::focus::confidence::{
    ConfidenceMap, ConfidenceOptions, FrameCalibration, saturation_mask,
};
use crate::focus::energy::{EnergyMaps, EnergyOptions};
use crate::focus::noise::{self, DEFAULT_NOISE_K};
use crate::focus::pyramid::Pyramid;
use crate::io::metadata::MetadataPolicy;
use crate::io::tiff_read::read_tiff;
use crate::io::tiff_write::{self, WriteOptions};
use crate::stack::pmax::{self, WeightOptions};
use crate::{Frame, VERSION};

/// Pipeline failure with the exit code documented in `docs/cli.md` §5.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineError {
    /// Bad arguments that clap cannot catch.
    Usage(String),
    /// Missing, corrupt, unsupported, or mismatched input.
    Input(String),
    /// An output could not be written.
    Io(String),
    /// Unexpected internal failure.
    Internal(String),
}

impl PipelineError {
    /// Process exit code for this failure.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Input(_) | Self::Io(_) => 3,
            Self::Internal(_) => 1,
        }
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message)
            | Self::Input(message)
            | Self::Io(message)
            | Self::Internal(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for PipelineError {}

/// Runs the load -> copy -> write pipeline.
pub fn run(cli: &Cli) -> Result<(), PipelineError> {
    let progress = Progress::new(cli.quiet, cli.verbose);

    let mut frames = Vec::with_capacity(cli.inputs.len());
    for (index, path) in cli.inputs.iter().enumerate() {
        progress.step(format_args!(
            "loading {}/{} {}",
            index + 1,
            cli.inputs.len(),
            path.display()
        ));
        let frame = read_tiff(path).map_err(|err| PipelineError::Input(err.to_string()))?;
        frames.push(frame);
    }

    let first = frames
        .first()
        .ok_or_else(|| PipelineError::Usage("at least one input is required".to_string()))?;
    for frame in &frames[1..] {
        if frame.meta.width != first.meta.width
            || frame.meta.height != first.meta.height
            || frame.meta.bits != first.meta.bits
        {
            return Err(PipelineError::Input(format!(
                "input '{}' is {}x{} at {} bits but '{}' is {}x{} at {} bits",
                frame.meta.path.display(),
                frame.meta.width,
                frame.meta.height,
                frame.meta.bits,
                first.meta.path.display(),
                first.meta.width,
                first.meta.height,
                first.meta.bits
            )));
        }
    }

    let reference = match cli.reference {
        Reference::Auto => 0,
        Reference::Index(index) => {
            if index >= frames.len() {
                return Err(PipelineError::Usage(format!(
                    "--reference {index} is out of range for {} input frame(s)",
                    frames.len()
                )));
            }
            index
        }
    };

    let options = WriteOptions {
        compression: compression(cli.compression),
        bits: cli.bits.value(),
        metadata: if cli.strip_metadata {
            MetadataPolicy::Strip
        } else {
            MetadataPolicy::Copy
        },
    };

    let output = if cli.method == Method::Pmax && frames.len() > 1 {
        progress.step(format_args!("stacking with PMax"));
        pmax_frame(cli, &frames, reference)
    } else {
        frames[reference].clone()
    };

    progress.step(format_args!("writing {}", cli.output.display()));
    tiff_write::write_tiff(&cli.output, &output, &options)
        .map_err(|err| PipelineError::Io(err.to_string()))?;

    if let Some(path) = &cli.report {
        write_report(path, cli, &frames, reference)?;
    }

    progress.step(format_args!(
        "wrote {} ({}x{}, {} bits, {} frame(s))",
        cli.output.display(),
        first.meta.width,
        first.meta.height,
        options.bits,
        frames.len()
    ));
    Ok(())
}

/// Stacks loaded frames with the PMax engine.
///
/// Focus energies come from the encoded luma pyramids from
/// `docs/focus-measure.md`; blending happens in linear light per
/// `docs/stacking.md` §2.2–2.3.
fn pmax_frame(cli: &Cli, frames: &[Frame], reference: usize) -> Frame {
    let reference_frame = &frames[reference];
    let width = reference_frame.meta.width;
    let height = reference_frame.meta.height;
    let count = (width as usize) * (height as usize);

    let mut encoded: Vec<Vec<f32>> = frames
        .iter()
        .map(|frame| {
            frame
                .rgb
                .iter()
                .map(|&value| f32::from(value) / 65535.0)
                .collect()
        })
        .collect();

    let mut guide = vec![0.0f32; count];
    encoded_luma_buffer(&encoded[reference], &mut guide);

    let mut energies = Vec::with_capacity(frames.len());
    let mut aggregates = Vec::with_capacity(frames.len());
    let mut calibrations = Vec::with_capacity(frames.len());
    let mut saturation = Vec::with_capacity(frames.len());

    for rgb in &encoded {
        let mut luma = vec![0.0f32; count];
        encoded_luma_buffer(rgb, &mut luma);

        let mut pyramid = Pyramid::new();
        pyramid.build(&luma, width, height);
        let noise = noise::estimate(&pyramid, DEFAULT_NOISE_K);
        let energy = EnergyMaps::compute(
            &pyramid,
            &noise,
            EnergyOptions {
                radius: cli.estimation_radius,
                contrast_normalize: cli.contrast_normalize,
            },
        );
        let aggregate = energy.aggregate();
        let sigmas: Vec<f32> = noise.sigmas().collect();

        calibrations.push(FrameCalibration::new(&aggregate, &sigmas));
        aggregates.push(aggregate);
        energies.push(energy);
        saturation.push(saturation_mask(rgb, width, height, 3));
    }

    let confidence = ConfidenceMap::compute(
        &aggregates,
        &guide,
        width,
        height,
        &calibrations,
        Some(&saturation),
        ConfidenceOptions::default(),
    );

    // Decode to linear light in place to avoid a second full-resolution copy.
    for rgb in &mut encoded {
        for value in rgb.iter_mut() {
            *value = srgb_decode(*value);
        }
    }
    let views: Vec<&[f32]> = encoded.iter().map(Vec::as_slice).collect();
    let blended = pmax::stack(
        &views,
        width,
        height,
        &energies,
        &confidence,
        WeightOptions {
            beta: cli.pmax_beta,
        },
    );

    let mut output = reference_frame.clone();
    output.rgb = blended
        .iter()
        .map(|&value| (value.clamp(0.0, 1.0) * 65535.0).round() as u16)
        .collect();
    output
}

fn compression(value: Compression) -> tiff_write::Compression {
    match value {
        Compression::None => tiff_write::Compression::None,
        Compression::Lzw => tiff_write::Compression::Lzw,
        Compression::Deflate => tiff_write::Compression::Deflate,
    }
}

fn write_report(
    path: &Path,
    cli: &Cli,
    frames: &[Frame],
    reference: usize,
) -> Result<(), PipelineError> {
    let mut report = String::new();
    report.push_str("{\n");
    let _ = writeln!(report, "  \"version\": {},", json_string(VERSION));
    let _ = writeln!(
        report,
        "  \"inputs\": [{}],",
        cli.inputs
            .iter()
            .map(|path| json_string(&path.to_string_lossy()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(
        report,
        "  \"output\": {},",
        json_string(&cli.output.to_string_lossy())
    );
    let frame = frames
        .get(reference)
        .ok_or_else(|| PipelineError::Internal("missing reference frame".to_string()))?;
    let _ = writeln!(report, "  \"width\": {},", frame.meta.width);
    let _ = writeln!(report, "  \"height\": {},", frame.meta.height);
    let _ = writeln!(report, "  \"bits\": {},", frame.meta.bits);
    let _ = writeln!(report, "  \"frames\": {},", frames.len());
    let _ = writeln!(report, "  \"reference\": {reference},");
    let _ = writeln!(
        report,
        "  \"method\": {},",
        json_string(&format!("{:?}", cli.method))
    );
    let _ = writeln!(
        report,
        "  \"alignment\": {},",
        json_string(&format!("{:?}", cli.align))
    );
    let _ = writeln!(
        report,
        "  \"compression\": {}",
        json_string(&format!("{:?}", cli.compression))
    );
    report.push_str("}\n");

    std::fs::write(path, report).map_err(|err| {
        PipelineError::Io(format!(
            "failed to write report '{}': {err}",
            path.display()
        ))
    })
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

struct Progress {
    enabled: bool,
}

impl Progress {
    fn new(quiet: bool, verbose: bool) -> Self {
        Self {
            enabled: !quiet && (verbose || io::stderr().is_terminal()),
        }
    }

    fn step(&self, message: fmt::Arguments<'_>) {
        if self.enabled {
            let _ = writeln!(io::stderr(), "focus-stack: {message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_string_escapes_control_characters() {
        assert_eq!(json_string("a\n\"b\\"), "\"a\\n\\\"b\\\\\"");
    }

    #[test]
    fn exit_codes_match_the_cli_document() {
        assert_eq!(PipelineError::Usage(String::new()).exit_code(), 2);
        assert_eq!(PipelineError::Input(String::new()).exit_code(), 3);
        assert_eq!(PipelineError::Io(String::new()).exit_code(), 3);
        assert_eq!(PipelineError::Internal(String::new()).exit_code(), 1);
    }

    #[test]
    fn bits_map_to_numeric_depth() {
        assert_eq!(crate::cli::Bits::Eight.value(), 8);
        assert_eq!(crate::cli::Bits::Sixteen.value(), 16);
    }
}
