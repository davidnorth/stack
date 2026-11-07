use focus_stack::focus::confidence::ConfidenceMap;
use focus_stack::focus::energy::{EnergyMaps, EnergyOptions};
use focus_stack::focus::noise::NoiseModel;
use focus_stack::focus::pyramid::Pyramid;
use focus_stack::stack::pmax::{DEFAULT_BETA, LevelWeights, WeightOptions, level_weights};

const EPS: f32 = 1.0e-5;

fn assert_finite(values: &[f32]) {
    for &value in values {
        assert!(value.is_finite(), "weight is not finite: {value}");
        assert!((0.0..=1.0).contains(&value), "weight out of range: {value}");
    }
}

fn assert_sums_to_one(level: &focus_stack::stack::pmax::WeightLevel) {
    let count = level.len();
    for index in 0..count {
        let luma: f32 = (0..level.frames)
            .map(|frame| level.weights[frame * count + index])
            .sum();
        let chroma: f32 = (0..level.frames)
            .map(|frame| level.chroma[frame * count + index])
            .sum();
        assert!((luma - 1.0).abs() <= EPS, "luma sum {luma}");
        assert!((chroma - 1.0).abs() <= EPS, "chroma sum {chroma}");
    }
}

#[test]
fn weights_and_chroma_sum_to_one() {
    let frame0 = [0.10, 0.20, 0.30, 0.40, 0.50, 0.60];
    let frame1 = [0.60, 0.50, 0.40, 0.30, 0.20, 0.10];
    let frame2 = [0.25, 0.25, 0.10, 0.90, 0.25, 0.25];
    let confidence = [0.0, 0.25, 0.5, 0.75, 1.0, 0.4];

    let weights = level_weights(
        &[&frame0, &frame1, &frame2],
        2,
        3,
        &confidence,
        DEFAULT_BETA,
    );

    assert_eq!(weights.frames, 3);
    assert_finite(&weights.weights);
    assert_finite(&weights.chroma);
    assert_sums_to_one(&weights);
}

#[test]
fn flat_region_is_uniform_and_finite() {
    let frame0 = [0.0; 6];
    let frame1 = [0.0; 6];
    let frame2 = [0.0; 6];
    let confidence = [1.0; 6];

    let weights = level_weights(
        &[&frame0, &frame1, &frame2],
        2,
        3,
        &confidence,
        DEFAULT_BETA,
    );

    assert_eq!(weights.p99, 0.0);
    assert_finite(&weights.weights);
    assert_finite(&weights.chroma);
    for value in &weights.weights {
        assert!((value - 1.0 / 3.0).abs() <= EPS);
    }
    for value in &weights.chroma {
        assert!((value - 1.0 / 3.0).abs() <= EPS);
    }
    assert_sums_to_one(&weights);
}

#[test]
fn non_finite_energies_do_not_produce_nan() {
    let frame0 = [f32::NAN, f32::INFINITY, 0.0];
    let frame1 = [f32::NEG_INFINITY, 0.0, 0.0];
    let confidence = [0.5, f32::NAN, 1.0];

    let weights = level_weights(&[&frame0, &frame1], 1, 3, &confidence, DEFAULT_BETA);

    assert_finite(&weights.weights);
    assert_finite(&weights.chroma);
    assert_sums_to_one(&weights);
}

#[test]
fn confidence_gates_toward_uniform() {
    // Two samples make p99 the maximum, so the well-lit frame is normalized to 1.
    let frame0 = [0.0];
    let frame1 = [1.0];

    let uniform = level_weights(&[&frame0, &frame1], 1, 1, &[0.0], DEFAULT_BETA);
    assert!((uniform.weights[0] - 0.5).abs() <= EPS);
    assert!((uniform.weights[1] - 0.5).abs() <= EPS);

    let confident = level_weights(&[&frame0, &frame1], 1, 1, &[1.0], DEFAULT_BETA);
    let expected = 1.0 / (1.0 + (-DEFAULT_BETA).exp());
    assert!((confident.weights[0] - (1.0 - expected)).abs() <= 1.0e-4);
    assert!((confident.weights[1] - expected).abs() <= 1.0e-4);

    let half = level_weights(&[&frame0, &frame1], 1, 1, &[0.5], DEFAULT_BETA);
    for frame in 0..2 {
        let blended = 0.5 * 0.5 + 0.5 * confident.weights[frame];
        assert!((half.weights[frame] - blended).abs() <= EPS);
    }

    // Gating never favors the lower-energy frame.
    assert!(half.weights[1] > half.weights[0]);
    assert!(confident.weights[1] > half.weights[1]);
}

