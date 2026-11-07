use focus_stack::align::preprocess::{PreprocessParams, hann_window, preprocess_rgb};

fn params(long_edges: [u32; 2]) -> PreprocessParams {
    PreprocessParams {
        long_edges,
        ..PreprocessParams::default()
    }
}

fn image(width: u32, height: u32, sample: impl Fn(u32, u32) -> [f32; 3]) -> Vec<f32> {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            rgb.extend_from_slice(&sample(x, y));
        }
    }
    rgb
}

fn at(level: &focus_stack::align::preprocess::AlignmentLevel, x: u32, y: u32) -> usize {
    (y * level.width + x) as usize
}

#[test]
fn luma_is_rec709_encoded() {
    let rgb = vec![1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
    let levels = preprocess_rgb(&rgb, 3, 1, &PreprocessParams::default());

    assert_eq!(levels.len(), 1);
    let level = &levels[0];
    assert!((level.luma[0] - 0.2126).abs() < 1e-6);
    assert!((level.luma[1] - 0.7152).abs() < 1e-6);
    assert!((level.luma[2] - 0.0722).abs() < 1e-6);
}

#[test]
fn flat_sky_is_excluded() {
    let rgb = image(64, 48, |x, y| {
        if y < 24 {
            [0.60, 0.70, 0.80]
        } else if (x / 8 + y / 8) % 2 == 0 {
            [0.15, 0.15, 0.15]
        } else {
            [0.75, 0.75, 0.75]
        }
    });
    let levels = preprocess_rgb(&rgb, 64, 48, &PreprocessParams::default());
    let level = &levels[0];

    // Stay clear of the Gaussian footprint at the sky/foreground boundary.
    for y in 0..20 {
        for x in 0..64 {
            assert_eq!(level.mask[at(level, x, y)], 0.0, "sky at ({x}, {y})");
        }
    }
    let textured = (24..48)
        .flat_map(|y| (0..64).map(move |x| (x, y)))
        .filter(|&(x, y)| level.mask[at(level, x, y)] > 0.5)
        .count();
    assert!(textured > 500, "foreground should pass the mask");
}

#[test]
fn saturated_pixels_are_excluded() {
    let rgb = image(64, 64, |x, y| {
        let textured = if (x + y) % 2 == 0 {
            [0.20, 0.35, 0.50]
        } else {
            [0.60, 0.65, 0.70]
        };
        if (16..48).contains(&x) && (16..48).contains(&y) {
            if (x + y) % 2 == 0 {
                [1.0, 0.25, 0.20]
            } else {
                [1.0, 0.65, 0.45]
            }
        } else {
            textured
        }
    });
    let levels = preprocess_rgb(&rgb, 64, 64, &PreprocessParams::default());
    let level = &levels[0];

    let mut outside = 0;
    for y in 0..64 {
        for x in 0..64 {
            let mask = level.mask[at(level, x, y)];
            if (16..48).contains(&x) && (16..48).contains(&y) {
                assert_eq!(mask, 0.0, "saturated pixel at ({x}, {y})");
            } else if mask > 0.5 {
                outside += 1;
            }
        }
    }
    assert!(
        outside > 500,
        "unsaturated texture should remain in the mask"
    );
}

#[test]
fn crushed_shadows_are_excluded() {
    let rgb = image(64, 64, |x, y| {
        let textured = if (x + y) % 2 == 0 {
            [0.20, 0.35, 0.50]
        } else {
            [0.60, 0.65, 0.70]
        };
        if (16..48).contains(&x) && (16..48).contains(&y) {
            if (x + y) % 2 == 0 {
                [0.002, 0.003, 0.004]
            } else {
                [0.006, 0.007, 0.008]
            }
        } else {
            textured
        }
    });
    let levels = preprocess_rgb(&rgb, 64, 64, &PreprocessParams::default());
    let level = &levels[0];

    let mut outside = 0;
    for y in 0..64 {
        for x in 0..64 {
            let mask = level.mask[at(level, x, y)];
            if (16..48).contains(&x) && (16..48).contains(&y) {
                assert_eq!(mask, 0.0, "crushed shadow at ({x}, {y})");
            } else if mask > 0.5 {
                outside += 1;
            }
        }
    }
    assert!(
        outside > 500,
        "non-shadow texture should remain in the mask"
    );
}

#[test]
fn downsampling_caps_the_long_edge_and_bounds_buffers() {
    let rgb = image(64, 48, |x, y| {
        let value = ((x + y) % 7) as f32 / 7.0;
        [value, value * 0.5, 1.0 - value]
    });
    let levels = preprocess_rgb(&rgb, 64, 48, &params([32, 16]));

    assert_eq!(levels.len(), 2);
    assert_eq!((levels[0].width, levels[0].height), (32, 24));
    assert_eq!((levels[1].width, levels[1].height), (16, 12));

    for level in &levels {
        let pixels = (level.width * level.height) as usize;
        assert_eq!(level.luma.len(), pixels);
        assert_eq!(level.high_pass.len(), pixels);
        assert_eq!(level.gradient.len(), pixels);
        assert_eq!(level.mask.len(), pixels);
        assert_eq!(level.windowed.len(), pixels);
        assert!(level.width.max(level.height) <= 32);
    }
}

#[test]
fn targets_larger_than_the_source_collapse_to_native_size() {
    let rgb = image(20, 10, |x, _| [(x % 2) as f32, 0.4, 0.6]);
    let levels = preprocess_rgb(&rgb, 20, 10, &PreprocessParams::default());

    assert_eq!(levels.len(), 1);
    assert_eq!((levels[0].width, levels[0].height), (20, 10));
}

#[test]
fn high_pass_removes_smooth_illumination() {
    let rgb = image(64, 8, |x, _| {
        let value = 0.2 + 0.4 * x as f32 / 63.0;
        [value, value, value]
    });
    let levels = preprocess_rgb(&rgb, 64, 8, &PreprocessParams::default());
    let level = &levels[0];

    for y in 0..8 {
        for x in 8..56 {
            let value = level.high_pass[at(level, x, y)];
            assert!(
                value.abs() < 0.01,
                "high-pass residual {value} at ({x}, {y})"
            );
        }
    }
}

#[test]
fn hann_window_is_zero_at_the_border_and_one_at_the_centre() {
    let window = hann_window(5, 5);

    assert!((window[0] - 0.0).abs() < 1e-6);
    assert!((window[12] - 1.0).abs() < 1e-6);
    assert!((window[1] - window[3]).abs() < 1e-6);
    assert!((window[5] - window[15]).abs() < 1e-6);

    let single = hann_window(1, 3);
    assert!((single[1] - 1.0).abs() < 1e-6);
}

#[test]
fn windowed_luma_is_high_pass_times_hann() {
    let rgb = image(16, 12, |x, y| {
        let value = ((x * 3 + y * 5) % 11) as f32 / 11.0;
        [value, value, value]
    });
    let levels = preprocess_rgb(&rgb, 16, 12, &PreprocessParams::default());
    let level = &levels[0];
    let window = hann_window(level.width, level.height);

    for (index, &window_value) in window.iter().enumerate() {
        let expected = level.high_pass[index] * window_value;
        assert!((level.windowed[index] - expected).abs() < 1e-6);
    }
}
