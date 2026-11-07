//! Metric reference values and `focus-stack eval` output.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::io::tiff_write::{WriteOptions, write_tiff};
use focus_stack::quality::metrics::{
    LinearImage, delta_e00, depth_edges, detail_recovery, evaluate, halo_overshoot,
    linear_rgb_to_lab, noise_gain, psnr, ssim,
};
use focus_stack::{Frame, FrameMeta};

fn gray(width: u32, height: u32, value: impl Fn(usize, usize) -> f32) -> LinearImage {
    let mut rgb = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height as usize {
        for x in 0..width as usize {
            let sample = value(x, y);
            rgb.extend_from_slice(&[sample, sample, sample]);
        }
    }
    LinearImage::new(width, height, rgb).unwrap()
}

#[test]
fn psnr_matches_analytic_value() {
    let truth = gray(8, 8, |_, _| 0.5);
    let test = gray(8, 8, |_, _| 0.6);
    // MSE = 0.1^2 = 0.01, so PSNR = -20 log10(0.1) = 20 dB.
    let value = psnr(&truth, &test).unwrap();
    assert!((value - 20.0).abs() < 1.0e-5, "PSNR {value}");
    assert!(psnr(&truth, &truth).unwrap().is_infinite());
}

#[test]
fn ssim_identity_and_constant_reference() {
    let truth = gray(8, 8, |_, _| 0.5);
    let identical = ssim(&truth, &truth).unwrap();
    assert!((identical - 1.0).abs() < 1.0e-12, "SSIM {identical}");

    let shifted = gray(8, 8, |_, _| 0.6);
    // Constant images: (2*0.5*0.6 + C1) / (0.5^2 + 0.6^2 + C1).
    let constant = ssim(&truth, &shifted).unwrap();
    let expected = (0.6001_f64) / (0.6101_f64);
    assert!(
        (constant - expected).abs() < 1.0e-7,
        "SSIM {constant}, expected {expected}"
    );
}

#[test]
fn ssim_matches_reference_implementation() {
    let width = 16;
    let height = 16;
    let truth = gray(width, height, |x, y| {
        (0.5 + 0.35 * (x as f64 * 0.7).sin() * (y as f64 * 0.4).cos()) as f32
    });
    let test = gray(width, height, |x, y| {
        let index = y * width as usize + x;
        let base = 0.5 + 0.35 * (x as f64 * 0.7).sin() * (y as f64 * 0.4).cos();
        let offset = if index.is_multiple_of(3) {
            0.03
        } else if index.is_multiple_of(5) {
            -0.02
        } else {
            0.0
        };
        (base + offset) as f32
    });

    // Reference computed with a double-precision separable 11x11 Gaussian,
    // sigma 1.5, border-truncated and renormalized.
    let value = ssim(&truth, &test).unwrap();
    assert!(
        (value - 0.992_557_295).abs() < 1.0e-6,
        "SSIM {value}, expected 0.992557295"
    );
}

#[test]
fn detail_recovery_is_gradient_ratio() {
    let width = 20;
    let height = 20;
    let truth = gray(width, height, |x, _| {
        if x < 18 {
            0.2
        } else {
            0.2 + 0.02 * (x as f32 - 17.0)
        }
    });
    let test = gray(width, height, |x, _| {
        if x < 18 {
            0.2
        } else {
            0.2 + 1.05 * 0.02 * (x as f32 - 17.0)
        }
    });

    let value = detail_recovery(&truth, &test).unwrap();
    assert!((value - 1.05).abs() < 1.0e-6, "detail recovery {value}");
    assert!((detail_recovery(&truth, &truth).unwrap() - 1.0).abs() < 1.0e-9);
}

#[test]
fn halo_overshoot_uses_depth_edge_band() {
    let width = 21;
    let height = 21;
    let truth = gray(width, height, |x, _| if x < 10 { 0.0 } else { 1.0 });
    let depth: Vec<f32> = (0..height)
        .flat_map(|_| (0..width).map(|x| if x < 10 { 0.0 } else { 1.0 }))
        .collect();
    let edges = depth_edges(&depth, width, height);
    assert!(edges[9 * width as usize + 9]);
    assert!(edges[9 * width as usize + 10]);

    let test = gray(width, height, |x, _| {
        if (4..=14).contains(&x) {
            if x < 10 { 0.02 } else { 1.02 }
        } else if x < 10 {
            0.0
        } else {
            1.0
        }
    });

    let value = halo_overshoot(&truth, &test, &edges).unwrap();
    assert!((value - 0.02).abs() < 1.0e-6, "halo overshoot {value}");
    assert!(halo_overshoot(&truth, &truth, &edges).unwrap() < 1.0e-12);
}

