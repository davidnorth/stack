//! Tiny regression suite and CI gate (`docs/quality.md` §4).
//!
//! The expected metrics live in `fixtures/manifest.toml`; a run that moves a
//! metric beyond the manifest tolerance fails. Golden image hashes are not used
//! here because they measure determinism, not quality.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use focus_stack::align::phase::phase_correlate;
use focus_stack::color::luma::linear_luma;
use focus_stack::color::transfer::srgb_encode;
use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{WriteOptions, write_tiff};
use focus_stack::quality::metrics::{LinearImage, Metrics, evaluate};
use focus_stack::quality::synthetic::{SyntheticConfig, SyntheticStack};
use focus_stack::{Frame, FrameMeta};
use serde::Deserialize;

const MANIFEST_TEXT: &str = include_str!("../fixtures/manifest.toml");

#[derive(Debug, Deserialize)]
struct Manifest {
    schema_version: u32,
    tolerances: Tolerances,
    io: IoFixtures,
    alignment: Vec<AlignmentFixture>,
    stack: Vec<StackFixture>,
    report: ReportFixture,
}

#[derive(Debug, Deserialize)]
struct Tolerances {
    psnr_db: f64,
    ssim: f64,
    detail_recovery: f64,
    halo_overshoot: f64,
    noise_gain: f64,
    delta_e00: f64,
}

#[derive(Debug, Deserialize)]
struct IoFixtures {
    roundtrip: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct AlignmentFixture {
    id: String,
    config: String,
    golden_dx: f64,
    golden_dy: f64,
    tolerance_px: f64,
}

#[derive(Debug, Deserialize)]
struct StackFixture {
    id: String,
    config: String,
    method: String,
    psnr_db: f64,
    ssim: f64,
    detail_recovery: f64,
    halo_overshoot: f64,
    noise_gain: f64,
    delta_e00: f64,
}

#[derive(Debug, Deserialize)]
struct ReportFixture {
    method: String,
    required_keys: Vec<String>,
}

fn manifest() -> Manifest {
    let manifest: Manifest =
        toml::from_str(MANIFEST_TEXT).expect("fixtures/manifest.toml must parse");
    assert_eq!(manifest.schema_version, 1, "unsupported manifest schema");
    manifest
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn stack_fixture<'a>(manifest: &'a Manifest, id: &str) -> &'a StackFixture {
    manifest
        .stack
        .iter()
        .find(|fixture| fixture.id == id)
        .unwrap_or_else(|| panic!("manifest has no stack fixture '{id}'"))
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-regression-{}-{id}-{label}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_fixture_frames(stack: &SyntheticStack, dir: &TempDir) -> Vec<PathBuf> {
    let width = stack.config.width;
    let height = stack.config.height;
    stack
        .frames
        .iter()
        .enumerate()
        .map(|(index, samples)| {
            let rgb = samples
                .iter()
                .map(|&value| {
                    let encoded = srgb_encode(f32::from(value) / 65_535.0);
                    (encoded.clamp(0.0, 1.0) * 65_535.0).round() as u16
                })
                .collect();
            let path = dir.join(&format!("frame-{index}.tif"));
            let frame = Frame {
                meta: FrameMeta {
                    path: path.clone(),
                    width,
                    height,
                    bits: 16,
                    icc: None,
                    orientation: 1,
                    exif: None,
                    xmp: None,
                    iptc: None,
                    resolution: None,
                },
                rgb,
            };
            write_tiff(&path, &frame, &WriteOptions::default()).unwrap();
            path
        })
        .collect()
}

fn run_stack(config: &SyntheticConfig, method: &str) -> (TempDir, SyntheticStack) {
    let stack = SyntheticStack::generate(config).unwrap();
    let dir = TempDir::new(method);
    let inputs = write_fixture_frames(&stack, &dir);
    let output_path = dir.join("output.tif");

    let mut command = Command::new(env!("CARGO_BIN_EXE_focus-stack"));
    for input in &inputs {
        command.arg(input);
    }
    let output = command
        .arg("--method")
        .arg(method)
        .arg("--quiet")
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run focus-stack");
    assert!(
        output.status.success(),
        "method {method} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    (dir, stack)
}

fn linear_image(width: u32, height: u32, samples: Vec<f32>) -> LinearImage {
    LinearImage::new(width, height, samples).unwrap()
}

fn output_image(path: &Path) -> LinearImage {
    // The pipeline writes blended linear-light samples, so compare them
    // directly with the generator's linear scene.
    let frame = read_tiff(path).unwrap();
    linear_image(
        frame.meta.width,
        frame.meta.height,
        frame
            .rgb
            .iter()
            .map(|&value| f32::from(value) / 65_535.0)
            .collect(),
    )
}

fn gradient_energy(image: &LinearImage) -> f64 {
    let luma = image.luma();
    let width = image.width as usize;
    let height = image.height as usize;
    let mut total = 0.0;
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            if x + 1 < width {
                total += f64::from((luma[index + 1] - luma[index]).abs());
            }
            if y + 1 < height {
                total += f64::from((luma[index + width] - luma[index]).abs());
            }
        }
    }
    total
}

