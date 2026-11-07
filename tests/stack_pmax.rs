//! PMax pyramid blending, tone anchoring, and `--method pmax` integration.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::color::luma::{encoded_luma_buffer, linear_luma};
use focus_stack::color::transfer::{srgb_decode, srgb_encode};
use focus_stack::focus::confidence::{ConfidenceMap, ConfidenceOptions, FrameCalibration};
use focus_stack::focus::energy::{EnergyMaps, EnergyOptions};
use focus_stack::focus::noise::{self, DEFAULT_NOISE_K};
use focus_stack::focus::pyramid::Pyramid;
use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{WriteOptions, write_tiff};
use focus_stack::stack::pmax::{DEFAULT_BETA, WeightOptions, reconstruct, stack};
use focus_stack::{Frame, FrameMeta};

const WIDTH: usize = 64;
const HEIGHT: usize = 64;
const SKY_ROWS: usize = HEIGHT * 2 / 5;

fn ground_truth() -> Vec<f32> {
    let mut values = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let value = if y < SKY_ROWS {
                0.42 + 0.05 * (x as f32 / WIDTH as f32)
            } else {
                let fx = x as f32;
                let fy = y as f32;
                0.50 + 0.20 * (fx * 1.7).sin() * (fy * 0.9).cos()
                    + 0.12 * (fx * 0.8 + fy * 1.3).sin()
            };
            values[y * WIDTH + x] = value.clamp(0.0, 1.0);
        }
    }
    values
}

fn blur(src: &[f32], sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as isize;
    let mut kernel = Vec::with_capacity((2 * radius + 1) as usize);
    let denominator = 2.0 * sigma * sigma;
    let mut total = 0.0;
    for offset in -radius..=radius {
        let weight = (-((offset * offset) as f32) / denominator).exp();
        kernel.push(weight);
        total += weight;
    }
    for weight in &mut kernel {
        *weight /= total;
    }

    let mut horizontal = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut sum = 0.0;
            for (index, &weight) in kernel.iter().enumerate() {
                let dx = x as isize + index as isize - radius;
                let sx = dx.clamp(0, WIDTH as isize - 1) as usize;
                sum += weight * src[y * WIDTH + sx];
            }
            horizontal[y * WIDTH + x] = sum;
        }
    }

    let mut out = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut sum = 0.0;
            for (index, &weight) in kernel.iter().enumerate() {
                let dy = y as isize + index as isize - radius;
                let sy = dy.clamp(0, HEIGHT as isize - 1) as usize;
                sum += weight * horizontal[sy * WIDTH + x];
            }
            out[y * WIDTH + x] = sum;
        }
    }
    out
}

/// Deterministic pseudo-random sample in `[-1, 1]`.
fn pseudo_noise(seed: u32, index: usize) -> f32 {
    let mut value = seed.wrapping_add((index as u32).wrapping_mul(0x9E37_79B9));
    value ^= value >> 16;
    value = value.wrapping_mul(0x7FEB_352D);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846C_A68B);
    value ^= value >> 16;
    (value as f32 / u32::MAX as f32) * 2.0 - 1.0
}

fn synthetic_frames() -> (Vec<f32>, Vec<Vec<f32>>) {
    let sharp = ground_truth();
    let soft = blur(&sharp, 1.8);

    let mut first = vec![0.0f32; WIDTH * HEIGHT];
    let mut second = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            let (a, b) = if y < SKY_ROWS {
                (sharp[index], soft[index])
            } else {
                (soft[index], sharp[index])
            };
            first[index] = (a + 0.02 * pseudo_noise(1, index)).clamp(0.0, 1.0);
            second[index] = (b + 0.02 * pseudo_noise(2, index)).clamp(0.0, 1.0);
        }
    }
    (sharp, vec![first, second])
}

fn encoded_rgb(frame: &[f32]) -> Vec<f32> {
    frame
        .iter()
        .flat_map(|&value| {
            let encoded = srgb_encode(value);
            [encoded, encoded, encoded]
        })
        .collect()
}