#[test]
fn temperature_controls_selectivity() {
    let frame0 = [0.0];
    let frame1 = [1.0];

    let cold = level_weights(&[&frame0, &frame1], 1, 1, &[1.0], 0.0);
    assert!((cold.weights[0] - 0.5).abs() <= EPS);
    assert!((cold.weights[1] - 0.5).abs() <= EPS);

    let warm = level_weights(&[&frame0, &frame1], 1, 1, &[1.0], 6.0);
    let hot = level_weights(&[&frame0, &frame1], 1, 1, &[1.0], 12.0);
    assert!(hot.weights[1] > warm.weights[1]);
    assert!(hot.weights[0] < warm.weights[0]);
}

#[test]
fn chroma_weights_are_smoother_than_luma() {
    let frame0 = [0.0, 0.0];
    let frame1 = [1.0, 1.0];
    let weights = level_weights(&[&frame0, &frame1], 2, 1, &[1.0, 1.0], DEFAULT_BETA);

    for index in 0..2 {
        let luma_distance = (weights.weights[index] - weights.weights[2 + index]).abs();
        let chroma_distance = (weights.chroma[index] - weights.chroma[2 + index]).abs();
        assert!(chroma_distance < luma_distance);
        assert!((chroma_distance - 0.5 * luma_distance).abs() <= EPS);
    }
}

#[test]
fn percentile_uses_high_end_of_all_frames() {
    let frame0 = [1.0, 2.0];
    let frame1 = [3.0, 4.0];
    let weights = level_weights(&[&frame0, &frame1], 1, 2, &[1.0, 1.0], DEFAULT_BETA);
    assert!((weights.p99 - 4.0).abs() <= 1.0e-4);

    let flat = level_weights(&[&[2.0, 2.0]], 1, 2, &[1.0, 1.0], DEFAULT_BETA);
    assert!((flat.p99 - 2.0).abs() <= 1.0e-6);
}

fn two_frame_energy_maps() -> (Vec<EnergyMaps>, ConfidenceMap) {
    let width = 8u32;
    let height = 8u32;
    let count = (width * height) as usize;

    let make_frame = |textured_left: bool| {
        let mut luma = Vec::with_capacity(count);
        for y in 0..height as usize {
            for x in 0..width as usize {
                let textured = (x < 4) == textured_left;
                let value = if textured {
                    0.5 + 0.2 * ((x as f32) * 1.7 + (y as f32) * 0.9).sin()
                        + 0.1 * ((x as f32) * 0.4 - (y as f32) * 2.1).cos()
                } else {
                    0.5
                };
                luma.push(value);
            }
        }
        let mut pyramid = Pyramid::new();
        pyramid.build(&luma, width, height);
        EnergyMaps::compute(&pyramid, &NoiseModel::default(), EnergyOptions::default())
    };

    let energies = vec![make_frame(true), make_frame(false)];

    let confidence = (0..count)
        .map(|index| {
            let x = index % width as usize;
            if x < 4 { 0.0 } else { 1.0 }
        })
        .collect();

    let map = ConfidenceMap {
        width,
        height,
        best_frame: vec![0; count],
        best_energy: vec![0.0; count],
        margin: vec![0.0; count],
        confidence,
    };

    (energies, map)
}

#[test]
fn compute_matches_frame_levels_and_confidence_gate() {
    let (energies, confidence) = two_frame_energy_maps();
    let weights = LevelWeights::compute(&energies, &confidence, WeightOptions::default());

    assert_eq!(weights.len(), energies[0].levels().len());
    assert!(!weights.is_empty());

    let finest = weights.level(0).expect("finest level");
    assert_eq!((finest.width, finest.height), (8, 8));
    assert_eq!(finest.frames, 2);
    assert_finite(&finest.weights);
    assert_finite(&finest.chroma);
    assert_sums_to_one(finest);

    // c = 0 on the left half -> exactly uniform weights.
    for y in 0..8usize {
        for x in 0..4usize {
            let index = y * 8 + x;
            assert!((finest.weights[index] - 0.5).abs() <= EPS);
            assert!((finest.weights[64 + index] - 0.5).abs() <= EPS);
        }
    }

    for level in weights.levels() {
        assert_finite(&level.weights);
        assert_finite(&level.chroma);
        assert_sums_to_one(level);
    }

    // Coarser levels sample the full-resolution confidence by `x * 2^l`.
    let first = weights.level(1).expect("level 1");
    assert_eq!((first.width, first.height), (4, 4));
    for y in 0..4usize {
        for x in 0..2usize {
            let index = y * 4 + x;
            assert!((first.weights[index] - 0.5).abs() <= EPS);
        }
    }
}
