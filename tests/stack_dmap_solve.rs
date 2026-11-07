//! Edge-aware coarse-to-fine depth solve.

use focus_stack::DepthField;
use focus_stack::stack::dmap::{InitialDepth, JACOBI_TOLERANCE, SolveOptions, solve_depth};
use std::time::Instant;

const EPS: f32 = 1.0e-5;

fn initial(width: usize, height: usize, depth: Vec<f32>, confidence: Vec<f32>) -> InitialDepth {
    assert_eq!(depth.len(), width * height);
    assert_eq!(confidence.len(), width * height);
    InitialDepth {
        field: DepthField {
            d: depth,
            confidence,
        },
        best_energy: Vec::new(),
        margin: Vec::new(),
        flat: Vec::new(),
    }
}

#[test]
fn unconfident_flat_regions_are_smoothed() {
    const WIDTH: usize = 48;
    const HEIGHT: usize = 24;

    let guide = vec![0.0f32; WIDTH * HEIGHT];
    let mut depth = vec![0.0f32; WIDTH * HEIGHT];
    let mut confidence = vec![1.0f32; WIDTH * HEIGHT];

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            if (12..36).contains(&x) {
                // No trustworthy evidence: the solve must fill this band from
                // the confident neighbours, ignoring the arbitrary d0.
                depth[index] = ((x * 7 + y * 3) % 5) as f32;
                confidence[index] = 0.0;
            } else {
                depth[index] = if x < 12 { 0.0 } else { 4.0 };
            }
        }
    }

    let result = solve_depth(
        &initial(WIDTH, HEIGHT, depth, confidence.clone()),
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        SolveOptions::default(),
    );

    for y in 0..HEIGHT {
        for x in 12..35 {
            let index = y * WIDTH + x;
            let step = (result.field.d[index + 1] - result.field.d[index]).abs();
            assert!(
                step < 0.75,
                "flat band step at ({x}, {y}) is {step}, depth is not smooth"
            );
        }

        let row = y * WIDTH;
        assert!(
            result.field.d[row + 11] < 0.5,
            "left anchor drifted to {}",
            result.field.d[row + 11]
        );
        assert!(
            result.field.d[row + 36] > 3.5,
            "right anchor drifted to {}",
            result.field.d[row + 36]
        );
        let middle = result.field.d[row + 24];
        assert!(
            middle > 1.0 && middle < 3.0,
            "flat band did not interpolate between the anchors: {middle}"
        );
    }

    assert_eq!(result.field.confidence, confidence);
    assert!(result.field.d.iter().all(|value| value.is_finite()));
}

fn solve_edge_scene(edge_in_guide: bool) -> Vec<f32> {
    const WIDTH: usize = 64;
    const HEIGHT: usize = 32;

    let mut guide = vec![0.0f32; WIDTH * HEIGHT];
    let mut depth = vec![0.0f32; WIDTH * HEIGHT];
    let mut confidence = vec![1.0f32; WIDTH * HEIGHT];

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            if edge_in_guide && x >= WIDTH / 2 {
                guide[index] = 1.0;
            }
            if (24..40).contains(&x) {
                depth[index] = 2.0;
                confidence[index] = 0.0;
            } else if x >= 40 {
                depth[index] = 4.0;
            }
        }
    }

    solve_depth(
        &initial(WIDTH, HEIGHT, depth, confidence),
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        SolveOptions {
            smoothing_radius: 0.0,
            ..SolveOptions::default()
        },
    )
    .field
    .d
}

#[test]
fn guide_edges_keep_depth_sharp() {
    const WIDTH: usize = 64;
    const HEIGHT: usize = 32;
    let row = (HEIGHT / 2) * WIDTH;

    let flat = solve_edge_scene(false);
    let edge = solve_edge_scene(true);

    // A flat guide has nothing to stop diffusion, so the unknown band blends
    // smoothly across the middle.
    let flat_step = flat[row + 32] - flat[row + 31];
    assert!(
        flat_step < 1.0,
        "flat guide produced a sharp step {flat_step} without an edge"
    );
    assert!(flat[row + 31] > 1.0 && flat[row + 32] < 3.0);

    // The guide ridge cuts the diffusion, so each side stays anchored to its
    // own confident region.
    assert!(
        edge[row + 31] < 0.75,
        "left side bled across the edge: {}",
        edge[row + 31]
    );
    assert!(
        edge[row + 32] > 3.25,
        "right side bled across the edge: {}",
        edge[row + 32]
    );
    assert!(
        edge[row + 32] - edge[row + 31] > 2.5,
        "edge was softened: {} -> {}",
        edge[row + 31],
        edge[row + 32]
    );
}

