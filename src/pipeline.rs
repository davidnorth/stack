//! Load -> align -> measure -> stack -> write orchestration.

use std::fmt;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::cli::{Align, Cli, Compression, ExposureMatch, Method, OutputProfile, Reference};
use crate::color::luma::encoded_luma_buffer;
use crate::color::transfer::srgb_decode;
use crate::focus::confidence::{
    ConfidenceMap, ConfidenceOptions, FrameCalibration, saturation_mask,
};
use crate::focus::energy::{EnergyMaps, EnergyOptions};
use crate::focus::noise::{self, DEFAULT_NOISE_K, SIGMA_EPSILON};
use crate::focus::pyramid::Pyramid;
use crate::io::metadata::MetadataPolicy;
use crate::io::tiff_read::read_tiff;
use crate::io::tiff_write::{self, WriteOptions};
use crate::output::depth::write_depth_map;
use crate::output::preview;
use crate::output::report::{
    AlignmentReport, FrameAlignment, InputReport, MethodReport, OutputReport, ParamsReport, Report,
    TimingReport, icc_description, sha256_file, utc_timestamp_now,
};
use crate::stack::dmap::{self, CostVolume, DmapOptions, InterpolationOptions};
use crate::stack::pmax::{self, WeightOptions};
use crate::{DepthField, Frame, Similarity};

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
    configure_threads(cli);
    let progress = Progress::new(cli.quiet, cli.verbose);
    let output_path = cli.output.as_ref().ok_or_else(|| {
        PipelineError::Usage("the required argument '--output <PATH>' was not provided".to_string())
    })?;

    let load_start = Instant::now();
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
    let load_ms = elapsed_ms(load_start);

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

    let mut depth: Option<DepthField> = None;
    let chosen = chosen_method(cli.method, frames.len());
    let stack_start = Instant::now();
    let output = match cli.method {
        Method::Pmax if frames.len() > 1 => {
            progress.step(format_args!("stacking with PMax"));
            pmax_frame(cli, &frames, reference)
        }
        Method::Dmap => {
            progress.step(format_args!("stacking with DMap"));
            let (frame, field) = dmap_frame(cli, &frames, reference);
            depth = Some(field);
            frame
        }
        _ => frames[reference].clone(),
    };

    let depth_paths: Vec<&Path> = cli
        .depth_map
        .iter()
        .chain(cli.source_map.iter())
        .map(PathBuf::as_path)
        .collect();
    if depth.is_none() && !depth_paths.is_empty() {
        progress.step(format_args!("solving DMap depth for the depth map"));
        let (_, field) = dmap_frame(cli, &frames, reference);
        depth = Some(field);
    }
    let stack_ms = elapsed_ms(stack_start);

    let encode_start = Instant::now();
    progress.step(format_args!("writing {}", output_path.display()));
    tiff_write::write_tiff(output_path, &output, &options)
        .map_err(|err| PipelineError::Io(err.to_string()))?;

    let mut warnings = Vec::new();
    if let Some(field) = &depth {
        for path in depth_paths {
            let report = write_depth_map(
                path,
                &field.d,
                first.meta.width,
                first.meta.height,
                frames.len(),
            )
            .map_err(|err| PipelineError::Io(err.to_string()))?;
            if report.all_zero {
                Progress::warn(format_args!(
                    "depth map '{}' is all zeros for a single input frame",
                    path.display()
                ));
                warnings.push(format!(
                    "depth map '{}' is all zeros for a single input frame",
                    path.display()
                ));
            }
        }
    }

    let encode_ms = elapsed_ms(encode_start);

    if let Some(path) = &cli.preview {
        progress.step(format_args!("writing preview {}", path.display()));
        preview::write_preview(path, &output, cli.preview_size, output.meta.icc.as_deref())
            .map_err(|err| PipelineError::Io(err.to_string()))?;
    }

    if let Some(path) = &cli.report {
        write_report(ReportInputs {
            path,
            cli,
            output_path,
            output: &output,
            frames: &frames,
            reference,
            chosen,
            warnings,
            timing: TimingReport {
                load: load_ms,
                align: 0,
                focus: 0,
                stack: stack_ms,
                encode: encode_ms,
            },
        })?;
    }

    progress.step(format_args!(
        "wrote {} ({}x{}, {} bits, {} frame(s))",
        output_path.display(),
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
struct FocusInputs {
    width: u32,
    height: u32,
    encoded: Vec<Vec<f32>>,
    guide: Vec<f32>,
    energies: Vec<EnergyMaps>,
    aggregates: Vec<Vec<f32>>,
    calibrations: Vec<FrameCalibration>,
    saturation: Vec<Vec<bool>>,
}

/// Focus evidence shared by the PMax and DMap engines.
fn focus_inputs(cli: &Cli, frames: &[Frame], reference: usize) -> FocusInputs {
    let width = frames[reference].meta.width;
    let height = frames[reference].meta.height;
    let count = (width as usize) * (height as usize);

    let encoded: Vec<Vec<f32>> = frames
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

    FocusInputs {
        width,
        height,
        encoded,
        guide,
        energies,
        aggregates,
        calibrations,
        saturation,
    }
}

fn decode_linear(encoded: &mut [Vec<f32>]) {
    // Decode in place to avoid a second full-resolution copy.
    for rgb in encoded {
        for value in rgb.iter_mut() {
            *value = srgb_decode(*value);
        }
    }
}

fn output_frame(source: &Frame, rgb: &[f32]) -> Frame {
    let mut output = source.clone();
    output.rgb = rgb
        .iter()
        .map(|&value| (value.clamp(0.0, 1.0) * 65535.0).round() as u16)
        .collect();
    output
}

fn pmax_frame(cli: &Cli, frames: &[Frame], reference: usize) -> Frame {
    let mut inputs = focus_inputs(cli, frames, reference);
    let confidence = ConfidenceMap::compute(
        &inputs.aggregates,
        &inputs.guide,
        inputs.width,
        inputs.height,
        &inputs.calibrations,
        Some(&inputs.saturation),
        ConfidenceOptions::default(),
    );

    decode_linear(&mut inputs.encoded);
    let views: Vec<&[f32]> = inputs.encoded.iter().map(Vec::as_slice).collect();
    let blended = pmax::stack(
        &views,
        inputs.width,
        inputs.height,
        &inputs.energies,
        &confidence,
        WeightOptions {
            beta: cli.pmax_beta,
        },
    );

    output_frame(&frames[reference], &blended)
}

fn dmap_frame(cli: &Cli, frames: &[Frame], reference: usize) -> (Frame, DepthField) {
    let mut inputs = focus_inputs(cli, frames, reference);
    let volume = CostVolume::new(&inputs.aggregates, inputs.width, inputs.height);
    let noise_sigma = inputs
        .calibrations
        .get(reference)
        .map_or(SIGMA_EPSILON, |calibration| calibration.sigma);
    let smoothing_radius = cli
        .estimation_radius
        .map_or(dmap::DEFAULT_SMOOTHING_RADIUS, |radius| 0.5 * radius as f32);

    decode_linear(&mut inputs.encoded);
    let views: Vec<&[f32]> = inputs.encoded.iter().map(Vec::as_slice).collect();
    let result = dmap::stack_dmap(
        &views,
        inputs.width,
        inputs.height,
        &volume,
        &inputs.guide,
        DmapOptions {
            lambda: cli.dmap_lambda,
            keep_detail: cli.keep_detail,
            noise_sigma,
            smoothing_radius,
            interpolation: InterpolationOptions::default(),
        },
    );

    (
        output_frame(&frames[reference], &result.rgb),
        result.solved.field,
    )
}

fn compression(value: Compression) -> tiff_write::Compression {
    match value {
        Compression::None => tiff_write::Compression::None,
        Compression::Lzw => tiff_write::Compression::Lzw,
        Compression::Deflate => tiff_write::Compression::Deflate,
    }
}

struct ReportInputs<'a> {
    path: &'a Path,
    cli: &'a Cli,
    output_path: &'a Path,
    output: &'a Frame,
    frames: &'a [Frame],
    reference: usize,
    chosen: &'static str,
    warnings: Vec<String>,
    timing: TimingReport,
}

fn write_report(input: ReportInputs<'_>) -> Result<(), PipelineError> {
    let cli = input.cli;
    let inputs = input
        .frames
        .iter()
        .zip(&cli.inputs)
        .map(|(frame, path)| {
            let sha256 = sha256_file(path).map_err(|err| {
                PipelineError::Io(format!("failed to hash input '{}': {err}", path.display()))
            })?;
            Ok(InputReport::new(
                path.to_string_lossy().into_owned(),
                sha256,
                frame.meta.width,
                frame.meta.height,
                frame.meta.bits,
                frame.meta.orientation,
                icc_description(frame.meta.icc.as_deref()),
            ))
        })
        .collect::<Result<Vec<_>, PipelineError>>()?;

    let transforms = input
        .frames
        .iter()
        .map(|_| FrameAlignment::new(identity(), 1.0, 1.0))
        .collect();
    let alignment = AlignmentReport {
        reference_index: input.reference,
        frames: transforms,
        ..AlignmentReport::default()
    };

    let reference = match cli.reference {
        Reference::Auto => "auto".to_string(),
        Reference::Index(index) => index.to_string(),
    };
    let params = ParamsReport {
        method: method_name(cli.method).to_string(),
        align: align_name(cli.align).to_string(),
        reference,
        estimation_radius: cli.estimation_radius,
        keep_detail: cli.keep_detail,
        detail_transfer: cli.detail_transfer,
        exposure_match: exposure_name(cli.exposure_match).to_string(),
        compression: compression_name(cli.compression).to_string(),
        output_profile: output_profile_name(cli.output_profile).to_string(),
        seed: cli.seed,
        threads: cli.threads,
    };

    let output_hash = sha256_file(input.output_path).map_err(|err| {
        PipelineError::Io(format!(
            "failed to hash output '{}': {err}",
            input.output_path.display()
        ))
    })?;
    let output = OutputReport {
        path: input.output_path.to_string_lossy().into_owned(),
        sha256: output_hash,
        clipped_pct: clipped_fraction(&input.output.rgb),
        profile: icc_description(input.output.meta.icc.as_deref()),
    };

    let report = Report {
        inputs,
        params,
        alignment,
        method: MethodReport {
            chosen: input.chosen.to_string(),
            reason: method_reason(cli.method, input.frames.len()).to_string(),
        },
        output,
        timing_ms: cli.timings.then_some(input.timing),
        created_at: cli.stamp.then(utc_timestamp_now),
        warnings: input.warnings,
        ..Report::default()
    };

    std::fs::write(input.path, report.to_json()).map_err(|err| {
        PipelineError::Io(format!(
            "failed to write report '{}': {err}",
            input.path.display()
        ))
    })
}

fn identity() -> Similarity {
    Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    }
}

