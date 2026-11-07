use focus_stack::focus::confidence::{
    ConfidenceMap, ConfidenceOptions, EnergyStatistics, FrameCalibration, SATURATION_THRESHOLD,
    aggregate_sigma, guided_filter, saturation_mask,
};
use focus_stack::focus::coverage::{COVERAGE_TAU, CoverageMask};
use focus_stack::focus::energy::{EnergyMap, EnergyMaps, EnergyOptions, aggregate_levels};
use focus_stack::focus::noise::{DEFAULT_NOISE_K, estimate};
use focus_stack::focus::pyramid::Pyramid;

fn textured_scene(width: usize, height: usize) -> Vec<f32> {
    let horizon = height * 3 / 4;
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let value = if y < horizon {
                0.62
            } else {
                let (fx, fy) = (x as f32, y as f32);
                0.5 + 0.16 * (fx * 0.53 + fy * 0.19).sin()
                    + 0.12 * (fx * 0.17 - fy * 0.61).cos()
                    + 0.08 * ((fx + fy) * 0.83).sin()
            };
            out.push(value.clamp(0.0, 1.0));
        }
    }
    out
}

fn gaussian_blur(input: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    if sigma <= 0.0 {
        return input.to_vec();
    }

    let radius = (3.0 * sigma).ceil() as isize;
    let mut kernel = Vec::with_capacity((2 * radius + 1) as usize);
    let mut total = 0.0f32;
    for offset in -radius..=radius {
        let value = (-(offset * offset) as f32 / (2.0 * sigma * sigma)).exp();
        kernel.push(value);
        total += value;
    }
    for value in &mut kernel {
        *value /= total;
    }

    let mut horizontal = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for (index, &weight) in kernel.iter().enumerate() {
                let offset = index as isize - radius;
                let sx = (x as isize + offset).clamp(0, width as isize - 1) as usize;
                sum += weight * input[y * width + sx];
            }
            horizontal[y * width + x] = sum;
        }
    }

    let mut out = vec![0.0f32; width * height];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            for (index, &weight) in kernel.iter().enumerate() {
                let offset = index as isize - radius;
                let sy = (y as isize + offset).clamp(0, height as isize - 1) as usize;
                sum += weight * horizontal[sy * width + x];
            }
            out[y * width + x] = sum;
        }
    }

    out
}

