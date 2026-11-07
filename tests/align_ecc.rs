use focus_stack::Similarity;
use focus_stack::align::ecc::{EccParams, refine_similarity};
use std::f64::consts::TAU;

#[derive(Clone, Copy)]
struct Mode {
    frequency_x: f64,
    frequency_y: f64,
    phase: f64,
    amplitude: f64,
}

fn modes(seed: u32, count: usize) -> Vec<Mode> {
    let mut state = seed;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };
    let mut modes = Vec::with_capacity(count);
    for _ in 0..count {
        let frequency_x = f64::from(next() % 1201) / 100.0 - 6.0;
        let frequency_y = f64::from(next() % 1201) / 100.0 - 6.0;
        let phase = f64::from(next() % 10_000) / 10_000.0 * TAU;
        let amplitude = 0.5 + 0.5 * f64::from(next() % 1_000) / 1_000.0;
        modes.push(Mode {
            frequency_x,
            frequency_y,
            phase,
            amplitude,
        });
    }
    modes
}

fn scene(x: f64, y: f64, width: usize, height: usize, modes: &[Mode]) -> f32 {
    let mut value = 0.0f64;
    let mut norm = 0.0f64;
    for mode in modes {
        let u = mode.frequency_x * x / width as f64;
        let v = mode.frequency_y * y / height as f64;
        value += mode.amplitude * (TAU * (u + v) + mode.phase).cos();
        norm += mode.amplitude * mode.amplitude;
    }
    (0.5 + 0.15 * value / norm.sqrt()) as f32
}

fn similarity(scale: f64, degrees: f64, tx: f64, ty: f64) -> Similarity {
    let angle = degrees.to_radians();
    Similarity {
        a: scale * angle.cos(),
        b: scale * angle.sin(),
        tx,
        ty,
    }
}

fn render_reference(width: usize, height: usize, modes: &[Mode]) -> Vec<f32> {
    let mut image = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            image.push(scene(x as f64, y as f64, width, height, modes));
        }
    }
    image
}

/// `source(y) = reference(W^-1(y))`, so `W` maps reference coordinates to
/// source coordinates.
fn render_source(width: usize, height: usize, transform: &Similarity, modes: &[Mode]) -> Vec<f32> {
    let determinant = transform.a * transform.a + transform.b * transform.b;
    let mut image = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let dx = x as f64 - transform.tx;
            let dy = y as f64 - transform.ty;
            let reference_x = (transform.a * dx + transform.b * dy) / determinant;
            let reference_y = (-transform.b * dx + transform.a * dy) / determinant;
            image.push(scene(reference_x, reference_y, width, height, modes));
        }
    }
    image
}

fn apply(transform: &Similarity, x: f64, y: f64) -> (f64, f64) {
    (
        transform.a * x - transform.b * y + transform.tx,
        transform.b * x + transform.a * y + transform.ty,
    )
}

fn displacement_error(
    estimated: &Similarity,
    truth: &Similarity,
    width: usize,
    height: usize,
) -> f64 {
    let points = [
        (0.0, 0.0),
        (width as f64 - 1.0, 0.0),
        (0.0, height as f64 - 1.0),
        (width as f64 - 1.0, height as f64 - 1.0),
        ((width as f64 - 1.0) * 0.5, (height as f64 - 1.0) * 0.5),
    ];
    points
        .iter()
        .map(|&(x, y)| {
            let (ex, ey) = apply(estimated, x, y);
            let (tx, ty) = apply(truth, x, y);
            (ex - tx).hypot(ey - ty)
        })
        .fold(0.0f64, f64::max)
}

fn identity() -> Similarity {
    Similarity {
        a: 1.0,
        b: 0.0,
        tx: 0.0,
        ty: 0.0,
    }
}

fn refine(
    source: &[f32],
    reference: &[f32],
    width: usize,
    height: usize,
    initial: &Similarity,
    mask: Option<&[f32]>,
) -> focus_stack::align::ecc::EccResult {
    refine_similarity(
        source,
        reference,
        width as u32,
        height as u32,
        initial,
        mask,
        &EccParams::default(),
    )
    .expect("valid buffers and parameters")
}

const WIDTH: usize = 256;
const HEIGHT: usize = 256;

