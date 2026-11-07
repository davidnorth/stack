use focus_stack::align::logpolar::{FourierMellinParams, estimate_rotation_scale};
use std::f64::consts::TAU;

#[derive(Clone, Copy)]
struct Mode {
    frequency_x: f64,
    frequency_y: f64,
    phase: f64,
    amplitude: f64,
}

fn high_modes(count: usize) -> Vec<Mode> {
    let mut state = 0x2545_f491u32;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };
    let mut modes = Vec::with_capacity(count);
    for _ in 0..count {
        let angle = f64::from(next() % 10_000) / 10_000.0 * TAU;
        let radius = 20.0 + f64::from(next() % 10_000) / 10_000.0 * 80.0;
        let amplitude = 0.5 + 0.5 * f64::from(next() % 1_000) / 1_000.0;
        let phase = f64::from(next() % 10_000) / 10_000.0 * TAU;
        modes.push(Mode {
            frequency_x: radius * angle.cos(),
            frequency_y: radius * angle.sin(),
            phase,
            amplitude,
        });
    }
    modes
}

fn scene(x: f64, y: f64, width: usize, height: usize, modes: &[Mode]) -> f32 {
    let mut value = 0.0f64;
    for mode in modes {
        let u = mode.frequency_x * x / width as f64;
        let v = mode.frequency_y * y / height as f64;
        value += mode.amplitude * (TAU * (u + v) + mode.phase).cos();
    }
    (value / (modes.len() as f64).sqrt()) as f32
}

/// Render `reference` transformed by scale `s` and rotation `theta` about the
/// image centre: `source(y) = reference(center + R(-theta)(y - center) / s)`.
/// The alignment transform therefore maps reference to source.
fn render(width: usize, height: usize, scale: f64, rotation: f64, modes: &[Mode]) -> Vec<f32> {
    let center_x = (width as f64 - 1.0) * 0.5;
    let center_y = (height as f64 - 1.0) * 0.5;
    let (sin, cos) = rotation.sin_cos();

    let mut frame = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            let offset_x = x as f64 - center_x;
            let offset_y = y as f64 - center_y;
            let source_x = center_x + (cos * offset_x + sin * offset_y) / scale;
            let source_y = center_y + (-sin * offset_x + cos * offset_y) / scale;
            frame.push(scene(source_x, source_y, width, height, modes));
        }
    }
    frame
}

fn estimate(
    source: &[f32],
    reference: &[f32],
    width: usize,
    height: usize,
) -> focus_stack::align::logpolar::FourierMellinEstimate {
    estimate_rotation_scale(
        source,
        reference,
        width as u32,
        height as u32,
        &FourierMellinParams::default(),
    )
    .expect("valid buffers and parameters")
}

#[test]
fn recovers_rotation_and_scale_within_gate() {
    let width = 256;
    let height = 256;
    let modes = high_modes(256);
    let reference = render(width, height, 1.0, 0.0, &modes);

    let cases: [(f64, f64); 6] = [
        (1.0, 2.0),
        (1.0, -2.0),
        (1.015, 0.0),
        (0.985, 0.0),
        (1.015, 2.0),
        (0.985, -2.0),
    ];

    for (scale, degrees) in cases {
        let rotation = degrees.to_radians();
        let source = render(width, height, scale, rotation, &modes);
        let result = estimate(&source, &reference, width, height);

        let rotation_error = (result.rotation - rotation).abs().to_degrees();
        let scale_error = (result.scale / scale - 1.0).abs();
        assert!(
            rotation_error <= 0.1,
            "rotation error {rotation_error:.4}° for scale {scale}, rotation {degrees}° \
             (estimated {:.5} rad, flipped={})",
            result.rotation,
            result.flipped_180
        );
        assert!(
            scale_error <= 0.001,
            "scale error {:.5}% for scale {scale}, rotation {degrees}° \
             (estimated {:.6}, flipped={})",
            scale_error * 100.0,
            result.scale,
            result.flipped_180
        );
    }
}

#[test]
fn clamps_estimates_to_configured_range() {
    let width = 256;
    let height = 256;
    let modes = high_modes(256);
    let reference = render(width, height, 1.0, 0.0, &modes);
    let source = render(width, height, 1.10, 5.0_f64.to_radians(), &modes);
    let result = estimate(&source, &reference, width, height);

    assert!(
        result.scale <= FourierMellinParams::default().max_scale + 1e-12,
        "{}",
        result.scale
    );
    assert!(
        result.rotation.abs() <= FourierMellinParams::default().max_rotation + 1e-12,
        "{}",
        result.rotation
    );
    assert!(result.scale_clamped);
    assert!(result.rotation_clamped);
}

#[test]
fn accepts_non_power_of_two_log_polar_samples() {
    let width = 192;
    let height = 192;
    let modes = high_modes(256);
    let reference = render(width, height, 1.0, 0.0, &modes);
    let source = render(width, height, 1.01, 1.0_f64.to_radians(), &modes);
    let params = FourierMellinParams {
        radial_samples: 300,
        angular_samples: 600,
        ..FourierMellinParams::default()
    };

    let result = estimate_rotation_scale(&source, &reference, width as u32, height as u32, &params)
        .expect("valid buffers and parameters");

    assert!((result.rotation - 1.0_f64.to_radians()).abs().to_degrees() <= 0.1);
    assert!((result.scale / 1.01 - 1.0).abs() <= 0.001);
}