#[test]
fn noise_gain_is_std_ratio_in_flat_regions() {
    let width = 20;
    let height = 20;
    let truth = gray(width, height, |_, _| 0.5);
    let test = gray(width, height, |x, y| {
        let sign = if (y * width as usize + x).is_multiple_of(2) {
            1.0
        } else {
            -1.0
        };
        0.5 + 0.01 * sign
    });
    let input = gray(width, height, |x, y| {
        let sign = if (y * width as usize + x).is_multiple_of(2) {
            1.0
        } else {
            -1.0
        };
        0.5 + 0.005 * sign
    });

    let value = noise_gain(&truth, &test, &input).unwrap().unwrap();
    assert!((value - 2.0).abs() < 1.0e-6, "noise gain {value}");
}

#[test]
fn evaluate_reports_every_metric() {
    let width = 20;
    let height = 20;
    let truth = gray(width, height, |x, _| if x < 10 { 0.1 } else { 0.9 });
    let test = gray(width, height, |x, _| if x < 10 { 0.11 } else { 0.89 });
    let depth: Vec<f32> = (0..height)
        .flat_map(|_| (0..width).map(|x| if x < 10 { 0.0 } else { 1.0 }))
        .collect();
    let input = gray(width, height, |x, y| {
        if (y * width as usize + x).is_multiple_of(2) {
            0.505
        } else {
            0.495
        }
    });

    let metrics = evaluate(&truth, &test, Some(&depth), Some(&input)).unwrap();
    assert!(metrics.psnr_db.is_finite());
    assert!(metrics.ssim <= 1.0);
    assert!(metrics.detail_recovery.is_finite());
    assert!(metrics.halo_overshoot >= 0.0);
    assert!(metrics.noise_gain.is_some());
    assert!(metrics.delta_e00 >= 0.0);

    let table = metrics.table();
    for heading in [
        "PSNR",
        "SSIM",
        "Detail recovery",
        "Halo overshoot",
        "Noise gain",
        "Delta E00",
    ] {
        assert!(
            table.contains(heading),
            "table is missing {heading}:\n{table}"
        );
    }
    let json = metrics.to_json();
    for key in [
        "\"psnr_db\"",
        "\"ssim\"",
        "\"detail_recovery\"",
        "\"halo_overshoot\"",
        "\"noise_gain\"",
        "\"delta_e00\"",
    ] {
        assert!(json.contains(key), "JSON is missing {key}:\n{json}");
    }
}

