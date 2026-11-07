//! Integration coverage for the synthetic stack generator.

use focus_stack::quality::synthetic::{
    Psf, SyntheticConfig, SyntheticError, SyntheticStack, blur_radius,
};
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("fixtures")
        .join("synthetic")
        .join(name)
}

#[test]
fn committed_fixtures_parse_and_validate() {
    for (name, width, height, frames) in [
        ("stack_512.toml", 512, 512, 4),
        ("gaussian_64.toml", 64, 64, 3),
        ("transforms_96.toml", 96, 96, 2),
    ] {
        let config = SyntheticConfig::from_toml_file(&fixture(name)).unwrap();
        assert_eq!(
            (config.width, config.height, config.frames),
            (width, height, frames)
        );
        assert_eq!(config.exposure.len(), frames);
    }
}

#[test]
fn transforms_fixture_keeps_ground_truth_transform() {
    let config = SyntheticConfig::from_toml_file(&fixture("transforms_96.toml")).unwrap();
    let stack = SyntheticStack::generate(&config).unwrap();
    assert_eq!(stack.transforms.len(), 2);
    assert_eq!(stack.transforms[0].tx, 0.0);
    assert!((stack.transforms[1].a - 0.999_695_408_357_074).abs() < 1.0e-15);
    assert!((stack.transforms[1].b - 0.024_997_395_914_712_326).abs() < 1.0e-15);
    assert_eq!(stack.transforms[1].tx, 1.5);
    assert_eq!(stack.transforms[1].ty, -0.75);
}

#[test]
fn committed_512_fixture_renders_with_analytic_radii() {
    let config = SyntheticConfig::from_toml_file(&fixture("stack_512.toml")).unwrap();
    let stack = SyntheticStack::generate(&config).unwrap();
    assert_eq!(stack.frames.len(), config.frames);
    let pixels = config.width as usize * config.height as usize;
    assert_eq!(stack.scene.len(), pixels * 3);
    assert_eq!(stack.depth.len(), pixels);
    assert_eq!(stack.layer_depths.len(), config.layers);
    for frame in &stack.frames {
        assert_eq!(frame.len(), pixels * 3);
        assert!(frame.iter().any(|&v| v > 0));
    }

    let mut saw_positive = false;
    for (frame_index, &focus) in config.focus.iter().enumerate() {
        for (layer_index, &depth) in stack.layer_depths.iter().enumerate() {
            let expected = blur_radius(config.coc_scale, config.c_max, depth, focus);
            let actual = stack.layer_blur_radii[frame_index][layer_index];
            if actual > 0.0 {
                saw_positive = true;
                assert!(
                    (actual - expected).abs() < 1.0e-12,
                    "frame {frame_index} layer {layer_index}: {actual} != {expected}"
                );
            }
        }
    }
    assert!(saw_positive);

    // The fixture must be a stack, not N copies of the same frame.
    assert_ne!(stack.frames[0], stack.frames[1]);
    assert_ne!(stack.frames[1], stack.frames[2]);
}

#[test]
fn gaussian_fixture_is_deterministic() {
    let config = SyntheticConfig::from_toml_file(&fixture("gaussian_64.toml")).unwrap();
    let first = SyntheticStack::generate(&config).unwrap();
    let second = SyntheticStack::generate(&config).unwrap();
    assert_eq!(first.scene, second.scene);
    assert_eq!(first.depth, second.depth);
    assert_eq!(first.frames, second.frames);
}

#[test]
fn delta_response_matches_disk_second_moment() {
    let width = 64;
    let height = 64;
    let mut scene = vec![0.0f32; width * height * 3];
    let center = ((height / 2) * width + width / 2) * 3;
    scene[center..center + 3].fill(1.0);
    let depth = vec![1.0f32; width * height];
    let config = SyntheticConfig {
        width: width as u32,
        height: height as u32,
        frames: 2,
        seed: 5,
        coc_scale: 2.0,
        focus: vec![1.0, 0.5],
        noise_read: 0.0,
        noise_shot: 0.0,
        exposure: vec![1.0, 1.0],
        layers: 4,
        psf: Psf::Disk,
        c_max: 8.0,
        transforms: Vec::new(),
    };
    let stack = SyntheticStack::generate_from_scene(&scene, &depth, &config).unwrap();
    assert_eq!(stack.layer_blur_radii[0][0], 0.0);
    assert!((stack.layer_blur_radii[1][0] - 2.0).abs() < 1.0e-12);

    let in_focus = second_moment(&stack.frame_linear(0), width, height);
    let defocused = second_moment(&stack.frame_linear(1), width, height);
    let expected = 2.0f64.powi(2) / 2.0;
    assert!(in_focus < 1.0e-6, "in-focus moment = {in_focus}");
    assert!(
        (defocused - expected).abs() < expected * 0.15,
        "defocused moment = {defocused}, expected = {expected}"
    );
}

fn second_moment(frame: &[f32], width: usize, height: usize) -> f64 {
    let total: f64 = frame.iter().step_by(3).map(|&v| v as f64).sum();
    let mut moment = 0.0;
    for y in 0..height {
        for x in 0..width {
            let dx = x as f64 - (width / 2) as f64;
            let dy = y as f64 - (height / 2) as f64;
            moment += frame[(y * width + x) * 3] as f64 * (dx * dx + dy * dy);
        }
    }
    if total > 0.0 { moment / total } else { 0.0 }
}

#[test]
fn invalid_configs_are_rejected_with_clear_errors() {
    let error = SyntheticConfig::from_toml_str("width = 8\nheight = 8\n").unwrap_err();
    assert!(matches!(error, SyntheticError::Parse(_)));

    let error = SyntheticConfig::from_toml_str(
        r#"
width = 8
height = 8
frames = 2
seed = 1
coc_scale = 1.0
focus = [0.5]
"#,
    )
    .unwrap_err();
    assert!(matches!(error, SyntheticError::Invalid(_)));
}
