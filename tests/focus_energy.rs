use std::time::{Duration, Instant};

use focus_stack::focus::energy::{
    AGGREGATION_WEIGHTS, CONTRAST_EPSILON, EnergyMap, EnergyMaps, EnergyOptions, aggregate_levels,
    aggregation_weight, auto_radius, box_filter, energy_map, level_radius,
};
use focus_stack::focus::noise::{DEFAULT_NOISE_K, NoiseEstimate, NoiseModel, estimate};
use focus_stack::focus::pyramid::{Pyramid, PyramidLevel};

fn brute_box_filter(input: &[f32], width: usize, height: usize, radius: usize) -> Vec<f32> {
    let mut out = vec![0.0f32; width * height];
    for y in 0..height {
        let y0 = y.saturating_sub(radius);
        let y1 = (y + radius + 1).min(height);
        for x in 0..width {
            let x0 = x.saturating_sub(radius);
            let x1 = (x + radius + 1).min(width);
            let mut sum = 0.0f64;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    sum += f64::from(input[sy * width + sx]);
                }
            }
            out[y * width + x] = (sum / ((x1 - x0) * (y1 - y0)) as f64) as f32;
        }
    }
    out
}

fn textured_image(width: usize, height: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let (fx, fy) = (x as f32, y as f32);
            out.push(
                (0.5 + 0.12 * (fx * 0.41 + fy * 0.27).sin()
                    + 0.10 * (fx * 0.13 - fy * 0.53).cos()
                    + 0.06 * ((fx + fy) * 0.71).sin()
                    + 0.04 * (fx * 1.03 - fy * 0.87).cos())
                .clamp(0.0, 1.0),
            );
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

fn mean(values: &[f32]) -> f32 {
    values.iter().sum::<f32>() / values.len() as f32
}

fn flat_level(width: u32, height: u32, gaussian: f32, laplacian: f32) -> PyramidLevel {
    let count = (width * height) as usize;
    PyramidLevel {
        width,
        height,
        gaussian: vec![gaussian; count],
        laplacian: vec![laplacian; count],
    }
}

#[test]
fn box_filter_matches_brute_force() {
    let (width, height) = (9usize, 7usize);
    let input: Vec<f32> = (0..width * height)
        .map(|index| {
            let x = (index % width) as f32;
            let y = (index / width) as f32;
            0.3 + 0.2 * (x * 0.7).sin() + 0.1 * (y * 0.4).cos()
        })
        .collect();

    for radius in [0u32, 1, 2, 4, 20] {
        let actual = box_filter(&input, width as u32, height as u32, radius);
        let expected = brute_box_filter(&input, width, height, radius as usize);
        for (index, (&a, &b)) in actual.iter().zip(&expected).enumerate() {
            assert!(
                (a - b).abs() < 1e-5,
                "radius {radius}, pixel {index}: {a} != {b}"
            );
        }
    }
}

#[test]
fn radius_schedule_matches_documentation() {
    assert_eq!(auto_radius(6000, 4000), 12);
    assert_eq!(auto_radius(9504, 6336), 17);
    assert_eq!(auto_radius(20_000, 20_000), 32);
    assert_eq!(auto_radius(100, 100), 12);

    assert_eq!(level_radius(32, 0), 32);
    assert_eq!(level_radius(32, 1), 16);
    assert_eq!(level_radius(32, 2), 8);
    assert_eq!(level_radius(32, 3), 4);
    assert_eq!(level_radius(32, 4), 2);
    assert_eq!(level_radius(32, 7), 2);
    assert_eq!(level_radius(12, 0), 12);
    assert_eq!(level_radius(12, 1), 6);
    assert_eq!(level_radius(12, 2), 3);
    assert_eq!(level_radius(12, 3), 2);
}

#[test]
fn energy_subtracts_noise_and_optional_contrast() {
    let level = flat_level(8, 8, 0.5, 0.2);
    let raw = energy_map(&level, 1, 0.0, false);
    for &value in &raw.energy {
        assert!((value - 0.04).abs() < 1e-6, "unexpected energy {value}");
    }

    let floored = energy_map(&level, 1, 0.01, false);
    for &value in &floored.energy {
        assert!(
            (value - 0.03).abs() < 1e-6,
            "unexpected floored energy {value}"
        );
    }

    let above_energy = energy_map(&level, 1, 1.0, false);
    assert!(above_energy.energy.iter().all(|&value| value == 0.0));

    let normalized = energy_map(&level, 1, 0.01, true);
    let expected = 0.03 / (0.25 + CONTRAST_EPSILON);
    for &value in &normalized.energy {
        assert!(
            (value - expected).abs() < 1e-6,
            "unexpected normalized energy {value}, expected {expected}"
        );
    }
}

#[test]
fn contrast_normalization_boosts_dark_detail() {
    let (width, height) = (16u32, 8u32);
    let count = (width * height) as usize;
    let mut level = PyramidLevel {
        width,
        height,
        gaussian: vec![0.0; count],
        laplacian: vec![0.05; count],
    };
    for y in 0..height as usize {
        for x in 0..width as usize {
            level.gaussian[y * width as usize + x] = if x < width as usize / 2 { 0.1 } else { 0.8 };
        }
    }

    let raw = energy_map(&level, 1, 0.0, false);
    let normalized = energy_map(&level, 1, 0.0, true);
    let dark = 4 * width as usize + 2;
    let bright = 4 * width as usize + 13;
    assert!((raw.energy[dark] - raw.energy[bright]).abs() < 1e-6);
    assert!(
        normalized.energy[dark] > 10.0 * normalized.energy[bright],
        "dark {} should be boosted over bright {}",
        normalized.energy[dark],
        normalized.energy[bright]
    );
}

#[test]
fn energy_falls_monotonically_on_a_blur_ladder() {
    let (width, height) = (256usize, 192usize);
    let sharp = textured_image(width, height);
    assert_eq!(
        focus_stack::focus::pyramid::level_count(width as u32, height as u32),
        5
    );

    let mut finest = Vec::new();
    let mut aggregated = Vec::new();
    for sigma in [0.0f32, 1.0, 2.0, 4.0] {
        let blurred = gaussian_blur(&sharp, width, height, sigma);
        let mut pyramid = Pyramid::new();
        pyramid.build(&blurred, width as u32, height as u32);
        // Isolate the blur response from the noise floor; the floor is covered
        // by the dedicated subtraction test.
        let noise = NoiseModel::default();
        let options = EnergyOptions {
            radius: Some(3),
            contrast_normalize: false,
        };
        let maps = EnergyMaps::compute(&pyramid, &noise, options);

        finest.push(mean(&maps.level(0).unwrap().energy));
        aggregated.push(mean(&maps.aggregate()));
    }

    for window in finest.windows(2) {
        assert!(
            window[1] < window[0],
            "finest energy did not decrease: {finest:?}"
        );
    }
    for window in aggregated.windows(2) {
        assert!(
            window[1] < window[0],
            "aggregated energy did not decrease: {aggregated:?}"
        );
    }
}

#[test]
fn compute_uses_one_map_and_radius_per_level() {
    let (width, height) = (128u32, 96u32);
    let luma = textured_image(width as usize, height as usize);
    let mut pyramid = Pyramid::new();
    pyramid.build(&luma, width, height);
    let noise = estimate(&pyramid, DEFAULT_NOISE_K);

    let maps = EnergyMaps::compute(
        &pyramid,
        &noise,
        EnergyOptions {
            radius: Some(20),
            contrast_normalize: false,
        },
    );

    assert_eq!(maps.radius0(), 20);
    assert_eq!(maps.levels().len(), pyramid.levels().len());
    for (index, (map, level)) in maps.levels().iter().zip(pyramid.levels()).enumerate() {
        assert_eq!((map.width, map.height), (level.width, level.height));
        assert_eq!(map.radius, level_radius(20, index));
        assert_eq!(map.energy.len(), map.len());
        assert!(
            map.energy
                .iter()
                .all(|value| value.is_finite() && *value >= 0.0)
        );
    }
}

#[test]
fn aggregation_uses_documented_weights_and_skips_the_residual() {
    assert_eq!(
        AGGREGATION_WEIGHTS,
        [1.0, 0.9, 0.8, 0.6, 0.5, 0.4, 0.3, 0.2]
    );
    for (index, &weight) in AGGREGATION_WEIGHTS.iter().enumerate() {
        assert_eq!(aggregation_weight(index), Some(weight));
    }
    assert_eq!(aggregation_weight(AGGREGATION_WEIGHTS.len()), None);

    let levels = vec![
        EnergyMap {
            width: 2,
            height: 2,
            radius: 2,
            energy: vec![1.0; 4],
        },
        EnergyMap {
            width: 1,
            height: 1,
            radius: 2,
            energy: vec![10.0],
        },
        EnergyMap {
            width: 1,
            height: 1,
            radius: 2,
            energy: vec![100.0],
        },
    ];

    let aggregated = aggregate_levels(&levels);
    assert_eq!(aggregated.len(), 4);
    for &value in &aggregated {
        assert!(
            (value - 10.0).abs() < 1e-6,
            "residual leaked into aggregation: {value}"
        );
    }

    let single = aggregate_levels(&levels[..1]);
    assert_eq!(single, vec![1.0; 4]);
}

#[test]
fn aggregation_from_compute_has_finest_resolution() {
    let (width, height) = (64u32, 48u32);
    let luma = textured_image(width as usize, height as usize);
    let mut pyramid = Pyramid::new();
    pyramid.build(&luma, width, height);
    let noise = estimate(&pyramid, DEFAULT_NOISE_K);
    let maps = EnergyMaps::compute(&pyramid, &noise, EnergyOptions::default());

    let aggregated = maps.aggregate();
    assert_eq!(aggregated.len(), (width * height) as usize);
    assert!(
        aggregated
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0)
    );
    assert!(mean(&aggregated) > 0.0);
}

