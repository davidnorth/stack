use focus_stack::focus::noise::{
    DEFAULT_NOISE_K, SIGMA_EPSILON, estimate, estimate_level, estimate_levels, noise_floor,
    subtract_noise_floor,
};
use focus_stack::focus::pyramid::{Pyramid, PyramidLevel};

/// Small deterministic generator, so the tests do not need a random crate.
struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    fn uniform(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32) / ((1u64 << 24) as f32)
    }

    fn normal(&mut self) -> f32 {
        let u1 = self.uniform().max(f32::MIN_POSITIVE);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos()
    }
}

fn flat_level(width: u32, height: u32, sigma: f32, seed: u64) -> PyramidLevel {
    let count = (width * height) as usize;
    let mut rng = Lcg::new(seed);
    PyramidLevel {
        width,
        height,
        gaussian: vec![0.5; count],
        laplacian: (0..count).map(|_| sigma * rng.normal()).collect(),
    }
}

#[test]
fn synthetic_sigma_is_within_five_percent_per_level() {
    let (width, height) = (192, 128);
    let expected = [0.01f32, 0.02, 0.04];
    let levels: Vec<_> = expected
        .iter()
        .enumerate()
        .map(|(index, &sigma)| flat_level(width, height, sigma, 0x51ce + index as u64))
        .collect();

    let model = estimate_levels(&levels, DEFAULT_NOISE_K);
    assert_eq!(model.levels.len(), expected.len());

    for (index, (&expected_sigma, estimate)) in expected.iter().zip(&model.levels).enumerate() {
        let relative_error = (estimate.sigma - expected_sigma).abs() / expected_sigma;
        assert!(
            relative_error < 0.05,
            "level {index}: sigma {} differs from {expected_sigma} by {:.2}%",
            estimate.sigma,
            relative_error * 100.0
        );
        assert!(
            (estimate.tau - noise_floor(estimate.sigma, DEFAULT_NOISE_K)).abs() < 1e-9,
            "level {index}: tau does not match the noise floor"
        );
        assert_eq!(estimate.samples, (width * height) as usize);
    }
}

#[test]
fn texture_does_not_inflate_the_estimate() {
    let (width, height) = (320, 240);
    let sigma = 0.012;
    let mut rng = Lcg::new(0x7e47);
    let count = (width * height) as usize;
    let mut gaussian = vec![0.5; count];
    let mut laplacian = vec![0.0; count];

    for y in 0..height as usize {
        for x in 0..width as usize {
            let index = y * width as usize + x;
            if x < width as usize / 2 {
                let (fx, fy) = (x as f32, y as f32);
                gaussian[index] = 0.5 + 0.20 * (fx * 1.7).sin() * (fy * 1.1).cos();
                laplacian[index] = 0.10 * (fx * 0.9).sin() + 0.08 * (fy * 1.3).cos();
            } else {
                laplacian[index] = sigma * rng.normal();
            }
        }
    }

    let level = PyramidLevel {
        width,
        height,
        gaussian,
        laplacian,
    };
    let estimate = estimate_level(&level, DEFAULT_NOISE_K);
    let relative_error = (estimate.sigma - sigma).abs() / sigma;
    assert!(
        relative_error < 0.05,
        "texture inflated sigma to {} (expected {sigma}, {:.2}% error)",
        estimate.sigma,
        relative_error * 100.0
    );
    assert!(
        estimate.samples < count,
        "low-gradient mask should exclude the textured half"
    );
}

#[test]
fn clean_input_uses_epsilon_instead_of_zero() {
    let level = PyramidLevel {
        width: 8,
        height: 8,
        gaussian: vec![0.5; 64],
        laplacian: vec![0.0; 64],
    };

    let estimate = estimate_level(&level, DEFAULT_NOISE_K);
    assert_eq!(estimate.sigma, SIGMA_EPSILON);
    assert!(estimate.tau > 0.0 && estimate.tau.is_finite());
}

#[test]
fn floor_uses_k_times_variance_and_subtraction_saturates() {
    assert!((noise_floor(0.2, DEFAULT_NOISE_K) - 0.12).abs() < 1e-6);

    let mut energy = vec![0.05, 0.12, 0.35];
    subtract_noise_floor(&mut energy, 0.12);
    assert!((energy[0] - 0.0).abs() < 1e-6);
    assert!((energy[1] - 0.0).abs() < 1e-6);
    assert!((energy[2] - 0.23).abs() < 1e-6);
}

#[test]
fn model_has_one_estimate_for_every_pyramid_level() {
    let (width, height) = (64, 48);
    let mut rng = Lcg::new(0xfeed);
    let luma: Vec<f32> = (0..(width * height) as usize)
        .map(|index| {
            let x = (index % width as usize) as f32;
            let y = (index / width as usize) as f32;
            (0.5 + 0.2 * (x * 0.3).sin() + 0.1 * (y * 0.2).cos() + 0.01 * rng.normal())
                .clamp(0.0, 1.0)
        })
        .collect();

    let mut pyramid = Pyramid::new();
    pyramid.build(&luma, width, height);

    let model = estimate(&pyramid, DEFAULT_NOISE_K);
    assert_eq!(model.levels.len(), pyramid.levels().len());
    assert_eq!(model.sigmas().count(), pyramid.levels().len());
    assert!(model.levels.iter().all(|level| {
        level.sigma.is_finite() && level.sigma > 0.0 && level.tau.is_finite() && level.tau >= 0.0
    }));
}