/// Deterministic standard normal generator, so the sky carries realistic
/// per-frame sensor noise rather than only floating-point blur artifacts.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn uniform(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }

    fn normal(&mut self) -> f32 {
        let u1 = self.uniform().max(f32::MIN_POSITIVE);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

fn blur_ladder(
    scene: &[f32],
    width: usize,
    height: usize,
    sigmas: &[f32],
    noise_sigma: f32,
) -> Vec<Vec<f32>> {
    let horizon = height * 3 / 4;
    sigmas
        .iter()
        .enumerate()
        .map(|(frame, &sigma)| {
            let blurred = gaussian_blur(scene, width, height, sigma);
            let mut rng = Lcg::new(0x5eed + frame as u64);
            blurred
                .iter()
                .enumerate()
                .map(|(index, &value)| {
                    // Defocus does not change a flat sky; keeping the sky
                    // identical across frames isolates the best-vs-second
                    // margin from blur-kernel normalization artifacts.
                    if index / width < horizon {
                        scene[index]
                    } else {
                        (value + noise_sigma * rng.normal()).clamp(0.0, 1.0)
                    }
                })
                .collect()
        })
        .collect()
}

fn region_mean(values: &[f32], width: usize, x0: usize, x1: usize, y0: usize, y1: usize) -> f32 {
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for y in y0..y1 {
        for x in x0..x1 {
            sum += values[y * width + x];
            count += 1;
        }
    }
    sum / count as f32
}

fn analyze(frames: &[Vec<f32>], guide: &[f32], width: u32, height: u32) -> ConfidenceMap {
    let mut energies = Vec::with_capacity(frames.len());
    let mut calibrations = Vec::with_capacity(frames.len());

    for frame in frames {
        let mut pyramid = Pyramid::new();
        pyramid.build(frame, width, height);
        let noise = estimate(&pyramid, DEFAULT_NOISE_K);
        let maps = EnergyMaps::compute(
            &pyramid,
            &noise,
            EnergyOptions {
                radius: Some(6),
                contrast_normalize: false,
            },
        );
        let energy = maps.aggregate();
        let sigmas: Vec<f32> = noise.sigmas().collect();
        calibrations.push(FrameCalibration::new(&energy, &sigmas));
        energies.push(energy);
    }

    ConfidenceMap::compute(
        &energies,
        guide,
        width,
        height,
        &calibrations,
        None,
        ConfidenceOptions::default(),
    )
}

#[test]
fn margin_is_best_vs_second_with_low_index_ties() {
    let guide = vec![0.5f32; 4];
    let energies = vec![
        vec![1.0, 1.0, 0.0, 2.0],
        vec![0.0, 1.0, 0.0, 1.0],
        vec![0.0, 1.0, 0.0, 1.0],
    ];

    let map = ConfidenceMap::compute(
        &energies,
        &guide,
        4,
        1,
        &[],
        None,
        ConfidenceOptions::default(),
    );

    assert_eq!(map.best_frame, vec![0, 0, 0, 0]);
    assert!(map.margin[0] > 0.999);
    assert_eq!(map.margin[1], 0.0);
    assert_eq!(map.margin[2], 0.0);
    assert!((map.margin[3] - 0.5).abs() < 1e-5);
    assert!(
        map.confidence
            .iter()
            .all(|value| (0.0..=1.0).contains(value))
    );
}

#[test]
fn guided_filter_preserves_constants_and_identity_radius() {
    let (width, height) = (32usize, 24usize);
    let guide: Vec<f32> = (0..width * height)
        .map(|index| {
            let x = (index % width) as f32;
            let y = (index / width) as f32;
            0.5 + 0.3 * (x * 0.7).sin() * (y * 0.4).cos()
        })
        .collect();
    let input = vec![0.7f32; width * height];

    let smoothed = guided_filter(&guide, &input, width as u32, height as u32, 8, 1e-3);
    for value in smoothed {
        assert!(
            (value - 0.7).abs() < 1e-4,
            "constant was distorted: {value}"
        );
    }

    let identity = guided_filter(&guide, &input, width as u32, height as u32, 0, 1e-3);
    for (actual, expected) in identity.iter().zip(&input) {
        assert!((actual - expected).abs() < 1e-5);
    }
}

#[test]
fn saturation_mask_uses_any_channel_and_the_documented_threshold() {
    let rgb = [
        0.10, 0.50, 0.30, // plain
        0.20, 0.996, 0.30, // saturated green
        0.995, 0.00, 0.00, // exactly at the threshold
    ];
    let mask = saturation_mask(&rgb, 3, 1, 3);
    assert_eq!(mask, vec![false, true, true]);
    assert_eq!(SATURATION_THRESHOLD, 0.995);
}

#[test]
fn saturation_gate_zeroes_the_winning_pixel() {
    let guide = vec![0.5f32; 4];
    let energies = vec![vec![2.0f32; 4], vec![1.0f32; 4]];
    let saturation = vec![vec![false, false, true, false], vec![false; 4]];

    let map = ConfidenceMap::compute(
        &energies,
        &guide,
        4,
        1,
        &[],
        Some(&saturation),
        ConfidenceOptions::default(),
    );

    assert_eq!(map.confidence[2], 0.0);
    for index in [0usize, 1, 3] {
        assert!(map.confidence[index] > 0.0);
    }
}

#[test]
fn confidence_is_high_on_texture_and_near_zero_on_synthetic_sky() {
    let (width, height) = (256usize, 256usize);
    let scene = textured_scene(width, height);
    let sigmas = [0.0f32, 1.0, 2.0, 4.0];
    let frames = blur_ladder(&scene, width, height, &sigmas, 0.002);

    let map = analyze(&frames, &scene, width as u32, height as u32);
    let horizon = height * 3 / 4;

    // Stay clear of the horizon: coarse pyramid levels deliberately bridge
    // texture across a scale of a few pixels, and the guided filter has r = 8.
    let sky = region_mean(&map.confidence, width, 16, width - 16, 0, horizon / 2);
    let texture = region_mean(
        &map.confidence,
        width,
        16,
        width - 16,
        horizon + 16,
        height - 16,
    );

    assert!(
        sky < 0.01,
        "synthetic sky confidence {sky} is not near zero"
    );
    assert!(
        texture > 0.30,
        "textured region confidence {texture} is not high enough"
    );
    assert!(
        map.confidence
            .iter()
            .all(|value| value.is_finite() && (0.0..=1.0).contains(value))
    );
}

#[test]
fn frame_ranking_accuracy_on_a_blur_ladder_exceeds_95_percent() {
    let (width, height) = (256usize, 256usize);
    let scene = textured_scene(width, height);
    let sigmas = [0.0f32, 1.0, 2.0, 4.0];
    let frames = blur_ladder(&scene, width, height, &sigmas, 0.002);

    let map = analyze(&frames, &scene, width as u32, height as u32);
    let horizon = height * 3 / 4;

    let mut correct = 0usize;
    let mut total = 0usize;
    for y in horizon + 16..height - 16 {
        for x in 16..width - 16 {
            let index = y * width + x;
            total += 1;
            if map.best_frame[index] == 0 {
                correct += 1;
            }
        }
    }

    let accuracy = correct as f32 / total as f32;
    assert!(
        accuracy > 0.95,
        "blur-ladder ranking accuracy {:.2}% is not above 95%",
        accuracy * 100.0
    );
}

#[test]
fn coverage_mask_reports_fraction_and_connected_boxes() {
    let (width, height) = (8usize, 6usize);
    let mut energy = vec![1.0f32; width * height];
    for y in 1..4 {
        for x in 1..4 {
            energy[y * width + x] = 0.0;
        }
    }
    energy[4 * width + 6] = 0.0;

    let mask = CoverageMask::from_uniform_floor(&energy, width as u32, height as u32, 0.5);
    assert_eq!(mask.gap_count(), 10);
    assert_eq!(mask.len(), width * height);
    assert!((mask.gap_fraction() - 10.0 / 48.0).abs() < 1e-6);
    assert!((mask.covered_fraction() - 38.0 / 48.0).abs() < 1e-6);
    assert!(mask.is_significant(0.1));
    assert!(!mask.is_significant(0.5));

    let boxes = mask.gap_boxes();
    assert_eq!(boxes.len(), 2);
    assert_eq!((boxes[0].x0, boxes[0].y0), (1, 1));
    assert_eq!((boxes[0].x1, boxes[0].y1), (4, 4));
    assert_eq!(boxes[0].area(), 9);
    assert_eq!((boxes[1].x0, boxes[1].y0), (6, 4));
    assert_eq!((boxes[1].x1, boxes[1].y1), (7, 5));
    assert_eq!(boxes[1].area(), 1);
}

#[test]
fn coverage_uses_the_winning_frames_calibration() {
    let map = ConfidenceMap {
        width: 2,
        height: 1,
        best_frame: vec![0, 1],
        best_energy: vec![0.5, 2.0],
        margin: vec![0.5, 0.5],
        confidence: vec![0.5, 0.5],
    };
    let calibrations = [
        FrameCalibration {
            sigma: 0.1,
            stats: EnergyStatistics::default(),
        },
        FrameCalibration {
            sigma: 1.0,
            stats: EnergyStatistics::default(),
        },
    ];

    let mask = CoverageMask::from_confidence(&map, &calibrations, COVERAGE_TAU);
    assert_eq!(mask.gap, vec![false, true]);
}

#[test]
fn calibration_aggregates_sigma_and_reports_statistics() {
    let stats = EnergyStatistics::from_slice(&[0.0, 1.0, 2.0, 3.0, 4.0]);
    assert_eq!(stats.min, 0.0);
    assert_eq!(stats.max, 4.0);
    assert_eq!(stats.mean, 2.0);
    assert_eq!(stats.median, 2.0);
    assert_eq!(stats.p95, 4.0);

    let sigma = aggregate_sigma(&[0.1, 0.2, 0.3]);
    let expected = (0.1f32 * 0.1 + 0.9 * 0.2 * 0.2).sqrt();
    assert!((sigma - expected).abs() < 1e-6, "{sigma} != {expected}");

    let calibration = FrameCalibration::new(&[1.0, 2.0, 3.0], &[0.1, 0.2, 0.3]);
    assert_eq!(calibration.stats.median, 2.0);
    assert!((calibration.coverage_floor(COVERAGE_TAU) - 4.0 * expected * expected).abs() < 1e-6);
}

#[test]
fn aggregation_samples_a_coarse_level_across_the_full_width() {
    // Regression for the §6 aggregation: each level uses its own source-to-
    // destination ratio, so a quarter-width level must span all eight columns
    // rather than clamp at the halfway point.
    let levels = vec![
        EnergyMap {
            width: 8,
            height: 1,
            radius: 2,
            energy: vec![0.0; 8],
        },
        EnergyMap {
            width: 4,
            height: 1,
            radius: 2,
            energy: vec![0.0; 4],
        },
        EnergyMap {
            width: 2,
            height: 1,
            radius: 2,
            energy: vec![1.0, 3.0],
        },
        EnergyMap {
            width: 1,
            height: 1,
            radius: 2,
            energy: vec![100.0],
        },
    ];

    let aggregated = aggregate_levels(&levels);
    let expected = [1.0f32, 1.5, 2.0, 2.5, 3.0, 3.0, 3.0, 3.0];
    for (index, (&actual, &value)) in aggregated.iter().zip(&expected).enumerate() {
        assert!(
            (actual - 0.8 * value).abs() < 1e-6,
            "column {index}: {actual} != {}",
            0.8 * value
        );
    }
}
