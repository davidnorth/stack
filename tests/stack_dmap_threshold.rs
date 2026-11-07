//! Automatic contrast threshold, keep-detail override, and confidence ramp.

use focus_stack::stack::dmap::{
    ContrastThreshold, CostVolume, THRESHOLD_CAP_FRACTION, ThresholdOptions, threshold_depth,
};

const EPS: f32 = 1.0e-5;

fn two_region_stack() -> CostVolume {
    const WIDTH: usize = 64;
    const HEIGHT: usize = 32;
    const FRAMES: usize = 4;

    let mut aggregates = vec![vec![0.0f32; WIDTH * HEIGHT]; FRAMES];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = y * WIDTH + x;
            if x < WIDTH / 2 {
                // Sky: every frame ties, so the margin carries no confidence.
                let sky = 0.01 + 0.0001 * ((x + y) % 3) as f32;
                for aggregate in &mut aggregates {
                    aggregate[pixel] = sky;
                }
            } else {
                // Texture: one frame wins with a large best-vs-second margin.
                let winner = (x + y) % FRAMES;
                for (frame, aggregate) in aggregates.iter_mut().enumerate() {
                    aggregate[pixel] = if frame == winner {
                        0.90 + 0.01 * ((x * 3 + y) % 5) as f32
                    } else {
                        0.15 + 0.01 * ((x + y * 7) % 3) as f32
                    };
                }
            }
        }
    }

    CostVolume::new(&aggregates, WIDTH as u32, HEIGHT as u32)
}

fn continuous_stack(scale: f32, seed: u32, width: usize, height: usize) -> CostVolume {
    const FRAMES: usize = 4;

    let mut aggregates = vec![vec![0.0f32; width * height]; FRAMES];
    for y in 0..height {
        for x in 0..width {
            let pixel = y * width + x;
            let u = ((x as f32 + 0.5) / width as f32 + (y as f32 + 0.5) / height as f32) * 0.5;
            let level = scale * 0.01 * (1.0 + 199.0 * u);
            let winner = (x * 3 + y * 5 + seed as usize) % FRAMES;
            for (frame, aggregate) in aggregates.iter_mut().enumerate() {
                let jitter = 1.0 + 0.01 * (((x + seed as usize) * 7 + y * 3 + frame) % 5) as f32;
                aggregate[pixel] = if frame == winner {
                    level * jitter
                } else {
                    level * 0.25 * jitter
                };
            }
        }
    }

    CostVolume::new(&aggregates, width as u32, height as u32)
}

#[test]
fn sky_is_low_confidence_and_texture_is_retained() {
    let volume = two_region_stack();
    let (depth, threshold) = threshold_depth(
        &volume,
        ThresholdOptions {
            noise_sigma: 0.01,
            keep_detail: None,
        },
    );

    assert!(
        threshold.threshold > 0.01 && threshold.threshold < 0.5,
        "threshold {} did not land in the sky/texture gap",
        threshold.threshold
    );
    assert!(threshold.otsu.is_finite());
    assert!(threshold.noise_floor > 0.0);
    assert!(threshold.cap > 0.0);

    for y in 0..volume.height as usize {
        for x in 0..volume.width as usize {
            let pixel = y * volume.width as usize + x;
            if x < volume.width as usize / 2 {
                assert!(
                    depth.field.confidence[pixel] <= EPS,
                    "sky pixel {pixel} kept confidence {}",
                    depth.field.confidence[pixel]
                );
                assert!(depth.flat[pixel], "sky pixel {pixel} was not flagged flat");
            } else {
                assert!(
                    depth.field.confidence[pixel] > 0.30,
                    "texture pixel {pixel} lost confidence: {}",
                    depth.field.confidence[pixel]
                );
                assert!(!depth.flat[pixel], "texture pixel {pixel} was flagged flat");
            }
        }
    }

    assert!(
        (threshold.confident_pct - 50.0).abs() < 5.0,
        "confident percentage {} should track the textured half",
        threshold.confident_pct
    );
    assert!((threshold.coverage_pct - 100.0).abs() <= EPS);
}