#[test]
fn recovers_similarity_from_identity_with_subpixel_accuracy() {
    let modes = modes(0x1234_5678, 48);
    let reference = render_reference(WIDTH, HEIGHT, &modes);
    let truth = similarity(1.006, 0.35, 2.3, -1.7);
    let source = render_source(WIDTH, HEIGHT, &truth, &modes);

    let result = refine(&source, &reference, WIDTH, HEIGHT, &identity(), None);
    let error = displacement_error(&result.transform, &truth, WIDTH, HEIGHT);

    assert!(
        error < 0.05,
        "residual {error:.4} px (transform {:?}, truth {truth:?})",
        result.transform
    );
    assert!(
        result.correlation > 0.99,
        "correlation {}",
        result.correlation
    );
    assert!(result.is_reliable());
}

#[test]
fn is_robust_to_five_percent_outlier_pixels() {
    let modes = modes(0x0bad_f00d, 48);
    let reference = render_reference(WIDTH, HEIGHT, &modes);
    let truth = similarity(0.995, -0.4, -3.1, 2.6);
    let mut source = render_source(WIDTH, HEIGHT, &truth, &modes);

    let mut state = 0xdead_beefu32;
    for value in &mut source {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        if state % 100 < 5 {
            *value = if state & 0x100 == 0 { 0.0 } else { 1.0 };
        }
    }

    let result = refine(&source, &reference, WIDTH, HEIGHT, &identity(), None);
    let error = displacement_error(&result.transform, &truth, WIDTH, HEIGHT);

    assert!(
        error < 0.05,
        "residual {error:.4} px with 5% outliers (transform {:?})",
        result.transform
    );
}

#[test]
fn composes_with_a_coarse_estimate() {
    let modes = modes(0x51ee_d0f1, 48);
    let reference = render_reference(WIDTH, HEIGHT, &modes);
    let truth = similarity(1.008, 0.6, 4.0, -2.5);
    let source = render_source(WIDTH, HEIGHT, &truth, &modes);

    // Fourier-Mellin gets rotation and scale to within ~0.2°, and phase
    // correlation leaves a residual translation of a few pixels.
    let coarse = similarity(1.008 * 1.002, 0.6 + 0.12, 4.0 + 1.8, -2.5 - 1.4);
    let result = refine(&source, &reference, WIDTH, HEIGHT, &coarse, None);
    let error = displacement_error(&result.transform, &truth, WIDTH, HEIGHT);

    assert!(
        error < 0.05,
        "residual {error:.4} px from coarse start (transform {:?})",
        result.transform
    );
}

#[test]
fn mask_excludes_an_occluded_patch() {
    let modes = modes(0x00c0_ffee, 48);
    let reference = render_reference(WIDTH, HEIGHT, &modes);
    let truth = similarity(1.004, 0.25, 1.7, 1.1);
    let mut source = render_source(WIDTH, HEIGHT, &truth, &modes);

    let mut mask = vec![1.0f32; WIDTH * HEIGHT];
    for y in 90..150 {
        for x in 60..120 {
            source[y * WIDTH + x] = 0.85 * (x as f32) / WIDTH as f32;
            mask[y * WIDTH + x] = 0.0;
        }
    }

    let result = refine(&source, &reference, WIDTH, HEIGHT, &identity(), Some(&mask));
    let error = displacement_error(&result.transform, &truth, WIDTH, HEIGHT);

    assert!(
        error < 0.05,
        "residual {error:.4} px with an occluded patch (transform {:?})",
        result.transform
    );
}

#[test]
fn reports_low_correlation_for_unrelated_images() {
    let mut state = 0x1111_1111u32;
    let mut noise = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / 16_777_216.0
    };
    let reference: Vec<f32> = (0..WIDTH * HEIGHT).map(|_| noise()).collect();
    let unrelated: Vec<f32> = (0..WIDTH * HEIGHT).map(|_| noise()).collect();

    let result = refine(&unrelated, &reference, WIDTH, HEIGHT, &identity(), None);
    assert!(
        result.correlation < 0.3,
        "correlation {}",
        result.correlation
    );
    assert!(result.correlation_warns);
    assert!(!result.is_reliable());
}