fn pmax_stack(frames: &[Vec<f32>]) -> Vec<f32> {
    let width = WIDTH as u32;
    let height = HEIGHT as u32;

    let mut encoded = Vec::with_capacity(frames.len());
    let mut energies = Vec::with_capacity(frames.len());
    let mut aggregates = Vec::with_capacity(frames.len());
    let mut calibrations = Vec::with_capacity(frames.len());

    for frame in frames {
        let rgb = encoded_rgb(frame);
        let mut luma = vec![0.0f32; WIDTH * HEIGHT];
        encoded_luma_buffer(&rgb, &mut luma);

        let mut pyramid = Pyramid::new();
        pyramid.build(&luma, width, height);
        let noise = noise::estimate(&pyramid, DEFAULT_NOISE_K);
        let energy = EnergyMaps::compute(&pyramid, &noise, EnergyOptions::default());
        let aggregate = energy.aggregate();
        let sigmas: Vec<f32> = noise.sigmas().collect();

        calibrations.push(FrameCalibration::new(&aggregate, &sigmas));
        aggregates.push(aggregate);
        energies.push(energy);
        encoded.push(rgb);
    }

    let mut guide = vec![0.0f32; WIDTH * HEIGHT];
    encoded_luma_buffer(&encoded[0], &mut guide);
    let confidence = ConfidenceMap::compute(
        &aggregates,
        &guide,
        width,
        height,
        &calibrations,
        None,
        ConfidenceOptions::default(),
    );

    let linear: Vec<Vec<f32>> = encoded
        .iter()
        .map(|rgb| rgb.iter().map(|&value| srgb_decode(value)).collect())
        .collect();
    let views: Vec<&[f32]> = linear.iter().map(Vec::as_slice).collect();
    stack(
        &views,
        width,
        height,
        &energies,
        &confidence,
        WeightOptions { beta: DEFAULT_BETA },
    )
}

/// Pick the largest-magnitude Laplacian coefficient at every level and pixel.
fn naive_laplacian_max(frames: &[Vec<f32>]) -> Vec<f32> {
    let width = WIDTH as u32;
    let height = HEIGHT as u32;
    let pyramids: Vec<Pyramid> = frames
        .iter()
        .map(|frame| {
            let mut pyramid = Pyramid::new();
            pyramid.build(frame, width, height);
            pyramid
        })
        .collect();

    let dimensions: Vec<(u32, u32)> = pyramids[0]
        .levels()
        .iter()
        .map(|level| (level.width, level.height))
        .collect();
    let mut coefficients: Vec<Vec<f32>> = dimensions
        .iter()
        .map(|&(level_width, level_height)| {
            vec![0.0f32; (level_width as usize) * (level_height as usize)]
        })
        .collect();

    for (level_index, out) in coefficients.iter_mut().enumerate() {
        for (pixel, value) in out.iter_mut().enumerate() {
            let mut best = 0.0;
            let mut best_magnitude = -1.0;
            for pyramid in &pyramids {
                let candidate = pyramid.levels()[level_index].laplacian[pixel];
                if candidate.abs() > best_magnitude {
                    best_magnitude = candidate.abs();
                    best = candidate;
                }
            }
            *value = best;
        }
    }

    reconstruct(&coefficients, &dimensions)
}

fn windowed_ssim(a: &[f32], b: &[f32]) -> f32 {
    const C1: f32 = 0.01 * 0.01;
    const C2: f32 = 0.03 * 0.03;
    const WINDOW: usize = 8;

    let mut total = 0.0f32;
    let mut windows = 0usize;
    for by in (0..HEIGHT).step_by(WINDOW) {
        for bx in (0..WIDTH).step_by(WINDOW) {
            let mut sum_a = 0.0f32;
            let mut sum_b = 0.0f32;
            let mut sum_aa = 0.0f32;
            let mut sum_bb = 0.0f32;
            let mut sum_ab = 0.0f32;
            let mut count = 0.0f32;

            for y in by..(by + WINDOW).min(HEIGHT) {
                for x in bx..(bx + WINDOW).min(WIDTH) {
                    let index = y * WIDTH + x;
                    let (va, vb) = (a[index], b[index]);
                    sum_a += va;
                    sum_b += vb;
                    sum_aa += va * va;
                    sum_bb += vb * vb;
                    sum_ab += va * vb;
                    count += 1.0;
                }
            }

            let mean_a = sum_a / count;
            let mean_b = sum_b / count;
            let variance_a = (sum_aa / count - mean_a * mean_a).max(0.0);
            let variance_b = (sum_bb / count - mean_b * mean_b).max(0.0);
            let covariance = sum_ab / count - mean_a * mean_b;

            let numerator = (2.0 * mean_a * mean_b + C1) * (2.0 * covariance + C2);
            let denominator =
                (mean_a * mean_a + mean_b * mean_b + C1) * (variance_a + variance_b + C2);
            total += numerator / denominator;
            windows += 1;
        }
    }
    total / windows as f32
}

fn rms_error(a: &[f32], b: &[f32], rows: usize) -> f32 {
    let count = rows * WIDTH;
    let sum: f32 = (0..count).map(|index| (a[index] - b[index]).powi(2)).sum();
    (sum / count as f32).sqrt()
}

