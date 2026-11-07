//! Hybrid mask and automatic method selection (§4.3–§4.4).

use std::str::FromStr;

use focus_stack::stack::hybrid::{
    DetailOptions, MASK_CONFIDENCE_HIGH, MASK_CONFIDENCE_LOW, MaskOptions, hybrid_mask,
    stack_hybrid_masked, transfer_detail, transfer_detail_masked,
};
use focus_stack::stack::method::{
    AUTO_CONFIDENT_MIN_FRACTION, HybridInputs, HybridParameters, Method, MethodOptions,
    SelectionReason, SelectionStats, select_method, select_method_with,
};

const WIDTH: usize = 64;
const HEIGHT: usize = 64;

fn assert_close(left: f32, right: f32) {
    assert!(
        (left - right).abs() < 1.0e-6,
        "expected {left} to be close to {right}"
    );
}

fn sample(mask: &[f32], x: usize, y: usize) -> f32 {
    mask[y * WIDTH + x]
}

#[test]
fn mask_opens_only_on_confident_textured_pixels() {
    let mut confidence = vec![0.0f32; WIDTH * HEIGHT];
    let mut energy = vec![1.0f32; WIDTH * HEIGHT];
    for y in 0..HEIGHT {
        for x in 0..WIDTH / 2 {
            // Low-confidence, low-texture half.
            confidence[y * WIDTH + x] = 0.0;
            energy[y * WIDTH + x] = 0.01;
        }
        for x in WIDTH / 2..WIDTH {
            // Confident, textured half.
            confidence[y * WIDTH + x] = 1.0;
            energy[y * WIDTH + x] = 1.0;
        }
    }
    let guide = vec![0.5f32; WIDTH * HEIGHT];

    let mask = hybrid_mask(
        &confidence,
        &energy,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        0.1,
        MaskOptions::default(),
    );

    assert_close(sample(&mask, 8, HEIGHT / 2), 0.0);
    assert_close(sample(&mask, WIDTH - 8, HEIGHT / 2), 1.0);
    assert!(mask.iter().all(|value| (0.0..=1.0).contains(value)));
}

#[test]
fn mask_closes_on_smooth_low_confidence_stack() {
    let confidence = vec![1.0f32; WIDTH * HEIGHT];
    let energy = vec![0.01f32; WIDTH * HEIGHT];
    let guide = vec![0.5f32; WIDTH * HEIGHT];

    let mask = hybrid_mask(
        &confidence,
        &energy,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        0.1,
        MaskOptions::default(),
    );

    assert!(mask.iter().all(|&value| value <= 1.0e-6));
}

#[test]
fn mask_confidence_ramp_uses_documented_edges() {
    let guide = vec![0.5f32; WIDTH * HEIGHT];
    let energy = vec![1.0f32; WIDTH * HEIGHT];
    let confidence = vec![MASK_CONFIDENCE_LOW; WIDTH * HEIGHT];
    let mask = hybrid_mask(
        &confidence,
        &energy,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        0.1,
        MaskOptions::default(),
    );
    assert!(mask.iter().all(|&value| value <= 1.0e-6));

    let confidence = vec![MASK_CONFIDENCE_HIGH; WIDTH * HEIGHT];
    let mask = hybrid_mask(
        &confidence,
        &energy,
        &guide,
        WIDTH as u32,
        HEIGHT as u32,
        0.1,
        MaskOptions::default(),
    );
    assert!(mask.iter().all(|&value| (value - 1.0).abs() < 1.0e-6));
}

#[test]
fn masked_transfer_is_dmap_alone_when_mask_is_closed() {
    let dmap = vec![0.4f32; WIDTH * HEIGHT];
    let pmax: Vec<f32> = (0..WIDTH * HEIGHT)
        .map(|index| 0.4 + 0.02 * ((index % 7) as f32))
        .collect();
    let mask = vec![0.0f32; WIDTH * HEIGHT];

    let out = transfer_detail_masked(
        &dmap,
        &pmax,
        &mask,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
    );

    for (&actual, &expected) in out.iter().zip(&dmap) {
        assert_close(actual, expected);
    }
}

#[test]
fn fully_open_mask_matches_unmasked_transfer() {
    let dmap = vec![0.4f32; WIDTH * HEIGHT];
    let pmax: Vec<f32> = (0..WIDTH * HEIGHT)
        .map(|index| 0.4 + 0.02 * ((index % 7) as f32))
        .collect();
    let mask = vec![1.0f32; WIDTH * HEIGHT];
    let options = DetailOptions::default();

    let masked = transfer_detail_masked(&dmap, &pmax, &mask, WIDTH as u32, HEIGHT as u32, options);
    let unmasked = transfer_detail(&dmap, &pmax, WIDTH as u32, HEIGHT as u32, options);

    for (&masked, &unmasked) in masked.iter().zip(&unmasked) {
        assert_close(masked, unmasked);
    }
}