fn sharpest_frame(stack: &SyntheticStack) -> LinearImage {
    let width = stack.config.width;
    let height = stack.config.height;
    let mut best = None;
    for index in 0..stack.config.frames {
        let image = linear_image(width, height, stack.frame_linear(index));
        let energy = gradient_energy(&image);
        if best
            .as_ref()
            .is_none_or(|(best_energy, _)| energy > *best_energy)
        {
            best = Some((energy, image));
        }
    }
    best.expect("stack has at least one frame").1
}

fn measure_stack(stack: &SyntheticStack, output: &LinearImage) -> Metrics {
    let truth = linear_image(stack.config.width, stack.config.height, stack.scene.clone());
    let sharpest = sharpest_frame(stack);
    evaluate(&truth, output, Some(&stack.depth), Some(&sharpest)).unwrap()
}

fn assert_metric(label: &str, actual: f64, expected: f64, tolerance: f64) {
    assert!(
        actual.is_finite(),
        "{label} is not finite: {actual} (expected {expected} ± {tolerance})"
    );
    assert!(
        (actual - expected).abs() <= tolerance,
        "{label} regressed beyond tolerance: {actual}, expected {expected} ± {tolerance}"
    );
}

fn check_stack(id: &str) {
    let manifest = manifest();
    let fixture = stack_fixture(&manifest, id);
    let started = Instant::now();

    let config = SyntheticConfig::from_toml_file(&fixtures_dir().join(&fixture.config)).unwrap();
    let (dir, stack) = run_stack(&config, &fixture.method);
    let output = output_image(&dir.join("output.tif"));
    assert_eq!(
        (output.width, output.height),
        (config.width, config.height),
        "{id} output dimensions"
    );
    let metrics = measure_stack(&stack, &output);

    let tolerance = &manifest.tolerances;
    assert_metric(
        "PSNR (dB)",
        metrics.psnr_db,
        fixture.psnr_db,
        tolerance.psnr_db,
    );
    assert_metric("SSIM", metrics.ssim, fixture.ssim, tolerance.ssim);
    assert_metric(
        "detail recovery",
        metrics.detail_recovery,
        fixture.detail_recovery,
        tolerance.detail_recovery,
    );
    assert_metric(
        "halo overshoot",
        metrics.halo_overshoot,
        fixture.halo_overshoot,
        tolerance.halo_overshoot,
    );
    assert_metric(
        "noise gain",
        metrics.noise_gain.expect("sharpest input supplied"),
        fixture.noise_gain,
        tolerance.noise_gain,
    );
    assert_metric(
        "delta E00",
        metrics.delta_e00,
        fixture.delta_e00,
        tolerance.delta_e00,
    );

    eprintln!(
        "{id}: {metrics:?} in {:.1}s",
        started.elapsed().as_secs_f64()
    );
}

#[test]
fn manifest_lists_every_engine_and_fixture() {
    let manifest = manifest();
    for method in ["pmax", "dmap", "hybrid"] {
        assert!(
            manifest
                .stack
                .iter()
                .any(|fixture| fixture.method == method),
            "manifest has no stack fixture for method '{method}'"
        );
    }
    for fixture in &manifest.stack {
        assert!(
            fixtures_dir().join(&fixture.config).exists(),
            "missing config for {}",
            fixture.id
        );
    }
    for fixture in &manifest.alignment {
        assert!(
            fixtures_dir().join(&fixture.config).exists(),
            "missing config for {}",
            fixture.id
        );
    }
    for path in &manifest.io.roundtrip {
        assert!(
            fixtures_dir().join(path).exists(),
            "missing I/O fixture {path}"
        );
    }
    assert!(
        manifest
            .stack
            .iter()
            .any(|fixture| fixture.method == manifest.report.method),
        "report method '{}' has no stack fixture",
        manifest.report.method
    );
}

