//! Depth interpolation blend, depth-map encoding, and CLI output.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use focus_stack::DepthField;
use focus_stack::io::tiff_read::read_tiff;
use focus_stack::io::tiff_write::{WriteOptions, write_tiff};
use focus_stack::output::depth::{decode_depth, encode_depth, write_depth_map};
use focus_stack::stack::dmap::{CostVolume, InterpolationOptions, interpolate_frames};
use focus_stack::{Frame, FrameMeta};

const EPS: f32 = 1.0e-4;

struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "focus-stack-depth-{}-{id}-{label}",
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

fn synthetic_frame(path: PathBuf, width: u32, height: u32, ridge: u32) -> Frame {
    let mut rgb = vec![0u16; (width * height * 3) as usize];
    for y in 0..height {
        for x in 0..width {
            let bright = x.abs_diff(ridge) <= 1;
            let value = if bright {
                56_000
            } else {
                2_000 + ((x * 7 + y * 3) % 1_000) as u16
            };
            let pixel = ((y * width + x) * 3) as usize;
            rgb[pixel] = value;
            rgb[pixel + 1] = value;
            rgb[pixel + 2] = value;
        }
    }
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
            resolution: None,
        },
        rgb,
    }
}

#[test]
fn depth_values_encode_and_decode_frame_positions() {
    assert_eq!(encode_depth(0.0, 3), 0);
    assert_eq!(encode_depth(1.0, 3), 32_768);
    assert_eq!(encode_depth(2.0, 3), 65_535);
    assert_eq!(encode_depth(-4.0, 3), 0);
    assert_eq!(encode_depth(9.0, 3), 65_535);
    assert_eq!(encode_depth(f32::NAN, 3), 0);
    assert_eq!(encode_depth(1.0, 1), 0);

    assert!((decode_depth(0, 3) - 0.0).abs() <= EPS);
    assert!((decode_depth(32_768, 3) - 1.0).abs() <= EPS);
    assert!((decode_depth(65_535, 3) - 2.0).abs() <= EPS);
    assert!((decode_depth(65_535, 1) - 0.0).abs() <= EPS);
}

#[test]
fn depth_map_round_trips_through_a_grayscale_tiff() {
    const WIDTH: u32 = 9;
    const HEIGHT: u32 = 2;
    const FRAMES: usize = 3;

    let dir = TempDir::new("round-trip");
    let path = dir.join("depth.tif");
    let depth: Vec<f32> = (0..(WIDTH * HEIGHT) as usize)
        .map(|pixel| (pixel % FRAMES) as f32)
        .collect();

    let report = write_depth_map(&path, &depth, WIDTH, HEIGHT, FRAMES).unwrap();
    assert_eq!(report.frames, FRAMES);
    assert!(!report.all_zero);

    let decoded = read_tiff(&path).unwrap();
    assert_eq!(decoded.meta.width, WIDTH);
    assert_eq!(decoded.meta.height, HEIGHT);
    assert_eq!(decoded.meta.bits, 16);

    for pixel in 0..(WIDTH * HEIGHT) as usize {
        let value = decoded.rgb[pixel * 3];
        assert_eq!(value, decoded.rgb[pixel * 3 + 1]);
        assert_eq!(value, decoded.rgb[pixel * 3 + 2]);
        let frame = decode_depth(value, FRAMES);
        let expected = (pixel % FRAMES) as f32;
        assert!(
            (frame - expected).abs() <= EPS,
            "pixel {pixel} decoded to {frame}, expected {expected}"
        );
        assert_eq!(frame.round() as usize, pixel % FRAMES);
    }
}

#[test]
fn single_frame_depth_map_is_zero_and_flagged() {
    let dir = TempDir::new("single");
    let path = dir.join("depth.tif");

    let report = write_depth_map(&path, &[3.0, 1.0, 2.0, 0.0], 2, 2, 1).unwrap();
    assert!(report.all_zero);

    let decoded = read_tiff(&path).unwrap();
    assert!(decoded.rgb.iter().all(|&value| value == 0));
}

fn ridge_frames(width: usize, height: usize) -> (Vec<f32>, Vec<f32>) {
    let mut first = vec![0.05f32; width * height * 3];
    let mut second = vec![0.05f32; width * height * 3];
    for y in 0..height {
        for x in 0..width {
            let pixel = (y * width + x) * 3;
            let value = if (30..34).contains(&x) { 0.85 } else { 0.05 };
            first[pixel] = value;
            first[pixel + 1] = value;
            first[pixel + 2] = value;

            let value = if (38..42).contains(&x) { 0.85 } else { 0.05 };
            second[pixel] = value;
            second[pixel + 1] = value;
            second[pixel + 2] = value;
        }
    }
    (first, second)
}