#[test]
fn reconstruct_matches_the_analysis_pyramid() {
    let mut source = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            source[y * WIDTH + x] = 0.5 + 0.2 * ((x as f32) * 1.3).sin() * ((y as f32) * 0.7).cos();
        }
    }

    let mut pyramid = Pyramid::new();
    pyramid.build(&source, WIDTH as u32, HEIGHT as u32);
    let dimensions: Vec<(u32, u32)> = pyramid
        .levels()
        .iter()
        .map(|level| (level.width, level.height))
        .collect();
    let coefficients: Vec<Vec<f32>> = pyramid
        .levels()
        .iter()
        .map(|level| level.laplacian.clone())
        .collect();

    let ours = reconstruct(&coefficients, &dimensions);
    let reference = pyramid.reconstruct();
    assert_eq!(ours.len(), reference.len());
    for (index, (ours, reference)) in ours.iter().zip(&reference).enumerate() {
        assert!(
            (ours - reference).abs() < 1.0e-5,
            "sample {index}: {ours} vs {reference}"
        );
    }
}

#[test]
fn pmax_beats_naive_laplacian_max_and_keeps_sky_calm() {
    let (sharp, frames) = synthetic_frames();

    let pmax = pmax_stack(&frames);
    let pmax_luma: Vec<f32> = pmax
        .as_chunks::<3>()
        .0
        .iter()
        .map(|pixel| linear_luma(pixel[0], pixel[1], pixel[2]))
        .collect();
    let naive = naive_laplacian_max(&frames);

    let sharp_encoded: Vec<f32> = sharp.iter().map(|&value| srgb_encode(value)).collect();
    let pmax_encoded: Vec<f32> = pmax_luma
        .iter()
        .map(|&value| srgb_encode(value.clamp(0.0, 1.0)))
        .collect();
    let naive_encoded: Vec<f32> = naive
        .iter()
        .map(|&value| srgb_encode(value.clamp(0.0, 1.0)))
        .collect();

    let pmax_ssim = windowed_ssim(&sharp_encoded, &pmax_encoded);
    let naive_ssim = windowed_ssim(&sharp_encoded, &naive_encoded);
    assert!(
        pmax_ssim > naive_ssim,
        "PMax SSIM {pmax_ssim} must beat naive Laplacian-max {naive_ssim}"
    );

    let sharp_sky: Vec<f32> = sharp_encoded[..SKY_ROWS * WIDTH].to_vec();
    let pmax_sky = rms_error(&sharp_sky, &pmax_encoded[..SKY_ROWS * WIDTH], SKY_ROWS);
    let naive_sky = rms_error(&sharp_sky, &naive_encoded[..SKY_ROWS * WIDTH], SKY_ROWS);
    assert!(
        pmax_sky < naive_sky,
        "sky noise {pmax_sky} must stay below naive {naive_sky}"
    );
    assert!(pmax_sky < 0.02, "sky noise blew up: {pmax_sky}");
}

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-pmax-{}-{id}-{label}",
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

fn input_frame(path: PathBuf, seed: f32) -> Frame {
    let mut rgb = Vec::with_capacity(WIDTH * HEIGHT * 3);
    for index in 0..(WIDTH * HEIGHT * 3) {
        let x = (index % WIDTH) as f32;
        let y = ((index / WIDTH) % HEIGHT) as f32;
        let value = 0.5 + 0.2 * (x * 0.7 + y * 0.4 + seed).sin();
        rgb.push((value.clamp(0.0, 1.0) * 65535.0).round() as u16);
    }

    Frame {
        meta: FrameMeta {
            path,
            width: WIDTH as u32,
            height: HEIGHT as u32,
            bits: 16,
            icc: None,
            orientation: 1,
            exif: None,
            xmp: None,
            resolution: None,
        },
        rgb,
    }
}

#[test]
fn method_pmax_writes_valid_16_bit_output() {
    let dir = TempDir::new("cli");
    let first = dir.join("first.tif");
    let second = dir.join("second.tif");
    write_tiff(
        &first,
        &input_frame(first.clone(), 0.0),
        &WriteOptions::default(),
    )
    .unwrap();
    write_tiff(
        &second,
        &input_frame(second.clone(), 1.7),
        &WriteOptions::default(),
    )
    .unwrap();

    let out = dir.join("out.tif");
    let output = Command::new(env!("CARGO_BIN_EXE_focus-stack"))
        .arg(&first)
        .arg(&second)
        .arg("-o")
        .arg(&out)
        .arg("--method")
        .arg("pmax")
        .arg("--quiet")
        .output()
        .expect("run focus-stack");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let written = read_tiff(&out).unwrap();
    assert_eq!(written.meta.width, WIDTH as u32);
    assert_eq!(written.meta.height, HEIGHT as u32);
    assert_eq!(written.meta.bits, 16);
    assert_eq!(written.rgb.len(), WIDTH * HEIGHT * 3);
    assert!(written.rgb.iter().any(|&value| value != 0));
}