#[test]
fn solve_converges_below_tolerance() {
    const WIDTH: usize = 32;
    const HEIGHT: usize = 32;

    let mut guide = vec![0.0f32; WIDTH * HEIGHT];
    let mut depth = vec![0.0f32; WIDTH * HEIGHT];
    let confidence = vec![0.6f32; WIDTH * HEIGHT];

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let index = y * WIDTH + x;
            let ramp = (x as f32 + 0.5) / WIDTH as f32;
            guide[index] = ramp;
            depth[index] = 3.0 * ramp;
        }
    }

    let result = solve_depth(
        &initial(WIDTH, HEIGHT, depth, confidence),
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        SolveOptions {
            iterations: 60,
            smoothing_radius: 0.0,
            ..SolveOptions::default()
        },
    );

    assert!(
        result.converged,
        "solve did not converge: max update {} after {} iterations",
        result.max_update, result.iterations
    );
    assert!(result.max_update <= JACOBI_TOLERANCE);
    assert!(result.iterations > 0 && result.iterations <= 60 * 3);
    assert!(result.field.d.iter().all(|value| value.is_finite()));
}

#[test]
#[ignore = "release-only 24 MP performance gate"]
fn solve_24mp_under_two_seconds() {
    let (width, height) = (6000usize, 4000usize);
    let count = width * height;

    let mut guide = vec![0.0f32; count];
    let mut depth = vec![0.0f32; count];
    let mut confidence = vec![0.5f32; count];
    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            guide[index] = ((x * 13 + y * 7) % 100) as f32 / 100.0;
            depth[index] = 3.0 * (x as f32 + 0.5) / width as f32;
            if y % 4 == 0 {
                // A flat, unconfident row exercises the post-smooth path.
                depth[index] = 0.0;
                confidence[index] = 0.0;
            }
        }
    }

    let initial = initial(width, height, depth, confidence);
    let start = Instant::now();
    let result = solve_depth(
        &initial,
        &guide,
        width as u32,
        height as u32,
        SolveOptions::default(),
    );
    let elapsed = start.elapsed();
    println!(
        "24 MP depth solve: {:.3} s, {} iterations, converged={}",
        elapsed.as_secs_f64(),
        result.iterations,
        result.converged
    );
    std::hint::black_box(&result);

    assert_eq!(result.field.d.len(), count);
    assert!(
        elapsed.as_secs_f64() < 2.0,
        "24 MP depth solve took {:.3} s (gate: 2 s)",
        elapsed.as_secs_f64()
    );
}

#[test]
#[should_panic(expected = "guide buffer length mismatch")]
fn solve_rejects_mismatched_guides() {
    let depth = vec![0.0f32; 4];
    let confidence = vec![1.0f32; 4];
    let _ = solve_depth(
        &initial(2, 2, depth, confidence),
        &[0.0f32; 3],
        2,
        2,
        SolveOptions::default(),
    );
}

#[test]
fn solve_preserves_zero_iterations() {
    let depth = vec![1.0f32, 2.0, 3.0, 4.0];
    let confidence = vec![1.0f32; 4];
    let result = solve_depth(
        &initial(2, 2, depth.clone(), confidence),
        &[0.0f32; 4],
        2,
        2,
        SolveOptions {
            iterations: 0,
            smoothing_radius: 0.0,
            ..SolveOptions::default()
        },
    );

    assert!(!result.converged);
    assert_eq!(result.iterations, 0);
    assert_eq!(result.field.d, depth);
    assert!((result.max_update - 0.0).abs() <= EPS);
}