fn rising_edges(values: &[f32], threshold: f32) -> usize {
    values
        .windows(2)
        .filter(|pair| pair[1] - pair[0] > threshold)
        .count()
}

#[test]
fn interpolation_avoids_double_edges_on_a_ridge() {
    const WIDTH: usize = 64;
    const HEIGHT: usize = 8;
    const FRAMES: usize = 2;

    let (first, second) = ridge_frames(WIDTH, HEIGHT);
    let count = WIDTH * HEIGHT;
    let depth = DepthField {
        d: vec![0.5f32; count],
        confidence: vec![1.0f32; count],
    };
    let mut aggregates = vec![vec![0.1f32; count]; FRAMES];
    aggregates[0].fill(0.9);
    let volume = CostVolume::new(&aggregates, WIDTH as u32, HEIGHT as u32);

    let blended = interpolate_frames(
        &[&first, &second],
        WIDTH as u32,
        HEIGHT as u32,
        &depth,
        &volume,
        InterpolationOptions::default(),
    );

    let row = (HEIGHT / 2) * WIDTH * 3;
    let gray: Vec<f32> = (0..WIDTH).map(|x| blended[row + x * 3]).collect();
    let naive: Vec<f32> = (0..WIDTH)
        .map(|x| 0.5 * (first[row + x * 3] + second[row + x * 3]))
        .collect();

    assert_eq!(
        rising_edges(&naive, 0.2),
        2,
        "the naive blend should show both occluder edges"
    );
    assert_eq!(
        rising_edges(&gray, 0.2),
        1,
        "the interpolation blend produced a double edge: {gray:?}"
    );
}

#[test]
fn interpolation_uses_the_depth_extremes_as_frame_positions() {
    const WIDTH: usize = 16;
    const HEIGHT: usize = 4;
    const FRAMES: usize = 3;

    let frames: Vec<Vec<f32>> = (0..FRAMES)
        .map(|frame| {
            let value = 0.2 + 0.3 * frame as f32;
            vec![value; WIDTH * HEIGHT * 3]
        })
        .collect();
    let views: Vec<&[f32]> = frames.iter().map(Vec::as_slice).collect();

    let mut aggregates = vec![vec![0.0f32; WIDTH * HEIGHT]; FRAMES];
    aggregates[0].fill(1.0);
    let volume = CostVolume::new(&aggregates, WIDTH as u32, HEIGHT as u32);

    for (depth_value, expected) in [(0.0f32, 0.2f32), (2.0f32, 0.8f32)] {
        let depth = DepthField {
            d: vec![depth_value; WIDTH * HEIGHT],
            confidence: vec![1.0f32; WIDTH * HEIGHT],
        };
        let blended = interpolate_frames(
            &views,
            WIDTH as u32,
            HEIGHT as u32,
            &depth,
            &volume,
            InterpolationOptions::default(),
        );
        for value in blended {
            assert!(
                (value - expected).abs() <= EPS,
                "depth {depth_value} produced {value}, expected {expected}"
            );
        }
    }
}

#[test]
fn cli_writes_a_depth_map_end_to_end() {
    const WIDTH: u32 = 24;
    const HEIGHT: u32 = 12;

    let dir = TempDir::new("cli");
    let mut inputs = Vec::new();
    for (index, ridge) in [6, 12, 18].into_iter().enumerate() {
        let path = dir.join(&format!("in{index}.tif"));
        let frame = synthetic_frame(path.clone(), WIDTH, HEIGHT, ridge);
        write_tiff(&path, &frame, &WriteOptions::default()).unwrap();
        inputs.push(path);
    }
    let out = dir.join("out.tif");
    let map = dir.join("depth.tif");

    let mut command = Command::new(env!("CARGO_BIN_EXE_focus-stack"));
    for input in &inputs {
        command.arg(input);
    }
    let output = command
        .args([
            "--method",
            "dmap",
            "--depth-map",
            map.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--quiet",
        ])
        .output()
        .expect("run focus-stack");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let main = read_tiff(&out).unwrap();
    assert_eq!(main.meta.width, WIDTH);
    assert_eq!(main.meta.height, HEIGHT);

    let depth = read_tiff(&map).unwrap();
    assert_eq!(depth.meta.width, WIDTH);
    assert_eq!(depth.meta.height, HEIGHT);
    assert_eq!(depth.meta.bits, 16);
    for chunk in depth.rgb.as_chunks::<3>().0 {
        assert_eq!(chunk[0], chunk[1]);
        assert_eq!(chunk[1], chunk[2]);
        let value = decode_depth(chunk[0], inputs.len());
        assert!(
            (0.0..=(inputs.len() - 1) as f32).contains(&value),
            "decoded depth {value} is outside the frame range"
        );
    }
}
