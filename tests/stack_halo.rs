//! Halo suppression: low-pass anchoring and edge-band overshoot diagnostics.

use focus_stack::stack::hybrid::{
    DEFAULT_DETAIL_TRANSFER, DETAIL_CLAMP_TAU, DetailOptions, HALO_ANCHOR_LEVELS, HALO_BAND_RADIUS,
    HALO_OVERSHOOT_LIMIT, HaloOptions, detail_plane, halo_overshoot, pyramid_anchor, stack_hybrid,
    stack_hybrid_halo, suppress_halo, transfer_detail,
};

const WIDTH: usize = 128;
const HEIGHT: usize = 128;
const RIDGE_LEFT: f32 = 48.0;
const RIDGE_RIGHT: f32 = 80.0;
const BACKGROUND: f32 = 0.25;
const PLATEAU: f32 = 0.65;

/// A vertical ridge with a sharp bright plateau between two depth edges.
fn ridge_truth() -> Vec<f32> {
    let mut values = vec![BACKGROUND; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in (RIDGE_LEFT as usize)..(RIDGE_RIGHT as usize) {
            values[y * WIDTH + x] = PLATEAU;
        }
    }
    values
}

/// A depth guide that is 1 on the near ridge and 0 elsewhere.
fn depth_guide() -> Vec<f32> {
    let mut values = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in (RIDGE_LEFT as usize)..(RIDGE_RIGHT as usize) {
            values[y * WIDTH + x] = 1.0;
        }
    }
    values
}

/// Classic edge ringing: bright on the plateau, dark on the background.
fn ridge_halo(amplitude: f32) -> Vec<f32> {
    let mut values = vec![0.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let mut value = 0.0f32;
            for (edge, bright_side_is_right) in [(RIDGE_LEFT, true), (RIDGE_RIGHT, false)] {
                let delta = x as f32 - edge;
                let falloff = (-(delta / 1.5).powi(2)).exp();
                let bright_side = if bright_side_is_right {
                    delta >= 0.0
                } else {
                    delta <= 0.0
                };
                value += if bright_side {
                    amplitude * falloff
                } else {
                    -amplitude * falloff
                };
            }
            values[y * WIDTH + x] = value;
        }
    }
    values
}

fn add(left: &[f32], right: &[f32]) -> Vec<f32> {
    left.iter()
        .zip(right)
        .map(|(&left, &right)| left + right)
        .collect()
}

fn mean(values: &[f32]) -> f32 {
    values.iter().sum::<f32>() / values.len() as f32
}

#[test]
fn ridge_halo_below_dmap_and_pmax_alone() {
    let truth = ridge_truth();
    let guide = depth_guide();
    let dmap = add(&truth, &ridge_halo(0.15));
    let pmax = add(&truth, &ridge_halo(0.30));

    let detail = DetailOptions::default();
    let halo = HaloOptions::default();
    let hybrid = transfer_detail(&dmap, &pmax, WIDTH as u32, HEIGHT as u32, detail);
    let suppressed = suppress_halo(&hybrid, &dmap, WIDTH as u32, HEIGHT as u32, detail, halo);

    let dmap_halo = halo_overshoot(
        &dmap,
        &truth,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        HALO_BAND_RADIUS,
    );
    let pmax_halo = halo_overshoot(
        &pmax,
        &truth,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        HALO_BAND_RADIUS,
    );
    let hybrid_halo = halo_overshoot(
        &hybrid,
        &truth,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        HALO_BAND_RADIUS,
    );
    let suppressed_halo = halo_overshoot(
        &suppressed.luma,
        &truth,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        HALO_BAND_RADIUS,
    );

    assert!(
        dmap_halo > 0.05,
        "synthetic DMap is not haloed enough: {dmap_halo}"
    );
    assert!(
        pmax_halo > dmap_halo,
        "synthetic PMax halo {pmax_halo} should exceed DMap {dmap_halo}"
    );
    assert!(
        suppressed_halo < dmap_halo * 0.5,
        "suppressed halo {suppressed_halo} not measurably below DMap {dmap_halo}"
    );
    assert!(
        suppressed_halo < pmax_halo * 0.5,
        "suppressed halo {suppressed_halo} not measurably below PMax {pmax_halo}"
    );
    assert!(
        suppressed_halo < hybrid_halo,
        "suppression must reduce the unsuppressed hybrid halo {hybrid_halo}"
    );
    assert!(
        suppressed_halo <= halo.overshoot_limit * 2.0,
        "suppressed halo {suppressed_halo} exceeds twice the documented limit"
    );
    assert!(
        (0.0..=detail.k).contains(&suppressed.k_effective),
        "effective k {} outside 0..=k",
        suppressed.k_effective
    );
}

