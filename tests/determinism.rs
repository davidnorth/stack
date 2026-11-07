//! Reproducibility guarantees from `docs/outputs.md` §7.
//!
//! At a fixed thread count the pipeline must be byte-identical. Across thread
//! counts the integer output may differ by at most one LSB because parallel
//! workers only write disjoint pixels; no reduction crosses threads.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::io::tiff_read::read_tiff;
use focus_stack::output::report::sha256_file;

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

fn run_ok(args: &[&str]) {
    let output = run_str(args);
    assert!(
        output.status.success(),
        "args {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-determinism-{}-{id}-{label}",
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

#[test]
fn same_thread_count_is_byte_identical() {
    let dir = TempDir::new("same-threads");
    let first = fixture("rgb16_none.tif");
    let second = fixture("rgb16_lzw.tif");
    let out = dir.join("out.tif");
    let report = dir.join("run.json");
    let args = [
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "--method",
        "dmap",
        "--threads",
        "2",
        "--seed",
        "7",
        "--report",
        report.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ];

    run_ok(&args);
    let first_output = fs::read(&out).unwrap();
    let first_report = fs::read(&report).unwrap();

    run_ok(&args);
    assert_eq!(first_output, fs::read(&out).unwrap());
    assert_eq!(first_report, fs::read(&report).unwrap());
}

#[test]
fn report_records_params_and_hashes() {
    let dir = TempDir::new("report-identity");
    let first = fixture("rgb16_none.tif");
    let second = fixture("rgb16_lzw.tif");
    let out = dir.join("out.tif");
    let report_path = dir.join("run.json");

    run_ok(&[
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "--method",
        "dmap",
        "--threads",
        "3",
        "--seed",
        "12345",
        "--report",
        report_path.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);

    let report = fs::read_to_string(&report_path).unwrap();
    assert!(report.contains("\"schema_version\": 1"));
    assert!(report.contains(&format!("\"version\": \"{}\"", focus_stack::VERSION)));
    assert!(report.contains(&format!("\"sha256\": \"{}\"", sha256_file(&first).unwrap())));
    assert!(report.contains(&format!(
        "\"sha256\": \"{}\"",
        sha256_file(&second).unwrap()
    )));
    assert!(report.contains(&format!("\"sha256\": \"{}\"", sha256_file(&out).unwrap())));
    assert!(report.contains("\"seed\": 12345"));
    assert!(report.contains("\"threads\": 3"));
    assert!(report.contains("\"chosen\": \"dmap\""));
    assert!(!report.contains("\"timing_ms\""));
    assert!(!report.contains("\"created_at\""));
}

#[test]
fn optional_sections_are_opt_in() {
    let dir = TempDir::new("optional");
    let first = fixture("rgb16_none.tif");
    let second = fixture("rgb16_lzw.tif");
    let out = dir.join("out.tif");
    let report = dir.join("run.json");

    run_ok(&[
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "--method",
        "dmap",
        "--timings",
        "--stamp",
        "--report",
        report.to_str().unwrap(),
        "-o",
        out.to_str().unwrap(),
    ]);

    let report = fs::read_to_string(report).unwrap();
    assert!(report.contains("\"timing_ms\""));
    assert!(report.contains("\"created_at\""));
}

#[test]
fn cross_thread_pixels_stay_within_one_lsb() {
    let dir = TempDir::new("cross-threads");
    let first = fixture("rgb16_none.tif");
    let second = fixture("rgb16_lzw.tif");
    let single = dir.join("single.tif");
    let multi = dir.join("multi.tif");

    run_ok(&[
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "--method",
        "dmap",
        "--threads",
        "1",
        "-o",
        single.to_str().unwrap(),
    ]);
    run_ok(&[
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "--method",
        "dmap",
        "--threads",
        "4",
        "-o",
        multi.to_str().unwrap(),
    ]);

    let single = read_tiff(single).unwrap();
    let multi = read_tiff(multi).unwrap();
    assert_eq!(single.meta.width, multi.meta.width);
    assert_eq!(single.meta.height, multi.meta.height);
    assert_eq!(single.rgb.len(), multi.rgb.len());

    let worst = single
        .rgb
        .iter()
        .zip(&multi.rgb)
        .map(|(a, b)| a.abs_diff(*b))
        .max()
        .unwrap_or(0);
    assert!(
        worst <= 1,
        "cross-thread pixel difference is {worst} LSB, tolerance is 1"
    );
}
