//! Hybrid detail transfer: guided base, soft clamp, and YCbCr recombination.

use std::f32::consts::TAU;

use focus_stack::color::luma::linear_luma;
use focus_stack::stack::hybrid::{
    DEFAULT_DETAIL_TRANSFER, DETAIL_CLAMP_TAU, DetailOptions, detail_plane, guided_base,
    stack_hybrid, transfer_detail,
};

const WIDTH: usize = 128;
const HEIGHT: usize = 128;

fn blur(source: &[f32], sigma: f32) -> Vec<f32> {
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
                sum += weight * source[y * WIDTH + sx];
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

/// Synthetic sharp ground truth with low- and high-frequency detail.
fn ground_truth() -> Vec<f32> {
    let mut values = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let fx = x as f32;
            let fy = y as f32;
            // Low-frequency tone plus micro-detail whose variance is below the
            // guided filter's epsilon, so the base actually removes it.
            let value = 0.5
                + 0.02 * (fx * 0.05 * TAU).sin() * (fy * 0.04 * TAU).cos()
                + 0.004 * (fx * 0.35 * TAU + fy * 0.28 * TAU).sin();
            values[y * WIDTH + x] = value.clamp(0.05, 0.95);
        }
    }
    values
}

/// DMap-like soft result: most of the high-frequency detail is lost.
fn soft_dmap(sharp: &[f32]) -> Vec<f32> {
    let blurred = blur(sharp, 4.0);
    sharp
        .iter()
        .zip(&blurred)
        .map(|(&sharp, &blurred)| 0.35 * sharp + 0.65 * blurred)
        .collect()
}

fn gradient_sum(values: &[f32], border: usize) -> f64 {
    let mut total = 0.0f64;
    for y in border..HEIGHT - border {
        for x in border..WIDTH - border {
            let index = y * WIDTH + x;
            let gx = values[index + 1] - values[index - 1];
            let gy = values[index + WIDTH] - values[index - WIDTH];
            total += f64::from((gx * gx + gy * gy).sqrt());
        }
    }
    total
}

#[test]
fn detail_recovery_at_100_percent() {
    let sharp = ground_truth();
    let dmap = soft_dmap(&sharp);

    let detail_options = DetailOptions::default();
    let hybrid = transfer_detail(&dmap, &sharp, WIDTH as u32, HEIGHT as u32, detail_options);

    let truth_gradient = gradient_sum(&sharp, 4);
    let dmap_recovery = gradient_sum(&dmap, 4) / truth_gradient;
    let hybrid_recovery = gradient_sum(&hybrid, 4) / truth_gradient;

    assert!(
        dmap_recovery < 0.95,
        "the synthetic DMap is not soft enough: recovery {dmap_recovery}"
    );
    assert!(
        (0.95..=1.10).contains(&hybrid_recovery),
        "detail recovery {hybrid_recovery} outside the 0.95..=1.10 gate (DMap {dmap_recovery})"
    );
}

#[test]
fn soft_clamp_bounds_detail_and_prevents_overshoot() {
    let dmap = vec![0.5f32; WIDTH * HEIGHT];
    let mut pmax = vec![0.0f32; WIDTH * HEIGHT];
    // A bright impulse is a high-contrast feature with small local variance, so
    // the guided base leaves a large residual for the clamp to bound.
    pmax[(HEIGHT / 2) * WIDTH + WIDTH / 2] = 1.0;

    let options = DetailOptions::default();
    let detail = detail_plane(&pmax, WIDTH as u32, HEIGHT as u32, options);
    let out = transfer_detail(&dmap, &pmax, WIDTH as u32, HEIGHT as u32, options);

    let limit = options.k * options.tau;
    let max_detail = detail
        .iter()
        .fold(0.0f32, |maximum, &value| maximum.max(value.abs()));
    let max_added = out
        .iter()
        .zip(&dmap)
        .fold(0.0f32, |maximum, (&out, &dmap)| {
            maximum.max((out - dmap).abs())
        });

    assert!(
        max_detail <= limit + 1.0e-5,
        "detail {max_detail} exceeds the soft-clamp limit {limit}"
    );
    assert!(
        max_added <= limit + 1.0e-5,
        "added luma {max_added} overshoots the limit {limit}"
    );
    assert!(
        max_detail > 0.5 * limit,
        "the impulse did not exercise the clamp: {max_detail}"
    );
}

#[test]
fn chroma_comes_from_dmap_not_pmax() {
    let count = WIDTH * HEIGHT;
    let dmap_color = [0.55f32, 0.35, 0.25];
    let pmax_color = [0.15f32, 0.45, 0.65];

    let mut dmap = Vec::with_capacity(count * 3);
    for _ in 0..count {
        dmap.extend_from_slice(&dmap_color);
    }

    // A luma ripple on the PMax side must not import PMax's blue chroma.
    let mut pmax = Vec::with_capacity(count * 3);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let ripple = 0.05 * ((x as f32) * 0.4 * TAU).sin() * ((y as f32) * 0.3 * TAU).cos();
            for channel in pmax_color {
                pmax.push((channel + ripple).clamp(0.0, 1.0));
            }
        }
    }

    let out = stack_hybrid(
        &dmap,
        &pmax,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
    );

    let dmap_luma = linear_luma(dmap_color[0], dmap_color[1], dmap_color[2]);
    let expected_cb = dmap_color[2] - dmap_luma;
    let expected_cr = dmap_color[0] - dmap_luma;
    let pmax_luma = linear_luma(pmax_color[0], pmax_color[1], pmax_color[2]);
    let pmax_cb = pmax_color[2] - pmax_luma;
    assert!(
        (pmax_cb - expected_cb).abs() > 0.05,
        "test colors need distinct chroma"
    );

    for pixel in out.as_chunks::<3>().0 {
        let luma = linear_luma(pixel[0], pixel[1], pixel[2]);
        assert!((pixel[2] - luma - expected_cb).abs() < 1.0e-5);
        assert!((pixel[0] - luma - expected_cr).abs() < 1.0e-5);
    }
}

#[test]
fn constant_images_have_no_detail() {
    let source = vec![0.42f32; WIDTH * HEIGHT];
    let base = guided_base(
        &source,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
    );
    let detail = detail_plane(
        &source,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
    );

    for (&base, &source) in base.iter().zip(&source) {
        assert!((base - source).abs() < 1.0e-6);
    }
    assert!(detail.iter().all(|&value| value.abs() < 1.0e-6));
}

#[test]
fn defaults_match_the_documented_parameters() {
    let options = DetailOptions::default();
    assert!((options.k - DEFAULT_DETAIL_TRANSFER).abs() < f32::EPSILON);
    assert!((options.tau - DETAIL_CLAMP_TAU).abs() < f32::EPSILON);
    assert_eq!(options.radius, 24);
    assert!((options.epsilon - 1.0e-3).abs() < f32::EPSILON);
}

#[test]
#[should_panic(expected = "luma buffer length mismatch")]
fn transfer_rejects_mismatched_buffers() {
    let _ = transfer_detail(&[0.0f32; 4], &[0.0f32; 3], 2, 2, DetailOptions::default());
}