#[test]
fn overshoot_diagnostic_ignores_blur_but_catches_ringing() {
    let truth = ridge_truth();
    let guide = depth_guide();
    let ringing = add(&truth, &ridge_halo(0.15));

    let clean = halo_overshoot(
        &truth,
        &truth,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        HALO_BAND_RADIUS,
    );
    let haloed = halo_overshoot(
        &ringing,
        &truth,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        HALO_BAND_RADIUS,
    );

    assert!(clean < 1.0e-6, "clean ridge reports halo {clean}");
    assert!(haloed > 0.1, "ringing not detected: {haloed}");
}

#[test]
fn low_pass_anchor_restores_dmap_coarse_tone() {
    let dmap = vec![0.40f32; WIDTH * HEIGHT];
    let detail = DetailOptions::default();
    let halo = HaloOptions::default();

    // A constant offset lives entirely in the coarse levels and must be
    // rejected by the DMap anchor.
    let mut shifted = dmap.clone();
    for value in &mut shifted {
        *value += 0.20;
    }
    let suppressed = suppress_halo(&shifted, &dmap, WIDTH as u32, HEIGHT as u32, detail, halo);

    assert!(
        (mean(&suppressed.luma) - 0.40).abs() < 1.0e-4,
        "coarse offset survived anchoring: mean {}",
        mean(&suppressed.luma)
    );
    assert!((suppressed.k_effective - detail.k).abs() < 1.0e-5);
}

#[test]
fn pyramid_anchor_removes_fine_detail_but_keeps_tone() {
    let mut image = vec![0.5f32; WIDTH * HEIGHT];
    // A compact high-frequency bump; the coarse pyramid must not keep it.
    for y in 60..64 {
        for x in 60..64 {
            image[y * WIDTH + x] += 0.20;
        }
    }

    let anchor = pyramid_anchor(&image, WIDTH as u32, HEIGHT as u32, HALO_ANCHOR_LEVELS);

    assert!(
        (mean(&image) - mean(&anchor)).abs() < 1.0e-4,
        "anchoring changed the mean"
    );
    let removed = (0..WIDTH * HEIGHT)
        .map(|index| (image[index] - anchor[index]).abs())
        .fold(0.0f32, f32::max);
    assert!(
        removed > 0.05,
        "the test bump was not high-frequency enough: {removed}"
    );
    assert!(removed < 0.20, "the coarse anchor kept the bump: {removed}");
}

#[test]
fn suppression_is_confined_to_the_edge_band() {
    let mut dmap = vec![0.4f32; WIDTH * HEIGHT];
    let mut pmax = vec![0.4f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let ripple = 0.01 * ((x as f32) * 0.4).sin() * ((y as f32) * 0.3).cos();
            dmap[y * WIDTH + x] += ripple;
            pmax[y * WIDTH + x] += ripple;
        }
    }
    // Add a strong ridge so the edge band exists.
    let truth = ridge_truth();
    for index in 0..WIDTH * HEIGHT {
        if truth[index] > BACKGROUND {
            dmap[index] += 0.2;
            pmax[index] += 0.2;
        }
    }

    let detail = DetailOptions::default();
    let hybrid = transfer_detail(&dmap, &pmax, WIDTH as u32, HEIGHT as u32, detail);
    let suppressed = suppress_halo(
        &hybrid,
        &dmap,
        WIDTH as u32,
        HEIGHT as u32,
        detail,
        HaloOptions::default(),
    );
    // A huge allowance disables only the edge-band clamp, leaving the same
    // coarse anchoring for comparison.
    let unclamped = suppress_halo(
        &hybrid,
        &dmap,
        WIDTH as u32,
        HEIGHT as u32,
        detail,
        HaloOptions {
            overshoot_limit: 1.0e6,
            ..HaloOptions::default()
        },
    );

    let mut edge_delta = 0.0f32;
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            let delta = (suppressed.luma[index] - unclamped.luma[index]).abs();
            if x > 100 {
                assert!(
                    delta < 1.0e-5,
                    "clamp changed detail at ({x}, {y}) by {delta}"
                );
            }
            edge_delta = edge_delta.max(delta);
        }
    }
    assert!(
        edge_delta > 0.01,
        "the edge-band clamp had no effect: {edge_delta}"
    );
}

