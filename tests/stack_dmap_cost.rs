//! Cost-volume construction and initial argmax depth.

use focus_stack::stack::dmap::{CostVolume, FLAT_MARGIN, argmax_depth};

const EPS: f32 = 1.0e-5;

fn band_depth(x: usize) -> usize {
    match x {
        0..=19 => 0,
        20..=39 => 2,
        _ => 3,
    }
}

#[test]
fn argmax_matches_synthetic_depth_within_one_frame() {
    const WIDTH: usize = 60;
    const HEIGHT: usize = 40;
    const FRAMES: usize = 4;

    let mut aggregates = vec![vec![0.0f32; WIDTH * HEIGHT]; FRAMES];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = y * WIDTH + x;
            let expected = band_depth(x);
            for (frame, aggregate) in aggregates.iter_mut().enumerate() {
                // Small deterministic variation keeps the synthetic scene
                // from exercising only exact ties.
                let wobble = 0.01 * ((x * 7 + y * 3 + frame * 5) % 5) as f32;
                aggregate[pixel] = if frame == expected {
                    0.80 + wobble
                } else {
                    0.20 + wobble
                };
            }
        }
    }

    let volume = CostVolume::new(&aggregates, WIDTH as u32, HEIGHT as u32);
    let result = argmax_depth(&volume, None);

    assert_eq!(result.field.d.len(), WIDTH * HEIGHT);
    assert_eq!(result.flat.len(), WIDTH * HEIGHT);

    let mut exact = 0usize;
    for pixel in 0..WIDTH * HEIGHT {
        let x = pixel % WIDTH;
        let expected = band_depth(x) as f32;
        let estimate = result.field.d[pixel];
        assert!(
            (estimate - expected).abs() <= 1.0,
            "pixel {pixel}: depth {estimate} is more than one frame from {expected}"
        );
        if (estimate - expected).abs() <= EPS {
            exact += 1;
        }
        assert!(!result.flat[pixel], "textured pixel {pixel} marked flat");
    }
    assert!(
        exact >= WIDTH * HEIGHT * 95 / 100,
        "only {exact} of {} pixels landed on the true frame",
        WIDTH * HEIGHT
    );
}

#[test]
fn tied_energies_flag_flat_regions() {
    const WIDTH: usize = 40;
    const HEIGHT: usize = 20;
    const FLAT_COLUMNS: usize = 20;

    let mut aggregates = vec![vec![0.0f32; WIDTH * HEIGHT]; 3];
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = y * WIDTH + x;
            if x < FLAT_COLUMNS {
                // Every frame ties: no trustworthy depth.
                for aggregate in &mut aggregates {
                    aggregate[pixel] = 0.2;
                }
            } else {
                aggregates[0][pixel] = 0.1;
                aggregates[1][pixel] = 0.9;
                aggregates[2][pixel] = 0.1;
            }
        }
    }

    let volume = CostVolume::new(&aggregates, WIDTH as u32, HEIGHT as u32);
    let result = argmax_depth(&volume, None);

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let pixel = y * WIDTH + x;
            if x < FLAT_COLUMNS {
                assert!(result.flat[pixel], "tied pixel {pixel} not flagged");
                assert!(result.margin[pixel] <= FLAT_MARGIN);
                assert!((result.best_energy[pixel] - 0.2).abs() <= EPS);
            } else {
                assert!(!result.flat[pixel], "textured pixel {pixel} flagged");
                assert!((result.field.d[pixel] - 1.0).abs() <= EPS);
                assert!((result.best_energy[pixel] - 0.9).abs() <= EPS);
            }
        }
    }

    // The winning frame costs nothing; the runner-up pays the energy gap.
    let textured = FLAT_COLUMNS;
    assert!(volume.cost(1, textured).abs() <= EPS);
    assert!((volume.cost(0, textured) - 0.8).abs() <= EPS);
}

#[test]
fn supplied_confidence_gates_flat_and_is_preserved() {
    const WIDTH: usize = 8;
    const HEIGHT: usize = 4;

    let mut aggregates = vec![vec![0.1f32; WIDTH * HEIGHT]; 3];
    for aggregate in &mut aggregates {
        for value in aggregate.iter_mut() {
            *value = 0.1;
        }
    }
    aggregates[2].fill(0.9);

    let confidence: Vec<f32> = (0..WIDTH * HEIGHT)
        .map(|pixel| match pixel % 4 {
            0 => 0.0,
            1 => 0.25,
            2 => 0.75,
            _ => f32::NAN,
        })
        .collect();

    let volume = CostVolume::new(&aggregates, WIDTH as u32, HEIGHT as u32);
    let result = argmax_depth(&volume, Some(&confidence));

    for pixel in 0..WIDTH * HEIGHT {
        let expected = match pixel % 4 {
            0 => 0.0,
            1 => 0.25,
            2 => 0.75,
            _ => 0.0,
        };
        assert!((result.field.confidence[pixel] - expected).abs() <= EPS);
        assert_eq!(result.flat[pixel], expected <= 0.0);
        assert!((result.field.d[pixel] - 2.0).abs() <= EPS);
    }
}

#[test]
fn non_finite_energies_do_not_win() {
    let mut aggregates = vec![vec![0.0f32; 4]; 2];
    aggregates[0] = vec![f32::NAN, f32::INFINITY, -1.0, 0.0];
    aggregates[1] = vec![0.5, 0.4, 0.2, f32::NEG_INFINITY];

    let volume = CostVolume::new(&aggregates, 2, 2);
    let result = argmax_depth(&volume, None);

    assert!(result.field.d.iter().all(|value| value.is_finite()));
    assert_eq!(result.field.d, vec![1.0, 1.0, 1.0, 0.0]);
    assert_eq!(result.best_energy, vec![0.5, 0.4, 0.2, 0.0]);
    assert_eq!(result.field.confidence[3], 0.0);
    assert!(result.flat[3]);
}

#[test]
#[should_panic(expected = "energy buffer length mismatch")]
fn cost_volume_rejects_mismatched_maps() {
    let aggregates = vec![vec![0.0f32; 4], vec![0.0f32; 3]];
    let _ = CostVolume::new(&aggregates, 2, 2);
}