#[test]
fn delta_e00_matches_sharma_reference_pairs() {
    let pairs = [
        ((50.0, 2.6772, -79.7751), (50.0, 0.0, -82.7485), 2.0425),
        ((50.0, 3.1571, -77.2803), (50.0, 0.0, -82.7485), 2.8615),
        ((50.0, 2.8361, -74.0200), (50.0, 0.0, -82.7485), 3.4412),
        ((50.0, -1.3802, -84.2814), (50.0, 0.0, -82.7485), 1.0000),
        ((50.0, -1.1848, -84.8006), (50.0, 0.0, -82.7485), 1.0000),
        ((50.0, -0.9009, -85.5211), (50.0, 0.0, -82.7485), 1.0000),
        ((50.0, 0.0, 0.0), (50.0, -1.0, 2.0), 2.3669),
        ((50.0, -1.0, 2.0), (50.0, 0.0, 0.0), 2.3669),
        ((50.0, 2.49, -0.001), (50.0, -2.49, 0.0009), 7.1792),
        ((50.0, 2.49, -0.001), (50.0, -2.49, 0.001), 7.1792),
        ((50.0, 2.49, -0.001), (50.0, -2.49, 0.0011), 7.2195),
        ((50.0, 2.49, -0.001), (50.0, -2.49, 0.0012), 7.2195),
        ((50.0, -0.001, 2.49), (50.0, 0.0009, -2.49), 4.8045),
        ((50.0, -0.001, 2.49), (50.0, 0.001, -2.49), 4.8045),
        ((50.0, -0.001, 2.49), (50.0, 0.0011, -2.49), 4.7461),
        ((50.0, 2.5, 0.0), (50.0, 0.0, -2.5), 4.3065),
        ((50.0, 2.5, 0.0), (73.0, 25.0, -18.0), 27.1492),
        ((50.0, 2.5, 0.0), (61.0, -5.0, 29.0), 22.8977),
        ((50.0, 2.5, 0.0), (56.0, -27.0, -3.0), 31.9030),
        ((50.0, 2.5, 0.0), (58.0, 24.0, 15.0), 19.4535),
        ((50.0, 2.5, 0.0), (50.0, 3.1736, 0.5854), 1.0000),
        ((50.0, 2.5, 0.0), (50.0, 3.2972, 0.0), 1.0000),
        ((50.0, 2.5, 0.0), (50.0, 1.8634, 0.5757), 1.0000),
        ((50.0, 2.5, 0.0), (50.0, 3.2592, 0.3350), 1.0000),
        (
            (60.2574, -34.0099, 36.2677),
            (60.4626, -34.1751, 39.4387),
            1.2644,
        ),
        (
            (63.0109, -31.0961, -5.8663),
            (62.8187, -29.7946, -4.0864),
            1.2630,
        ),
        (
            (61.2901, 3.7196, -5.3901),
            (61.4292, 2.2480, -4.9620),
            1.8731,
        ),
        (
            (35.0831, -44.1164, 3.7933),
            (35.0232, -40.0716, 1.5901),
            1.8645,
        ),
        (
            (22.7233, 20.0904, -46.6940),
            (23.0331, 14.9730, -42.5619),
            2.0373,
        ),
        (
            (36.4612, 47.8580, 18.3852),
            (36.2715, 50.5065, 21.2231),
            1.4146,
        ),
        (
            (90.8027, -2.0831, 1.4410),
            (91.1528, -1.6435, 0.0447),
            1.4441,
        ),
        (
            (90.9257, -0.5406, -0.9208),
            (88.6381, -0.8985, -0.7239),
            1.5381,
        ),
        (
            (6.7747, -0.2908, -2.4247),
            (5.8714, -0.0985, -2.2286),
            0.6377,
        ),
        (
            (2.0776, 0.0795, -1.1350),
            (0.9033, -0.0636, -0.5514),
            0.9082,
        ),
    ];

    for (lab1, lab2, expected) in pairs {
        let value = delta_e00([lab1.0, lab1.1, lab1.2], [lab2.0, lab2.1, lab2.2]);
        assert!(
            (value - expected).abs() < 1.0e-4,
            "delta E00 {value}, expected {expected} for {lab1:?} vs {lab2:?}"
        );
    }
}

#[test]
fn linear_white_is_lab_white() {
    let [l, a, b] = linear_rgb_to_lab([1.0, 1.0, 1.0]);
    assert!((l - 100.0).abs() < 1.0e-4, "L* {l}");
    assert!(a.abs() < 1.0e-4, "a* {a}");
    assert!(b.abs() < 1.0e-4, "b* {b}");
}

#[test]
fn rejects_mismatched_dimensions() {
    let small = gray(4, 4, |_, _| 0.5);
    let large = gray(5, 4, |_, _| 0.5);
    assert!(psnr(&small, &large).is_err());
    assert!(ssim(&small, &large).is_err());
    assert!(evaluate(&small, &large, None, None).is_err());
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-metrics-{}-{id}-{label}",
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

fn write_solid(path: PathBuf, value: u16) {
    let width = 8;
    let height = 8;
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
        rgb: vec![value; (width * height * 3) as usize],
    };
    write_tiff(&path, &frame, &WriteOptions::default()).unwrap();
}

#[test]
fn eval_cli_prints_table_and_json() {
    let dir = TempDir::new("cli");
    let truth = dir.join("truth.tif");
    let test = dir.join("test.tif");
    write_solid(truth.clone(), 32_768);
    write_solid(test.clone(), 38_000);

    let table = Command::new(env!("CARGO_BIN_EXE_focus-stack"))
        .args([
            "eval",
            "--truth",
            truth.to_str().unwrap(),
            "--test",
            test.to_str().unwrap(),
        ])
        .output()
        .expect("run focus-stack eval");
    assert!(
        table.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&table.stderr)
    );
    let text = String::from_utf8_lossy(&table.stdout);
    assert!(text.contains("PSNR (dB)"), "table:\n{text}");
    assert!(text.contains("Delta E00"), "table:\n{text}");

    let json = Command::new(env!("CARGO_BIN_EXE_focus-stack"))
        .args([
            "eval",
            "--json",
            "--truth",
            truth.to_str().unwrap(),
            "--test",
            test.to_str().unwrap(),
        ])
        .output()
        .expect("run focus-stack eval --json");
    assert!(
        json.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&json.stderr)
    );
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(text.contains("\"psnr_db\""), "JSON:\n{text}");
    assert!(text.contains("\"noise_gain\": null"), "JSON:\n{text}");
}