#[test]
fn chosen_threshold_and_statistics_are_reported() {
    let volume = two_region_stack();
    let computed = ContrastThreshold::compute(&volume, ThresholdOptions::default());
    let (_, applied) = threshold_depth(&volume, ThresholdOptions::default());

    assert_eq!(computed.threshold, applied.threshold);
    assert!(computed.threshold.is_finite());
    assert!(computed.confident_pct.is_finite());
    assert!(computed.coverage_pct.is_finite());
    assert!((0.0..=100.0).contains(&computed.confident_pct));
    assert!((0.0..=100.0).contains(&computed.coverage_pct));
    assert!(computed.cap >= computed.threshold);
    assert!(computed.keep_detail.is_none());
}

#[test]
fn keep_detail_override_uses_the_requested_quantile_and_cap() {
    // Emax rises linearly from 0.01 to 1.0. The automatic Otsu split lands
    // near the geometric mean, while keep-detail 99 asks for the 1% quantile.
    let values: Vec<f32> = (0..200)
        .map(|index| 0.01 + (0.99 / 199.0) * index as f32)
        .collect();
    let volume = CostVolume::new(&[values], 200, 1);

    let automatic = ContrastThreshold::compute(&volume, ThresholdOptions::default());
    let override_threshold = ContrastThreshold::compute(
        &volume,
        ThresholdOptions {
            noise_sigma: 0.0,
            keep_detail: Some(99.0),
        },
    );
    let capped = ContrastThreshold::compute(
        &volume,
        ThresholdOptions {
            noise_sigma: 0.0,
            keep_detail: Some(50.0),
        },
    );

    assert_eq!(override_threshold.keep_detail, Some(99.0));
    assert!(
        override_threshold.threshold < automatic.threshold * 0.5,
        "override {} did not move below automatic {}",
        override_threshold.threshold,
        automatic.threshold
    );
    assert!(override_threshold.threshold > 0.0);
    assert!((override_threshold.threshold - 0.01995).abs() < 1.0e-3);
    assert!((capped.threshold - capped.cap).abs() <= EPS);
    assert!((capped.cap - THRESHOLD_CAP_FRACTION * 0.99).abs() < 1.0e-3);
}

#[test]
fn noise_floor_raises_the_threshold_above_the_otsu_split() {
    // 90% flat sky at 0.001, 10% texture at 1.0. Otsu separates the two spikes
    // just above the sky, so a 0.1 sigma floor (3 * sigma^2 = 0.03) dominates.
    let mut values = vec![0.001f32; 900];
    values.extend(std::iter::repeat_n(1.0f32, 100));
    let volume = CostVolume::new(&[values], 1000, 1);

    let quiet = ContrastThreshold::compute(&volume, ThresholdOptions::default());
    let noisy = ContrastThreshold::compute(
        &volume,
        ThresholdOptions {
            noise_sigma: 0.1,
            keep_detail: None,
        },
    );

    assert!((noisy.noise_floor - 0.03).abs() <= EPS);
    assert!(
        noisy.threshold > quiet.threshold * 2.0,
        "noise floor {} did not raise the threshold",
        noisy.threshold
    );
    assert!((noisy.threshold - 0.03).abs() <= EPS);
    assert!(noisy.threshold <= noisy.cap);
}

#[test]
fn threshold_is_stable_across_fixture_stacks() {
    let variants = [
        continuous_stack(0.5, 0, 48, 32),
        continuous_stack(1.0, 1, 64, 40),
        continuous_stack(2.0, 2, 80, 48),
    ];
    let scales = [0.5f32, 1.0, 2.0];

    let normalized: Vec<f32> = variants
        .iter()
        .zip(scales)
        .map(|(volume, scale)| {
            let threshold = ContrastThreshold::compute(
                volume,
                ThresholdOptions {
                    noise_sigma: 0.01,
                    keep_detail: None,
                },
            );
            threshold.threshold / scale
        })
        .collect();

    let reference = normalized[1];
    for (index, value) in normalized.iter().enumerate() {
        let relative = (value - reference).abs() / reference;
        assert!(
            relative < 0.05,
            "variant {index} normalized threshold {value} differs by {:.1}% from {reference}",
            relative * 100.0
        );
    }
}