#[test]
fn masked_rgb_stack_preserves_dmap_where_mask_is_closed() {
    let count = WIDTH * HEIGHT;
    let dmap: Vec<f32> = std::iter::repeat_n([0.3, 0.4, 0.5], count)
        .flatten()
        .collect();
    let pmax: Vec<f32> = dmap.iter().map(|value| value + 0.01).collect();
    let mask = vec![0.0f32; count];

    let out = stack_hybrid_masked(
        &dmap,
        &pmax,
        &mask,
        WIDTH as u32,
        HEIGHT as u32,
        DetailOptions::default(),
    );

    for (&actual, &expected) in out.iter().zip(&dmap) {
        assert_close(actual, expected);
    }
}

#[test]
fn auto_prefers_hybrid_on_textured_stacks() {
    let selection = select_method(Method::Auto, 0.75);
    assert_eq!(selection.method, Method::Hybrid);
    assert_eq!(selection.reason, SelectionReason::Textured);
    assert!(selection.is_hybrid());
}

#[test]
fn auto_prefers_dmap_on_smooth_or_low_confidence_stacks() {
    let selection = select_method(Method::Auto, 0.10);
    assert_eq!(selection.method, Method::Dmap);
    assert_eq!(selection.reason, SelectionReason::LowConfidence);
    assert!(!selection.is_hybrid());
}

#[test]
fn auto_threshold_is_strictly_below_forty_percent() {
    assert_eq!(select_method(Method::Auto, 0.39).method, Method::Dmap);
    assert_eq!(
        select_method(Method::Auto, AUTO_CONFIDENT_MIN_FRACTION).method,
        Method::Hybrid
    );
}

#[test]
fn explicit_methods_override_auto_decision() {
    for method in [Method::Pmax, Method::Dmap, Method::Hybrid] {
        let selection = select_method(method, 0.01);
        assert_eq!(selection.method, method);
        assert_eq!(selection.reason, SelectionReason::Explicit);
    }
}

#[test]
fn selection_uses_plumbed_threshold_and_reports_stats() {
    let options = MethodOptions {
        method: Method::Auto,
        auto_confident_min_fraction: 0.60,
        ..MethodOptions::default()
    };
    let stats = SelectionStats::from_percentages(50.0, 92.0);
    let selection = select_method_with(options, stats);

    assert_eq!(selection.method, Method::Dmap);
    assert_eq!(selection.reason, SelectionReason::LowConfidence);
    assert_close(selection.confident_fraction, 0.5);
    assert_close(selection.coverage_fraction, 0.92);
    assert_close(selection.auto_confident_min_fraction, 0.6);

    let selection = options.select(SelectionStats::from_percentages(70.0, 92.0));
    assert_eq!(selection.method, Method::Hybrid);
    assert_eq!(selection.reason, SelectionReason::Textured);
}

#[test]
fn method_names_round_trip() {
    for method in [Method::Auto, Method::Pmax, Method::Dmap, Method::Hybrid] {
        assert_eq!(Method::from_str(method.as_str()), Ok(method));
        assert_eq!(method.to_string(), method.as_str());
    }
    assert!(Method::from_str("bogus").is_err());
}

#[test]
fn hybrid_parameters_run_the_masked_pipeline() {
    let count = WIDTH * HEIGHT;
    let dmap: Vec<f32> = std::iter::repeat_n([0.3, 0.4, 0.5], count)
        .flatten()
        .collect();
    let pmax: Vec<f32> = dmap.iter().map(|value| value + 0.01).collect();
    let confidence = vec![0.0f32; count];
    let energy = vec![0.0f32; count];
    let guide = vec![0.5f32; count];

    let result = HybridParameters::default().stack(HybridInputs {
        dmap: &dmap,
        pmax: &pmax,
        confidence: &confidence,
        best_energy: &energy,
        guide: &guide,
        width: WIDTH as u32,
        height: HEIGHT as u32,
        threshold: 0.1,
    });

    assert_eq!(result.rgb.len(), dmap.len());
    for (&actual, &expected) in result.rgb.iter().zip(&dmap) {
        assert_close(actual, expected);
    }
}
