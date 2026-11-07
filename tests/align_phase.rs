use focus_stack::align::phase::{MIN_CONFIDENCE, PhaseError, phase_correlate};

const TAU: f64 = std::f64::consts::TAU;

/// Highest frequency, in cycles across the frame, used by the test scene.
/// Keeping the content band-limited below Nyquist makes fractional sampling
/// well defined and gives a realistic, dense low-pass spectrum.
const BAND: i32 = 12;

#[derive(Clone, Copy)]
struct Mode {
    frequency_x: f64,
    frequency_y: f64,
    phase: f64,
    amplitude: f64,
}

fn modes() -> Vec<Mode> {
    let mut state = 0x1234_5678u32;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };

    let mut modes = Vec::new();
    for frequency_x in -BAND..=BAND {
        for frequency_y in -BAND..=BAND {
            if frequency_x == 0 && frequency_y == 0 {
                continue;
            }
            let amplitude = 0.5 + 0.5 * f64::from(next() % 1_000) / 1_000.0;
            let phase = f64::from(next() % 10_000) / 10_000.0 * TAU;
            modes.push(Mode {
                frequency_x: f64::from(frequency_x),
                frequency_y: f64::from(frequency_y),
                phase,
                amplitude,
            });
        }
    }
    modes
}

/// Deterministic, band-limited, periodic test scene. Sampling it at shifted
/// coordinates is an exact translation of the underlying continuous signal.
fn scene(x: f64, y: f64, width: usize, height: usize, modes: &[Mode]) -> f32 {
    let mut value = 0.0f64;
    for mode in modes {
        let u = mode.frequency_x * x / width as f64;
        let v = mode.frequency_y * y / height as f64;
        value += mode.amplitude * (TAU * (u + v) + mode.phase).cos();
    }
    (value / (modes.len() as f64).sqrt()) as f32
}

/// `source(x + dx, y + dy) == reference(x, y)` for the returned frame.
fn render(width: usize, height: usize, dx: f64, dy: f64, modes: &[Mode]) -> Vec<f32> {
    let mut frame = Vec::with_capacity(width * height);
    for y in 0..height {
        for x in 0..width {
            frame.push(scene(x as f64 - dx, y as f64 - dy, width, height, modes));
        }
    }
    frame
}

fn estimate(source: &[f32], reference: &[f32], width: usize, height: usize) -> (f64, f64) {
    let result =
        phase_correlate(source, reference, width as u32, height as u32).expect("valid buffers");
    (result.dx, result.dy)
}

fn assert_shift(actual: f64, expected: f64, axis: &str, case: (f64, f64)) {
    assert!(
        (actual - expected).abs() <= 0.05,
        "{axis} error for shift {case:?}: expected {expected}, got {actual}"
    );
}

#[test]
fn recovers_known_integer_shifts() {
    let width = 64;
    let height = 64;
    let modes = modes();
    let reference = render(width, height, 0.0, 0.0, &modes);

    let cases = [
        (0.0, 0.0),
        (1.0, 0.0),
        (0.0, -3.0),
        (5.0, -4.0),
        (-7.0, 6.0),
        (12.0, 10.0),
    ];

    for case in cases {
        let source = render(width, height, case.0, case.1, &modes);
        let (dx, dy) = estimate(&source, &reference, width, height);
        assert_shift(dx, case.0, "dx", case);
        assert_shift(dy, case.1, "dy", case);
    }
}

#[test]
fn recovers_quarter_pixel_and_other_fractional_shifts() {
    let width = 64;
    let height = 64;
    let modes = modes();
    let reference = render(width, height, 0.0, 0.0, &modes);

    let cases = [
        (0.25, 0.0),
        (0.0, 0.25),
        (0.25, 0.25),
        (-0.25, 0.5),
        (0.5, -0.25),
        (0.75, 0.75),
        (-0.75, -0.25),
    ];

    for case in cases {
        let source = render(width, height, case.0, case.1, &modes);
        let (dx, dy) = estimate(&source, &reference, width, height);
        assert_shift(dx, case.0, "dx", case);
        assert_shift(dy, case.1, "dy", case);
    }
}

#[test]
fn recovers_impulse_shift_with_padded_dimensions() {
    // Non-power-of-two axes exercise zero-padding; two impulses give an exact
    // correlation peak regardless of edge effects.
    let width = 5;
    let height = 3;
    let mut reference = vec![0.0f32; width * height];
    reference[0] = 1.0;
    let mut source = vec![0.0f32; width * height];
    source[width + 3] = 1.0;

    let result =
        phase_correlate(&source, &reference, width as u32, height as u32).expect("valid buffers");

    assert!((result.dx - 3.0).abs() < 1e-6, "{}", result.dx);
    assert!((result.dy - 1.0).abs() < 1e-6, "{}", result.dy);
}

#[test]
fn confidence_is_high_for_a_pure_translation() {
    let width = 64;
    let height = 64;
    let modes = modes();
    let reference = render(width, height, 0.0, 0.0, &modes);
    let source = render(width, height, 2.25, -1.5, &modes);

    let result =
        phase_correlate(&source, &reference, width as u32, height as u32).expect("valid buffers");

    assert!(result.confidence >= MIN_CONFIDENCE, "{}", result.confidence);
    assert!(result.is_reliable());
    assert!(result.peak > 0.0);
}

#[test]
fn zero_images_are_reported_as_unreliable() {
    let zero = vec![0.0; 64 * 64];
    let result = phase_correlate(&zero, &zero, 64, 64).expect("valid buffers");

    assert_eq!(result.dx, 0.0);
    assert_eq!(result.dy, 0.0);
    assert_eq!(result.confidence, 0.0);
    assert!(!result.is_reliable());
}

#[test]
fn rejects_empty_or_mismatched_input() {
    assert_eq!(
        phase_correlate(&[], &[], 0, 10).unwrap_err(),
        PhaseError::EmptyInput
    );
    assert_eq!(
        phase_correlate(&[0.0; 3], &[0.0; 4], 2, 2).unwrap_err(),
        PhaseError::LengthMismatch {
            expected: 4,
            source: 3,
            reference: 4,
        }
    );
}
