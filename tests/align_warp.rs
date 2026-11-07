use focus_stack::Rect;
use focus_stack::Similarity;
use focus_stack::align::warp::{
    CROP_INSET, common_valid_rect, crop, lanczos3, prefilter_sigma, valid_rect, warp_linear,
};
use std::f64::consts::{PI, TAU};
use std::time::Instant;

/// Deterministic, asymmetric three-channel test image in `0.05..=0.95`.
fn test_image(width: u32, height: u32) -> Vec<f32> {
    let mut image = Vec::with_capacity(width as usize * height as usize * 3);
    for y in 0..height {
        for x in 0..width {
            let xf = f64::from(x);
            let yf = f64::from(y);
            image.push((0.5 + 0.3 * (0.11 * xf + 0.07 * yf).sin()) as f32);
            image.push((0.5 + 0.3 * (0.05 * xf - 0.13 * yf + 0.7).sin()) as f32);
            image.push((0.5 + 0.3 * (0.09 * xf + 0.17 * yf + 1.9).sin()) as f32);
        }
    }
    image
}

#[test]
fn identity_warp_is_bit_exact() {
    let (width, height) = (37u32, 23u32);
    let source = test_image(width, height);
    let identity = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };

    let warped = warp_linear(&source, width, height, width, height, 3, &identity).unwrap();
    assert_eq!(warped, source, "identity warp must not change any sample");
}

#[test]
fn ninety_degree_round_trip_is_within_one_lsb() {
    let size = 64u32;
    let source = test_image(size, size);
    let last = f64::from(size - 1);
    let clockwise = Similarity {
        a: 0.0,
        b: 1.0,
        tx: last,
        ty: 0.0,
    };
    let counter_clockwise = Similarity {
        a: 0.0,
        b: -1.0,
        tx: 0.0,
        ty: last,
    };

    let rotated = warp_linear(&source, size, size, size, size, 3, &clockwise).unwrap();
    let restored = warp_linear(&rotated, size, size, size, size, 3, &counter_clockwise).unwrap();

    let max_error = restored
        .iter()
        .zip(&source)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_error <= 1.0 / 65535.0,
        "90° round trip error {max_error} exceeds 1 LSB"
    );
}

#[test]
fn warp_clamps_to_edge_instead_of_black() {
    let (width, height) = (40u32, 30u32);
    let source = vec![0.3f32; width as usize * height as usize * 3];
    let transform = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 7.5,
        ty: -4.25,
    };

    let warped = warp_linear(&source, width, height, width, height, 3, &transform).unwrap();
    let min = warped.iter().copied().fold(f32::INFINITY, f32::min);
    assert!(min > 0.299, "edge clamp produced a dark border: {min}");
}

#[test]
fn rotated_crop_has_no_black_borders() {
    let (width, height) = (64u32, 64u32);
    let source = vec![0.25f32; width as usize * height as usize * 3];
    let angle = 0.05f64;
    let transform = Similarity {
        a: angle.cos(),
        b: angle.sin(),
        tx: 1.5,
        ty: -0.75,
    };

    let rect = valid_rect(width, height, width, height, &transform).expect("valid crop");
    let warped = warp_linear(&source, width, height, width, height, 3, &transform).unwrap();
    let cropped = crop(&warped, width, height, 3, rect.clone()).unwrap();

    assert_eq!(
        cropped.len(),
        rect.w as usize * rect.h as usize * 3,
        "crop size mismatch"
    );
    let min = cropped.iter().copied().fold(f32::INFINITY, f32::min);
    assert!(min > 0.249, "crop contains a dark border: {min}");
}

#[test]
fn identity_valid_rect_insets_edges() {
    let (width, height) = (100u32, 80u32);
    let identity = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    let rect = valid_rect(width, height, width, height, &identity).unwrap();
    assert_eq!(
        rect,
        Rect {
            x: CROP_INSET,
            y: CROP_INSET,
            w: width - 2 * CROP_INSET,
            h: height - 2 * CROP_INSET,
        }
    );
}

#[test]
fn translation_valid_rect_clips_the_shifted_edge() {
    let (size, translation) = (64u32, 10.5f64);
    let transform = Similarity {
        a: 1.0,
        b: 0.0,
        tx: translation,
        ty: 0.0,
    };
    let rect = valid_rect(size, size, size, size, &transform).unwrap();

    // Output x maps to source x + 10.5, so the right edge leaves the inset
    // source bounds after x = 48.
    assert_eq!(rect.x, 0);
    assert_eq!(rect.w, 49);
    assert_eq!(rect.y, CROP_INSET);
    assert_eq!(rect.h, size - 2 * CROP_INSET);
}

#[test]
fn common_valid_rect_intersects_all_frames() {
    let size = 64u32;
    let identity = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    let shifted = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 10.5,
        ty: 0.0,
    };
    let rect = common_valid_rect(size, size, size, size, &[identity, shifted]).unwrap();

    assert_eq!(
        rect,
        Rect {
            x: CROP_INSET,
            y: CROP_INSET,
            w: 45,
            h: size - 2 * CROP_INSET,
        }
    );
}