#[test]
fn full_hybrid_keeps_dmap_chroma() {
    let count = WIDTH * HEIGHT;
    let dmap_color = [0.55f32, 0.35, 0.25];
    let pmax_color = [0.20f32, 0.45, 0.60];
    let mut dmap = Vec::with_capacity(count * 3);
    let mut pmax = Vec::with_capacity(count * 3);
    for _ in 0..count {
        dmap.extend_from_slice(&dmap_color);
        pmax.extend_from_slice(&pmax_color);
    }

    let result = stack_hybrid_halo(
        &dmap,
        &pmax,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
        HaloOptions::default(),
    );

    let dmap_luma =
        focus_stack::color::luma::linear_luma(dmap_color[0], dmap_color[1], dmap_color[2]);
    let expected_cb = dmap_color[2] - dmap_luma;
    let expected_cr = dmap_color[0] - dmap_luma;
    for pixel in result.rgb.as_chunks::<3>().0 {
        let luma = focus_stack::color::luma::linear_luma(pixel[0], pixel[1], pixel[2]);
        assert!((pixel[2] - luma - expected_cb).abs() < 1.0e-5);
        assert!((pixel[0] - luma - expected_cr).abs() < 1.0e-5);
    }
    assert!(
        stack_hybrid_halo(
            &dmap,
            &pmax,
            WIDTH as u32,
            HEIGHT as u32,
            DetailOptions::default(),
            HaloOptions::default(),
        )
        .rgb
        .iter()
        .all(|value| (0.0..=1.0).contains(value))
    );
}

#[test]
fn defaults_match_the_documented_halo_parameters() {
    let options = HaloOptions::default();
    assert_eq!(options.anchor_levels, HALO_ANCHOR_LEVELS);
    assert_eq!(options.band_radius, HALO_BAND_RADIUS);
    assert!((options.overshoot_limit - HALO_OVERSHOOT_LIMIT).abs() < f32::EPSILON);
    assert_eq!(options.anchor_radius, 4);
    assert!((DetailOptions::default().tau - DETAIL_CLAMP_TAU).abs() < f32::EPSILON);
    assert!((DetailOptions::default().k - DEFAULT_DETAIL_TRANSFER).abs() < f32::EPSILON);
}

#[test]
#[should_panic(expected = "luma buffer length mismatch")]
fn suppress_halo_rejects_mismatched_buffers() {
    let _ = suppress_halo(
        &[0.0f32; 4],
        &[0.0f32; 3],
        2,
        2,
        DetailOptions::default(),
        HaloOptions::default(),
    );
}

#[test]
fn detail_plane_stays_bounded_after_suppression() {
    let dmap = vec![0.5f32; WIDTH * HEIGHT];
    let mut pmax = vec![0.5f32; WIDTH * HEIGHT];
    pmax[(HEIGHT / 2) * WIDTH + WIDTH / 2] = 1.0;
    let detail = DetailOptions::default();
    let hybrid = transfer_detail(&dmap, &pmax, WIDTH as u32, HEIGHT as u32, detail);
    let suppressed = suppress_halo(
        &hybrid,
        &dmap,
        WIDTH as u32,
        HEIGHT as u32,
        detail,
        HaloOptions::default(),
    );
    let base = detail_plane(&pmax, WIDTH as u32, HEIGHT as u32, detail);
    for (&suppressed, &base) in suppressed.luma.iter().zip(&base) {
        assert!(
            (suppressed - 0.5).abs() <= base.abs() + detail.k * detail.tau + 1.0e-5,
            "suppression amplified the impulse"
        );
    }
}

#[test]
fn stack_hybrid_without_halo_is_unchanged() {
    let dmap_color = [0.5f32, 0.4, 0.3];
    let count = WIDTH * HEIGHT;
    let mut dmap = Vec::with_capacity(count * 3);
    for _ in 0..count {
        dmap.extend_from_slice(&dmap_color);
    }
    // The no-halo path must still be the plain §4.1 transfer.
    let plain = stack_hybrid(
        &dmap,
        &dmap,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
    );
    for pixel in plain.as_chunks::<3>().0 {
        assert!((pixel[0] - dmap_color[0]).abs() < 1.0e-5);
    }
}