fn clipped_fraction(rgb: &[u16]) -> f64 {
    let pixels = rgb.len() / 3;
    if pixels == 0 {
        return 0.0;
    }
    let clipped = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .filter(|pixel| pixel.iter().any(|&value| value == 0 || value == u16::MAX))
        .count();
    clipped as f64 / pixels as f64
}

fn chosen_method(method: Method, frames: usize) -> &'static str {
    match method {
        Method::Pmax if frames > 1 => "pmax",
        Method::Dmap => "dmap",
        _ => "reference",
    }
}

fn method_reason(method: Method, frames: usize) -> &'static str {
    if frames == 1 {
        "single input"
    } else {
        match method {
            Method::Pmax | Method::Dmap => "explicit",
            _ => "fallback",
        }
    }
}

const fn method_name(value: Method) -> &'static str {
    match value {
        Method::Auto => "auto",
        Method::Pmax => "pmax",
        Method::Dmap => "dmap",
        Method::Hybrid => "hybrid",
    }
}

const fn align_name(value: Align) -> &'static str {
    match value {
        Align::Auto => "auto",
        Align::Translation => "translation",
        Align::Similarity => "similarity",
        Align::Off => "off",
    }
}

const fn exposure_name(value: ExposureMatch) -> &'static str {
    match value {
        ExposureMatch::Off => "off",
        ExposureMatch::Luma => "luma",
        ExposureMatch::Rgb => "rgb",
    }
}

const fn compression_name(value: Compression) -> &'static str {
    match value {
        Compression::None => "none",
        Compression::Lzw => "lzw",
        Compression::Deflate => "deflate",
    }
}

const fn output_profile_name(value: OutputProfile) -> &'static str {
    match value {
        OutputProfile::Same => "same",
        OutputProfile::Srgb => "srgb",
        OutputProfile::Linear => "linear",
    }
}

fn configure_threads(cli: &Cli) {
    if let Some(threads) = cli.threads {
        // Ignore an already-initialised global pool; the first caller owns it.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build_global();
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
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

    fn warn(message: fmt::Arguments<'_>) {
        let _ = writeln!(io::stderr(), "focus-stack: warning: {message}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
