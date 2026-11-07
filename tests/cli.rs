//! End-to-end CLI tests for argument parsing, validation, and the stub pipeline.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{WriteOptions, write_tiff};
use focus_stack::{Frame, FrameMeta};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures/io")
        .join(name)
}

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_focus-stack"))
}

fn run_str(args: &[&str]) -> Output {
    let mut command = command();
    for arg in args {
        command.arg(arg);
    }
    command.output().expect("run focus-stack")
}

/// Temporary directory removed when the test ends.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-cli-{}-{id}-{label}",
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

fn small_frame(path: PathBuf, width: u32, height: u32) -> Frame {
    Frame {
        meta: FrameMeta {
            path,
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
        rgb: vec![0; (width * height * 3) as usize],
    }
}

#[test]
fn help_lists_every_documented_option() {
    let output = run_str(&["--help"]);
    assert!(
        output.status.success(),
        "--help failed: {:?}",
        output.status
    );
    let help = String::from_utf8_lossy(&output.stdout);

    for option in [
        "--output",
        "--method",
        "--align",
        "--reference",
        "--max-rotation",
        "--scale-range",
        "--max-residual",
        "--force",
        "--keep-borders",
        "--estimation-radius",
        "--keep-detail",
        "--detail-transfer",
        "--exposure-match",
        "--pmax-beta",
        "--dmap-lambda",
        "--contrast-normalize",
        "--depth-map",
        "--source-map",
        "--report",
        "--preview",
        "--preview-size",
        "--compression",
        "--bits",
        "--output-profile",
        "--assume-profile",
        "--allow-profile-mismatch",
        "--strip-metadata",
        "--stamp",
        "--timings",
        "--threads",
        "--max-memory",
        "--seed",
        "--strict",
        "--verbose",
        "--quiet",
        "--version",
    ] {
        assert!(help.contains(option), "help is missing {option}:\n{help}");
    }
}

#[test]
fn one_input_performs_an_identity_pass() {
    let dir = TempDir::new("identity");
    let out = dir.join("out.tif");
    let source = fixture("rgb16_none.tif");

    let output = run_str(&[source.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let original = read_tiff(source).unwrap();
    let written = read_tiff(&out).unwrap();
    assert_eq!(written.meta.width, original.meta.width);
    assert_eq!(written.meta.height, original.meta.height);
    assert_eq!(written.meta.bits, 16);
    assert_eq!(written.rgb, original.rgb);
}

#[test]
fn multiple_inputs_write_a_valid_tiff() {
    let dir = TempDir::new("stack");
    let out = dir.join("out.tif");
    let first = fixture("rgb16_none.tif");
    let second = fixture("rgb16_lzw.tif");

    let output = run_str(&[
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = read_tiff(&out).unwrap();
    let original = read_tiff(first).unwrap();
    assert_eq!(written.meta.width, original.meta.width);
    assert_eq!(written.meta.height, original.meta.height);
    assert_eq!(written.rgb, original.rgb);
}

#[test]
fn mismatched_dimensions_are_rejected() {
    let dir = TempDir::new("mismatched-size");
    let odd = dir.join("odd.tif");
    let odd_frame = small_frame(odd.clone(), 3, 2);
    write_tiff(&odd, &odd_frame, &WriteOptions::default()).unwrap();
    let out = dir.join("out.tif");

    let output = run_str(&[
        fixture("rgb16_none.tif").to_str().unwrap(),
        odd.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !out.exists(),
        "output must not be written after validation fails"
    );
}

#[test]
fn mismatched_bit_depth_is_rejected() {
    let dir = TempDir::new("mismatched-bits");
    let out = dir.join("out.tif");

    let output = run_str(&[
        fixture("rgb16_none.tif").to_str().unwrap(),
        fixture("rgb8_none.tif").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(
        output.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!out.exists());
}

#[test]
fn missing_and_corrupt_inputs_exit_three() {
    let dir = TempDir::new("bad-input");
    let out = dir.join("out.tif");

    let missing = run_str(&[
        dir.join("missing.tif").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(missing.status.code(), Some(3));

    let corrupt = run_str(&[
        fixture("truncated.tif").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(corrupt.status.code(), Some(3));
    assert!(!out.exists());
}

#[test]
fn usage_errors_exit_two() {
    for args in [
        vec!["only-input-without-output.tif"],
        vec!["-o", "out.tif"],
        vec!["a.tif", "-o", "out.tif", "--method", "nonsense"],
        vec!["a.tif", "-o", "out.tif", "--reference", "many"],
    ] {
        let output = run_str(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "args {args:?} stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn out_of_range_reference_exits_two() {
    let dir = TempDir::new("reference");
    let out = dir.join("out.tif");

    let output = run_str(&[
        fixture("rgb16_none.tif").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--reference",
        "4",
    ]);
    assert_eq!(
        output.status.code(),
        Some(2),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn report_is_written() {
    let dir = TempDir::new("report");
    let out = dir.join("out.tif");
    let report = dir.join("run.json");

    let output = run_str(&[
        fixture("rgb16_none.tif").to_str().unwrap(),
        fixture("rgb16_lzw.tif").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--report",
        report.to_str().unwrap(),
        "--method",
        "dmap",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let report = fs::read_to_string(report).unwrap();
    assert!(report.contains("\"schema_version\": 1"));
    assert!(report.contains(&format!("\"version\": \"{}\"", focus_stack::VERSION)));
    assert!(report.contains("\"inputs\""));
    assert!(report.contains("\"sha256\""));
    assert!(report.contains("\"params\""));
    assert!(report.contains("\"chosen\": \"dmap\""));
    assert!(report.contains("\"output\""));
    assert!(!report.contains("\"timing_ms\""));
    assert!(!report.contains("\"created_at\""));
}

#[test]
fn explicit_reference_selects_the_requested_frame() {
    let dir = TempDir::new("explicit-reference");
    let out = dir.join("out.tif");
    let second = dir.join("second.tif");
    let first = fixture("rgb16_none.tif");
    let source = read_tiff(&first).unwrap();
    let mut frame = small_frame(second.clone(), source.meta.width, source.meta.height);
    frame.rgb = vec![1234; frame.rgb.len()];
    write_tiff(&second, &frame, &WriteOptions::default()).unwrap();

    let output = run_str(&[
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
        "--reference",
        "1",
    ]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let written = read_tiff(&out).unwrap();
    assert_eq!(written.rgb, vec![1234; written.rgb.len()]);
}

#[test]
fn output_paths_are_validated() {
    let dir = TempDir::new("output-path");
    let out = dir.join("missing-dir/out.tif");

    let output = run_str(&[
        fixture("rgb16_none.tif").to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);
    assert_eq!(output.status.code(), Some(3));
}

#[test]
fn paths_can_contain_spaces() {
    let dir = TempDir::new("spaces");
    let out = dir.join("out file.tif");
    let input_copy = dir.join("input file.tif");
    fs::copy(fixture("rgb16_none.tif"), &input_copy).unwrap();

    let output = run_str(&[input_copy.to_str().unwrap(), "-o", out.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(out.exists());
}