#[test]
fn minification_prefilter_attenuates_sub_nyquist_detail() {
    // Period-3 sinusoid, above the output Nyquist after 2x minification.
    // Without the pre-blur, sampling every other source pixel keeps nearly the
    // full 0.4 amplitude as an aliased period-3 pattern.
    let source_size = 128u32;
    let mut source = Vec::with_capacity(source_size as usize * source_size as usize);
    for _y in 0..source_size {
        for x in 0..source_size {
            let value = 0.5 + 0.4 * (TAU * f64::from(x) / 3.0 + 0.4).sin();
            source.push(value as f32);
        }
    }

    let output_size = source_size / 2;
    let transform = Similarity {
        a: 2.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    let warped = warp_linear(
        &source,
        source_size,
        source_size,
        output_size,
        output_size,
        1,
        &transform,
    )
    .unwrap();

    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut count = 0usize;
    for y in 8..output_size as usize - 8 {
        for x in 8..output_size as usize - 8 {
            let v = f64::from(warped[y * output_size as usize + x]);
            sum += v;
            sum_sq += v * v;
            count += 1;
        }
    }
    let mean = sum / count as f64;
    let variance = (sum_sq / count as f64 - mean * mean).max(0.0);
    let deviation = variance.sqrt();
    assert!(
        deviation < 0.01,
        "minification left aliased detail (std {deviation})"
    );
    assert!((mean - 0.5).abs() < 0.01, "pre-blur shifted the mean");
}

#[test]
fn prefilter_sigma_matches_the_documented_formula() {
    assert_eq!(prefilter_sigma(1.0), 0.0);
    assert_eq!(prefilter_sigma(0.99), 0.0);
    let sigma = prefilter_sigma(0.5);
    assert!((sigma - 3.0f64.sqrt()).abs() < 1.0e-12);
    assert_eq!(prefilter_sigma(f64::NAN), 0.0);
}

#[test]
fn lanczos3_is_the_documented_kernel() {
    assert_eq!(lanczos3(0.0), 1.0);
    for integer in [-3.0, -2.0, -1.0, 1.0, 2.0, 3.0, 4.0] {
        assert_eq!(lanczos3(integer), 0.0, "L({integer}) must be zero");
    }
    for x in [0.25, 0.5, 1.3, 2.2] {
        assert!((lanczos3(x) - lanczos3(-x)).abs() < 1.0e-15);
    }
    let expected = (PI * 0.5).sin() / (PI * 0.5);
    let expected = expected * ((PI * 0.5 / 3.0).sin() / (PI * 0.5 / 3.0));
    assert!((lanczos3(0.5) - expected).abs() < 1.0e-15);
}

#[test]
fn crop_copies_the_requested_rectangle() {
    let (width, height) = (10u32, 8u32);
    let source: Vec<f32> = (0..width as usize * height as usize * 3)
        .map(|index| index as f32 * 0.125)
        .collect();
    let rect = Rect {
        x: 2,
        y: 3,
        w: 4,
        h: 2,
    };
    let cropped = crop(&source, width, height, 3, rect.clone()).unwrap();

    for y in 0..rect.h as usize {
        for x in 0..rect.w as usize {
            for channel in 0..3 {
                let source_index =
                    ((rect.y as usize + y) * width as usize + rect.x as usize + x) * 3 + channel;
                let crop_index = (y * rect.w as usize + x) * 3 + channel;
                assert_eq!(cropped[crop_index], source[source_index]);
            }
        }
    }
}

#[test]
fn rejects_invalid_inputs() {
    let identity = Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    assert!(warp_linear(&[], 0, 10, 10, 10, 3, &identity).is_err());
    assert!(warp_linear(&[], 10, 10, 10, 10, 0, &identity).is_err());
    assert!(warp_linear(&[0.0; 4], 10, 10, 10, 10, 3, &identity).is_err());
    let degenerate = Similarity {
        a: 0.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    };
    assert!(warp_linear(&[0.0; 300], 10, 10, 10, 10, 3, &degenerate).is_err());
    assert!(
        crop(
            &[0.0; 300],
            10,
            10,
            3,
            Rect {
                x: 8,
                y: 0,
                w: 4,
                h: 1,
            },
        )
        .is_err()
    );
}

#[test]
#[ignore = "24 MP release-mode performance gate"]
fn warp_24mp_under_half_second() {
    let (width, height, channels) = (6000u32, 4000u32, 3usize);
    let source = vec![0.5f32; width as usize * height as usize * channels];
    let transform = Similarity {
        a: 0.0087f64.cos(),
        b: 0.0087f64.sin(),
        tx: 1.5,
        ty: -2.0,
    };

    let start = Instant::now();
    let warped = warp_linear(&source, width, height, width, height, channels, &transform).unwrap();
    let elapsed = start.elapsed();

    assert_eq!(warped.len(), source.len());
    assert!(
        elapsed.as_secs_f64() < 0.5,
        "24 MP warp took {:.3} s (gate: 0.5 s)",
        elapsed.as_secs_f64()
    );
}