#[test]
fn noise_model_with_floor_above_energy_zeros_the_map() {
    let level = flat_level(4, 4, 0.5, 0.1);
    let tau = 1.0;
    let level_map = energy_map(&level, 2, tau, false);
    assert!(level_map.energy.iter().all(|&value| value == 0.0));

    let noise = NoiseModel {
        levels: vec![NoiseEstimate {
            sigma: 1.0,
            tau,
            gradient_threshold: 0.0,
            samples: 1,
        }],
    };
    let pyramid_luma = vec![0.5f32; 16];
    let mut pyramid = Pyramid::new();
    pyramid.build(&pyramid_luma, 4, 4);
    let maps = EnergyMaps::compute(&pyramid, &noise, EnergyOptions::default());
    assert!(
        maps.level(0)
            .unwrap()
            .energy
            .iter()
            .all(|&value| value == 0.0)
    );
}

#[test]
#[ignore = "release-only 24 MP performance gate"]
fn four_frame_24mp_energy_pass_under_one_second() {
    let (width, height) = (6000u32, 4000u32);
    let luma = textured_image(width as usize, height as usize);
    let mut pyramid = Pyramid::new();
    pyramid.build(&luma, width, height);
    let noise = estimate(&pyramid, DEFAULT_NOISE_K);
    let options = EnergyOptions::default();

    let start = Instant::now();
    for _ in 0..4 {
        let maps = EnergyMaps::compute(&pyramid, &noise, options);
        std::hint::black_box(maps.aggregate());
    }
    let elapsed = start.elapsed();

    println!("4-frame 24 MP energy pass: {:.3} s", elapsed.as_secs_f64());
    assert!(
        elapsed < Duration::from_secs(1),
        "4-frame 24 MP energy pass took {:.3} s",
        elapsed.as_secs_f64()
    );
}
