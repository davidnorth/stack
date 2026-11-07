use std::mem::size_of;

use focus_stack::focus::pyramid::{Pyramid, level_count};

fn luma_image(width: u32, height: u32) -> Vec<f32> {
    (0..(width * height) as usize)
        .map(|i| {
            let x = (i % width as usize) as f32;
            let y = (i / width as usize) as f32;
            (0.5 + 0.25 * (x * 0.5).sin() + 0.25 * (y * 0.25).cos()).clamp(0.0, 1.0)
        })
        .collect()
}

fn build(width: u32, height: u32) -> (Pyramid, Vec<f32>) {
    let luma = luma_image(width, height);
    let mut pyramid = Pyramid::new();
    pyramid.build(&luma, width, height);
    (pyramid, luma)
}

#[test]
fn reconstruction_is_identity_within_tolerance() {
    // Odd dimensions exercise the ceil-halving phase.
    let (width, height) = (257, 131);
    let (pyramid, luma) = build(width, height);

    let reconstructed = pyramid.reconstruct();

    assert_eq!(reconstructed.len(), luma.len());
    let max_error = reconstructed
        .iter()
        .zip(&luma)
        .map(|(&a, &b)| (a - b).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_error < 1e-6,
        "reconstruction error {max_error:e} exceeds 1e-6"
    );
}

#[test]
fn level_count_matches_documented_frame_sizes() {
    // docs/focus-measure.md §2: six levels at 24 MP, seven at 60 MP.
    assert_eq!(level_count(6000, 4000), 6);
    assert_eq!(level_count(9504, 6336), 7);
    assert_eq!(level_count(4000, 6000), level_count(6000, 4000));
}

#[test]
fn levels_halve_until_the_coarsest_residual() {
    let (width, height) = (101, 67);
    let (pyramid, _) = build(width, height);

    assert_eq!(pyramid.levels().len(), level_count(width, height));

    let mut expected_width = width;
    let mut expected_height = height;
    for level in pyramid.levels() {
        assert_eq!(
            (level.width, level.height),
            (expected_width, expected_height)
        );
        assert_eq!(
            level.gaussian.len(),
            (expected_width * expected_height) as usize
        );
        assert_eq!(
            level.laplacian.len(),
            (expected_width * expected_height) as usize
        );
        expected_width = expected_width.div_ceil(2);
        expected_height = expected_height.div_ceil(2);
    }

    let coarsest = pyramid.levels().last().expect("at least one level");
    assert_eq!(coarsest.laplacian, coarsest.gaussian);
}

#[test]
fn buffers_are_reused_across_frames() {
    let (width, height) = (320, 240);
    let luma = luma_image(width, height);
    let mut pyramid = Pyramid::new();

    pyramid.build(&luma, width, height);
    let first_ptr = pyramid.levels()[0].gaussian.as_ptr();
    let first_capacity = pyramid.levels()[0].gaussian.capacity();
    let first_bytes = pyramid.memory_bytes();

    pyramid.build(&luma, width, height);
    assert_eq!(pyramid.levels()[0].gaussian.as_ptr(), first_ptr);
    assert_eq!(pyramid.levels()[0].gaussian.capacity(), first_capacity);
    assert_eq!(pyramid.memory_bytes(), first_bytes);
}

#[test]
fn memory_is_measured_and_bounded() {
    let (width, height) = (1024, 768);
    let (pyramid, _) = build(width, height);

    let bytes = pyramid.memory_bytes();
    let base = (width as usize) * (height as usize) * size_of::<f32>();
    let ratio = bytes as f64 / base as f64;
    println!(
        "{width}x{height} pyramid: {:.2} MiB ({ratio:.2}x base luma)",
        bytes as f64 / (1024.0 * 1024.0)
    );
    assert!(bytes > base, "pyramid must store more than the input luma");
    assert!(ratio < 4.0, "pyramid memory {ratio:.2}x exceeds 4x base");

    // Project the 24 MP footprint from the level dimensions without allocating
    // the full frame.
    let (mut w, mut h) = (6000u32, 4000u32);
    let mut samples = 0usize;
    let mut blur_samples = 0usize;
    for level in 0..level_count(w, h) {
        samples += 2 * (w as usize) * (h as usize);
        if level == 0 {
            blur_samples = (h as usize) * ((w as usize).div_ceil(2));
        }
        w = w.div_ceil(2);
        h = h.div_ceil(2);
    }
    let projected = (samples + blur_samples) * size_of::<f32>();
    println!(
        "24 MP projection: {:.2} MiB ({} levels)",
        projected as f64 / (1024.0 * 1024.0),
        level_count(6000, 4000)
    );
    assert!(
        projected < 400 * 1024 * 1024,
        "24 MP pyramid projection exceeds 400 MiB"
    );
}