#[test]
fn io_roundtrip_is_bit_exact() {
    let manifest = manifest();
    let dir = TempDir::new("io");
    for relative in &manifest.io.roundtrip {
        let source = read_tiff(fixtures_dir().join(relative)).unwrap();
        let destination = dir.join(&format!(
            "{}.tif",
            Path::new(relative).file_stem().unwrap().to_string_lossy()
        ));
        write_tiff(&destination, &source, &WriteOptions::default()).unwrap();
        let roundtrip = read_tiff(&destination).unwrap();

        assert_eq!(roundtrip.meta.width, source.meta.width, "{relative}");
        assert_eq!(roundtrip.meta.height, source.meta.height, "{relative}");
        assert_eq!(roundtrip.meta.bits, source.meta.bits, "{relative}");
        assert_eq!(roundtrip.rgb, source.rgb, "{relative} pixels");
        assert_eq!(roundtrip.meta.icc, source.meta.icc, "{relative} ICC");
    }
}

#[test]
fn alignment_estimate_matches_golden() {
    let manifest = manifest();
    for fixture in &manifest.alignment {
        let config =
            SyntheticConfig::from_toml_file(&fixtures_dir().join(&fixture.config)).unwrap();
        let stack = SyntheticStack::generate(&config).unwrap();
        assert_eq!(
            stack.frames.len(),
            2,
            "{} needs exactly two frames",
            fixture.id
        );

        let luma: Vec<Vec<f32>> = (0..2)
            .map(|frame| {
                stack
                    .frame_linear(frame)
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|pixel| linear_luma(pixel[0], pixel[1], pixel[2]))
                    .collect()
            })
            .collect();
        let result = phase_correlate(&luma[1], &luma[0], config.width, config.height).unwrap();
        assert!(
            (result.dx - fixture.golden_dx).abs() <= fixture.tolerance_px,
            "{} dx regressed: {}, expected {} ± {}",
            fixture.id,
            result.dx,
            fixture.golden_dx,
            fixture.tolerance_px
        );
        assert!(
            (result.dy - fixture.golden_dy).abs() <= fixture.tolerance_px,
            "{} dy regressed: {}, expected {} ± {}",
            fixture.id,
            result.dy,
            fixture.golden_dy,
            fixture.tolerance_px
        );
    }
}

#[test]
fn stack_512_pmax() {
    check_stack("stack-512-pmax");
}

#[test]
fn stack_512_dmap() {
    check_stack("stack-512-dmap");
}

#[test]
fn stack_512_hybrid() {
    check_stack("stack-512-hybrid");
}

#[test]
fn report_schema_matches_the_contract() {
    let manifest = manifest();
    let fixture = stack_fixture(&manifest, "stack-512-pmax");
    assert_eq!(fixture.method, manifest.report.method);

    let config = SyntheticConfig::from_toml_file(&fixtures_dir().join(&fixture.config)).unwrap();
    let stack = SyntheticStack::generate(&config).unwrap();
    let dir = TempDir::new("report");
    let inputs = write_fixture_frames(&stack, &dir);
    let output_path = dir.join("output.tif");
    let report_path = dir.join("report.json");

    let mut command = Command::new(env!("CARGO_BIN_EXE_focus-stack"));
    for input in &inputs {
        command.arg(input);
    }
    let output = command
        .arg("--method")
        .arg(&manifest.report.method)
        .arg("--quiet")
        .arg("--report")
        .arg(&report_path)
        .arg("-o")
        .arg(&output_path)
        .output()
        .expect("run focus-stack with --report");
    assert!(
        output.status.success(),
        "report run failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report = fs::read_to_string(&report_path).unwrap();
    for key in &manifest.report.required_keys {
        assert!(
            report.contains(&format!("\"{key}\"")),
            "report is missing '{key}':\n{report}"
        );
    }
    assert!(
        report.contains(&format!("\"chosen\": \"{}\"", fixture.method)),
        "report does not record the chosen method:\n{report}"
    );
    assert!(
        report.contains("\"schema_version\": 1"),
        "report schema version:\n{report}"
    );
    assert_eq!(
        report.matches("\"sha256\":").count(),
        inputs.len() + 1,
        "report must hash every input and the output:\n{report}"
    );
}
