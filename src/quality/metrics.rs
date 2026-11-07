//! PSNR, SSIM, detail, halo, noise, and delta-E00 metrics.
//!
//! Implements the metric table from `docs/quality.md` §3. Images are linear
//! light; the `eval` subcommand decodes encoded sRGB TIFFs first. Reference
//! values are fixed by the unit tests in `tests/metrics.rs`.

use std::fmt;

use crate::cli::EvalArgs;
use crate::color::luma::linear_luma;
use crate::color::transfer::srgb_decode;
use crate::io::tiff_read::read_tiff;

/// SSIM window radius in pixels (`11x11` window).
pub const SSIM_RADIUS: usize = 5;

/// SSIM Gaussian standard deviation.
pub const SSIM_SIGMA: f64 = 1.5;

/// PSNR target from `docs/quality.md` §3, in dB.
pub const TARGET_PSNR_DB: f64 = 38.0;
/// SSIM target.
pub const TARGET_SSIM: f64 = 0.98;
/// Detail-recovery target range.
pub const TARGET_DETAIL: (f64, f64) = (0.95, 1.10);
/// Halo-overshoot target.
pub const TARGET_HALO: f64 = 0.02;
/// Noise-gain target.
pub const TARGET_NOISE_GAIN: f64 = 1.10;
/// Mean delta-E00 target.
pub const TARGET_DELTA_E00: f64 = 1.0;

/// Interleaved linear-light RGB image.
#[derive(Debug, Clone, PartialEq)]
pub struct LinearImage {
    pub width: u32,
    pub height: u32,
    /// Interleaved RGB, length `width * height * 3`.
    pub rgb: Vec<f32>,
}

impl LinearImage {
    /// Build an image, validating the buffer length.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError::InvalidBuffer`] when `rgb` has the wrong length.
    pub fn new(width: u32, height: u32, rgb: Vec<f32>) -> Result<Self, MetricsError> {
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or(MetricsError::InvalidBuffer {
                expected: usize::MAX,
                got: rgb.len(),
            })?;
        if rgb.len() != expected {
            return Err(MetricsError::InvalidBuffer {
                expected,
                got: rgb.len(),
            });
        }
        Ok(Self { width, height, rgb })
    }

    /// Number of pixels.
    #[must_use]
    pub fn pixel_count(&self) -> usize {
        self.width as usize * self.height as usize
    }

    /// Linear Rec. 709 luma, one value per pixel.
    #[must_use]
    pub fn luma(&self) -> Vec<f32> {
        self.rgb
            .as_chunks::<3>()
            .0
            .iter()
            .map(|pixel| linear_luma(pixel[0], pixel[1], pixel[2]))
            .collect()
    }
}

/// Metric input or dimension failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetricsError {
    /// The RGB buffer does not match the declared dimensions.
    InvalidBuffer { expected: usize, got: usize },
    /// Two images or a depth map have different dimensions.
    DimensionMismatch {
        expected: (u32, u32),
        got: (u32, u32),
    },
    /// An image has no pixels.
    Empty,
}

impl fmt::Display for MetricsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBuffer { expected, got } => {
                write!(
                    formatter,
                    "RGB buffer has {got} samples, expected {expected}"
                )
            }
            Self::DimensionMismatch { expected, got } => write!(
                formatter,
                "image dimensions {}x{} do not match {}x{}",
                got.0, got.1, expected.0, expected.1
            ),
            Self::Empty => formatter.write_str("image has no pixels"),
        }
    }
}

impl std::error::Error for MetricsError {}

/// All metrics from one truth/test comparison.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// PSNR on linear luma, in dB.
    pub psnr_db: f64,
    /// Mean SSIM on linear luma.
    pub ssim: f64,
    /// `sum |grad test| / sum |grad truth|` over the GT-sharp mask.
    pub detail_recovery: f64,
    /// P99 absolute overshoot in the depth-edge band, normalized by local
    /// contrast.
    pub halo_overshoot: f64,
    /// Test/input noise ratio in GT-flat regions; `None` when no input was
    /// supplied.
    pub noise_gain: Option<f64>,
    /// Mean CIEDE2000 color error in GT-flat regions.
    pub delta_e00: f64,
}

impl Metrics {
    /// JSON object suitable for CI. Field order is stable.
    #[must_use]
    pub fn to_json(&self) -> String {
        let noise = self
            .noise_gain
            .map_or_else(|| "null".to_string(), json_number);
        format!(
            concat!(
                "{{\n",
                "  \"psnr_db\": {},\n",
                "  \"ssim\": {},\n",
                "  \"detail_recovery\": {},\n",
                "  \"halo_overshoot\": {},\n",
                "  \"noise_gain\": {},\n",
                "  \"delta_e00\": {}\n",
                "}}\n"
            ),
            json_number(self.psnr_db),
            json_number(self.ssim),
            json_number(self.detail_recovery),
            json_number(self.halo_overshoot),
            noise,
            json_number(self.delta_e00)
        )
    }

    /// Human-readable metric table.
    #[must_use]
    pub fn table(&self) -> String {
        use std::fmt::Write as _;

        let noise = self
            .noise_gain
            .map_or_else(|| "n/a".to_string(), |value| format!("{value:.4}"));
        let mut out = String::new();
        let _ = writeln!(out, "{:<18} {:>10}  Target", "Metric", "Value");
        let _ = writeln!(out, "{}", "-".repeat(44));
        let _ = writeln!(
            out,
            "{:<18} {:>10.4}  >= {TARGET_PSNR_DB:.1}",
            "PSNR (dB)", self.psnr_db
        );
        let _ = writeln!(
            out,
            "{:<18} {:>10.4}  >= {TARGET_SSIM:.2}",
            "SSIM", self.ssim
        );
        let _ = writeln!(
            out,
            "{:<18} {:>10.4}  {:.2}-{:.2}",
            "Detail recovery", self.detail_recovery, TARGET_DETAIL.0, TARGET_DETAIL.1
        );
        let _ = writeln!(
            out,
            "{:<18} {:>10.4}  <= {TARGET_HALO:.2}",
            "Halo overshoot", self.halo_overshoot
        );
        let _ = writeln!(
            out,
            "{:<18} {:>10}  <= {TARGET_NOISE_GAIN:.2}",
            "Noise gain", noise
        );
        let _ = writeln!(
            out,
            "{:<18} {:>10.4}  <= {TARGET_DELTA_E00:.1}",
            "Delta E00", self.delta_e00
        );
        out
    }
}

/// Compare a test image with linear-light ground truth.
///
/// `depth` is a per-pixel relative depth map used to find depth edges for the
/// halo metric. Without it, strong luma edges in the truth image are used.
/// `sharpest_input` supplies the noise-gain denominator.
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ or an image is empty.
pub fn evaluate(
    truth: &LinearImage,
    test: &LinearImage,
    depth: Option<&[f32]>,
    sharpest_input: Option<&LinearImage>,
) -> Result<Metrics, MetricsError> {
    ensure_same_size(truth, test)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    if let Some(values) = depth
        && values.len() != truth.pixel_count()
    {
        return Err(MetricsError::InvalidBuffer {
            expected: truth.pixel_count(),
            got: values.len(),
        });
    }
    if let Some(input) = sharpest_input {
        ensure_same_size(truth, input)?;
    }

    let truth_luma = truth.luma();
    let test_luma = test.luma();
    let truth_gradient = gradient_magnitude(&truth_luma, truth.width, truth.height);
    let test_gradient = gradient_magnitude(&test_luma, test.width, test.height);
    let sharp_threshold = percentile(&truth_gradient, 0.70);
    let sharp_mask: Vec<bool> = truth_gradient
        .iter()
        .map(|&value| value > sharp_threshold)
        .collect();

    let halo_edges = depth.map_or_else(
        || luma_edges(truth, 0.95),
        |values| depth_edges(values, truth.width, truth.height),
    );

    Ok(Metrics {
        psnr_db: psnr_from_luma(&truth_luma, &test_luma),
        ssim: ssim_from_luma(&truth_luma, &test_luma, truth.width, truth.height),
        detail_recovery: detail_from_gradients(&truth_gradient, &test_gradient, &sharp_mask),
        halo_overshoot: halo_from_edge_mask(
            &truth_luma,
            &test_luma,
            truth.width,
            truth.height,
            &halo_edges,
        ),
        noise_gain: sharpest_input
            .and_then(|input| noise_gain_from_luma(&test_luma, &input.luma(), &sharp_mask)),
        delta_e00: delta_e00_in_mask(truth, test, &sharp_mask)?,
    })
}

/// PSNR on linear luma. Returns infinity for identical images.
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ or an image is empty.
pub fn psnr(truth: &LinearImage, test: &LinearImage) -> Result<f64, MetricsError> {
    ensure_same_size(truth, test)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    Ok(psnr_from_luma(&truth.luma(), &test.luma()))
}

/// Mean SSIM using an 11x11 Gaussian window with sigma 1.5 on linear luma.
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ or an image is empty.
pub fn ssim(truth: &LinearImage, test: &LinearImage) -> Result<f64, MetricsError> {
    ensure_same_size(truth, test)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    Ok(ssim_from_luma(
        &truth.luma(),
        &test.luma(),
        truth.width,
        truth.height,
    ))
}

/// Detail recovery over the GT-sharp mask (`|grad truth| > p70`).
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ or an image is empty.
pub fn detail_recovery(truth: &LinearImage, test: &LinearImage) -> Result<f64, MetricsError> {
    ensure_same_size(truth, test)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    let truth_gradient = gradient_magnitude(&truth.luma(), truth.width, truth.height);
    let test_gradient = gradient_magnitude(&test.luma(), test.width, test.height);
    let threshold = percentile(&truth_gradient, 0.70);
    let mask: Vec<bool> = truth_gradient
        .iter()
        .map(|&value| value > threshold)
        .collect();
    Ok(detail_from_gradients(
        &truth_gradient,
        &test_gradient,
        &mask,
    ))
}

/// P99 halo overshoot in a 5 px band around `edges`, normalized by local
/// contrast in the truth luma.
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ, `edges` has the wrong
/// length, or an image is empty.
pub fn halo_overshoot(
    truth: &LinearImage,
    test: &LinearImage,
    edges: &[bool],
) -> Result<f64, MetricsError> {
    ensure_same_size(truth, test)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    if edges.len() != truth.pixel_count() {
        return Err(MetricsError::InvalidBuffer {
            expected: truth.pixel_count(),
            got: edges.len(),
        });
    }
    Ok(halo_from_edge_mask(
        &truth.luma(),
        &test.luma(),
        truth.width,
        truth.height,
        edges,
    ))
}

/// Noise gain: test luma std over sharpest-input luma std in GT-flat regions.
/// Returns `None` when the flat mask is empty or the input has no noise.
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ or an image is empty.
pub fn noise_gain(
    truth: &LinearImage,
    test: &LinearImage,
    sharpest_input: &LinearImage,
) -> Result<Option<f64>, MetricsError> {
    ensure_same_size(truth, test)?;
    ensure_same_size(truth, sharpest_input)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    let truth_luma = truth.luma();
    let gradient = gradient_magnitude(&truth_luma, truth.width, truth.height);
    let threshold = percentile(&gradient, 0.70);
    let flat: Vec<bool> = gradient.iter().map(|&value| value <= threshold).collect();
    Ok(noise_gain_from_luma(
        &test.luma(),
        &sharpest_input.luma(),
        &flat,
    ))
}

/// Mean CIEDE2000 color error in GT-flat regions, D65.
///
/// # Errors
///
/// Returns [`MetricsError`] when dimensions differ or an image is empty.
pub fn delta_e00_mean(truth: &LinearImage, test: &LinearImage) -> Result<f64, MetricsError> {
    ensure_same_size(truth, test)?;
    if truth.pixel_count() == 0 {
        return Err(MetricsError::Empty);
    }
    let gradient = gradient_magnitude(&truth.luma(), truth.width, truth.height);
    let threshold = percentile(&gradient, 0.70);
    let flat: Vec<bool> = gradient.iter().map(|&value| value <= threshold).collect();
    delta_e00_in_mask(truth, test, &flat)
}

/// CIEDE2000 color difference between two CIELAB colors.
#[must_use]
pub fn delta_e00(lab1: [f64; 3], lab2: [f64; 3]) -> f64 {
    let [l1, a1, b1] = lab1;
    let [l2, a2, b2] = lab2;

    let c1 = a1.hypot(b1);
    let c2 = a2.hypot(b2);
    let c_bar = 0.5 * (c1 + c2);
    let c_bar7 = c_bar.powi(7);
    let g = 0.5 * (1.0 - (c_bar7 / (c_bar7 + 25.0_f64.powi(7))).sqrt());

    let a1_prime = (1.0 + g) * a1;
    let a2_prime = (1.0 + g) * a2;
    let c1_prime = a1_prime.hypot(b1);
    let c2_prime = a2_prime.hypot(b2);
    let h1_prime = hue_angle(b1, a1_prime, c1_prime);
    let h2_prime = hue_angle(b2, a2_prime, c2_prime);

    let delta_l_prime = l2 - l1;
    let delta_c_prime = c2_prime - c1_prime;
    let delta_h_prime = if c1_prime * c2_prime == 0.0 {
        0.0
    } else {
        let difference = h2_prime - h1_prime;
        if difference.abs() <= 180.0 {
            difference
        } else if difference > 180.0 {
            difference - 360.0
        } else {
            difference + 360.0
        }
    };
    let delta_h = 2.0 * (c1_prime * c2_prime).sqrt() * (delta_h_prime.to_radians() / 2.0).sin();

    let l_bar_prime = 0.5 * (l1 + l2);
    let c_bar_prime = 0.5 * (c1_prime + c2_prime);
    let h_bar_prime = if c1_prime * c2_prime == 0.0 {
        h1_prime + h2_prime
    } else {
        let sum = h1_prime + h2_prime;
        if (h1_prime - h2_prime).abs() <= 180.0 {
            0.5 * sum
        } else if sum < 360.0 {
            0.5 * (sum + 360.0)
        } else {
            0.5 * (sum - 360.0)
        }
    };

    let t = 1.0 - 0.17 * (h_bar_prime - 30.0).to_radians().cos()
        + 0.24 * (2.0 * h_bar_prime).to_radians().cos()
        + 0.32 * (3.0 * h_bar_prime + 6.0).to_radians().cos()
        - 0.20 * (4.0 * h_bar_prime - 63.0).to_radians().cos();

    let delta_theta = 30.0 * (-((h_bar_prime - 275.0) / 25.0).powi(2)).exp();
    let c_bar_prime7 = c_bar_prime.powi(7);
    let r_c = 2.0 * (c_bar_prime7 / (c_bar_prime7 + 25.0_f64.powi(7))).sqrt();
    let s_l =
        1.0 + 0.015 * (l_bar_prime - 50.0).powi(2) / (20.0 + (l_bar_prime - 50.0).powi(2)).sqrt();
    let s_c = 1.0 + 0.045 * c_bar_prime;
    let s_h = 1.0 + 0.015 * c_bar_prime * t;
    let r_t = -(2.0 * delta_theta).to_radians().sin() * r_c;

    let l_term = delta_l_prime / s_l;
    let c_term = delta_c_prime / s_c;
    let h_term = delta_h / s_h;

    (l_term * l_term + c_term * c_term + h_term * h_term + r_t * c_term * h_term).sqrt()
}

/// Convert one linear-light sRGB color to CIELAB using the D65 white point.
#[must_use]
pub fn linear_rgb_to_lab(rgb: [f32; 3]) -> [f64; 3] {
    let r = f64::from(rgb[0]);
    let g = f64::from(rgb[1]);
    let b = f64::from(rgb[2]);

    // IEC 61966-2-1 primaries, D65.
    let x = 0.412_456_4 * r + 0.357_576_1 * g + 0.180_437_5 * b;
    let y = 0.212_672_9 * r + 0.715_152_2 * g + 0.072_175_0 * b;
    let z = 0.019_333_9 * r + 0.119_192_0 * g + 0.950_304_1 * b;

    const XN: f64 = 0.950_47;
    const YN: f64 = 1.0;
    const ZN: f64 = 1.088_83;

    let fx = lab_forward(x / XN);
    let fy = lab_forward(y / YN);
    let fz = lab_forward(z / ZN);

    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// Find depth edges where a 4-neighbor depth difference exceeds `threshold`
/// (default 0.5 frame units). Returns one flag per pixel.
#[must_use]
pub fn depth_edges(depth: &[f32], width: u32, height: u32) -> Vec<bool> {
    let mut edges = vec![false; depth.len()];
    if width == 0 || height == 0 {
        return edges;
    }
    let w = width as usize;
    let h = height as usize;
    for y in 0..h {
        for x in 0..w {
            let index = y * w + x;
            let value = depth[index];
            let left = x > 0 && (value - depth[index - 1]).abs() > 0.5;
            let right = x + 1 < w && (value - depth[index + 1]).abs() > 0.5;
            let up = y > 0 && (value - depth[index - w]).abs() > 0.5;
            let down = y + 1 < h && (value - depth[index + w]).abs() > 0.5;
            edges[index] = left || right || up || down;
        }
    }
    edges
}

/// Strong luma edges, used as a depth-edge fallback when no depth map is
/// supplied to `eval`.
#[must_use]
pub fn luma_edges(image: &LinearImage, quantile: f64) -> Vec<bool> {
    let gradient = gradient_magnitude(&image.luma(), image.width, image.height);
    let threshold = percentile(&gradient, quantile);
    gradient.iter().map(|&value| value > threshold).collect()
}

/// Runs `focus-stack eval`.
///
/// # Errors
///
/// Returns [`EvalError`] for bad arguments, unreadable TIFFs, or mismatched
/// dimensions.
pub fn run(args: &EvalArgs) -> Result<(), EvalError> {
    let truth = load_linear(&args.truth)?;
    let test = load_linear(&args.test)?;

    let depth = args
        .depth
        .as_ref()
        .map(|path| load_depth(path, &truth))
        .transpose()?;

    let mut inputs = Vec::with_capacity(args.input.len());
    for path in &args.input {
        inputs.push(load_linear(path)?);
    }
    let sharpest = inputs
        .iter()
        .max_by(|a, b| gradient_energy(a).total_cmp(&gradient_energy(b)));

    let metrics = evaluate(&truth, &test, depth.as_deref(), sharpest)
        .map_err(|error| EvalError::Input(error.to_string()))?;

    if args.json {
        print!("{}", metrics.to_json());
    } else {
        print!("{}", metrics.table());
    }
    Ok(())
}

/// Failure from the `eval` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalError {
    /// Bad arguments.
    Usage(String),
    /// An input file is missing, corrupt, or has the wrong dimensions.
    Input(String),
}

impl EvalError {
    /// Process exit code.
    #[must_use]
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) => 2,
            Self::Input(_) => 3,
        }
    }
}

impl fmt::Display for EvalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) | Self::Input(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for EvalError {}

fn load_linear(path: &std::path::Path) -> Result<LinearImage, EvalError> {
    let frame = read_tiff(path).map_err(|error| EvalError::Input(error.to_string()))?;
    let rgb = frame
        .rgb
        .iter()
        .map(|&value| srgb_decode(f32::from(value) / 65_535.0))
        .collect();
    Ok(LinearImage {
        width: frame.meta.width,
        height: frame.meta.height,
        rgb,
    })
}

fn load_depth(path: &std::path::Path, truth: &LinearImage) -> Result<Vec<f32>, EvalError> {
    let frame = read_tiff(path).map_err(|error| EvalError::Input(error.to_string()))?;
    if frame.meta.width != truth.width || frame.meta.height != truth.height {
        return Err(EvalError::Usage(format!(
            "depth map '{}' is {}x{} but truth is {}x{}",
            path.display(),
            frame.meta.width,
            frame.meta.height,
            truth.width,
            truth.height
        )));
    }
    Ok(frame
        .rgb
        .as_chunks::<3>()
        .0
        .iter()
        .map(|pixel| f32::from(pixel[0]) / 65_535.0)
        .collect())
}

fn ensure_same_size(truth: &LinearImage, other: &LinearImage) -> Result<(), MetricsError> {
    if truth.width != other.width || truth.height != other.height {
        return Err(MetricsError::DimensionMismatch {
            expected: (truth.width, truth.height),
            got: (other.width, other.height),
        });
    }
    Ok(())
}

fn psnr_from_luma(truth: &[f32], test: &[f32]) -> f64 {
    let mse = truth
        .iter()
        .zip(test)
        .map(|(&a, &b)| {
            let difference = f64::from(a) - f64::from(b);
            difference * difference
        })
        .sum::<f64>()
        / truth.len() as f64;
    if mse <= 0.0 {
        f64::INFINITY
    } else {
        10.0 * (1.0 / mse).log10()
    }
}

fn ssim_from_luma(truth: &[f32], test: &[f32], width: u32, height: u32) -> f64 {
    let w = width as usize;
    let h = height as usize;
    let count = w * h;
    let weights: Vec<f64> = (-(SSIM_RADIUS as isize)..=SSIM_RADIUS as isize)
        .map(|offset| {
            let offset = offset as f64;
            (-(offset * offset) / (2.0 * SSIM_SIGMA * SSIM_SIGMA)).exp()
        })
        .collect();

    let truth: Vec<f64> = truth.iter().map(|&value| f64::from(value)).collect();
    let test: Vec<f64> = test.iter().map(|&value| f64::from(value)).collect();

    let mut truth_sq = vec![0.0; count];
    let mut test_sq = vec![0.0; count];
    let mut cross = vec![0.0; count];
    for index in 0..count {
        truth_sq[index] = truth[index] * truth[index];
        test_sq[index] = test[index] * test[index];
        cross[index] = truth[index] * test[index];
    }

    let truth_h = horizontal_blur(&truth, w, h, &weights);
    let test_h = horizontal_blur(&test, w, h, &weights);
    let truth_sq_h = horizontal_blur(&truth_sq, w, h, &weights);
    let test_sq_h = horizontal_blur(&test_sq, w, h, &weights);
    let cross_h = horizontal_blur(&cross, w, h, &weights);
    let weight_h = horizontal_blur(&vec![1.0; count], w, h, &weights);

    let truth_sum = vertical_blur(&truth_h, w, h, &weights);
    let test_sum = vertical_blur(&test_h, w, h, &weights);
    let truth_sq_sum = vertical_blur(&truth_sq_h, w, h, &weights);
    let test_sq_sum = vertical_blur(&test_sq_h, w, h, &weights);
    let cross_sum = vertical_blur(&cross_h, w, h, &weights);
    let weight_sum = vertical_blur(&weight_h, w, h, &weights);

    const C1: f64 = 0.01 * 0.01;
    const C2: f64 = 0.03 * 0.03;

    let mut total = 0.0;
    for index in 0..count {
        let weight = weight_sum[index];
        let mean_truth = truth_sum[index] / weight;
        let mean_test = test_sum[index] / weight;
        let variance_truth = (truth_sq_sum[index] / weight - mean_truth * mean_truth).max(0.0);
        let variance_test = (test_sq_sum[index] / weight - mean_test * mean_test).max(0.0);
        let covariance = cross_sum[index] / weight - mean_truth * mean_test;

        let numerator = (2.0 * mean_truth * mean_test + C1) * (2.0 * covariance + C2);
        let denominator = (mean_truth * mean_truth + mean_test * mean_test + C1)
            * (variance_truth + variance_test + C2);
        total += numerator / denominator;
    }
    total / count as f64
}

fn horizontal_blur(values: &[f64], width: usize, height: usize, weights: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; values.len()];
    let radius = SSIM_RADIUS as isize;
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (index, &weight) in weights.iter().enumerate() {
                let sample = x as isize + index as isize - radius;
                if (0..width as isize).contains(&sample) {
                    sum += weight * values[y * width + sample as usize];
                }
            }
            out[y * width + x] = sum;
        }
    }
    out
}

fn vertical_blur(values: &[f64], width: usize, height: usize, weights: &[f64]) -> Vec<f64> {
    let mut out = vec![0.0; values.len()];
    let radius = SSIM_RADIUS as isize;
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (index, &weight) in weights.iter().enumerate() {
                let sample = y as isize + index as isize - radius;
                if (0..height as isize).contains(&sample) {
                    sum += weight * values[sample as usize * width + x];
                }
            }
            out[y * width + x] = sum;
        }
    }
    out
}

fn gradient_magnitude(luma: &[f32], width: u32, height: u32) -> Vec<f32> {
    let w = width as usize;
    let h = height as usize;
    let mut gradient = vec![0.0f32; luma.len()];
    for y in 0..h {
        let up = y.saturating_sub(1);
        let down = (y + 1).min(h - 1);
        let span_y = (down - up) as f32;
        for x in 0..w {
            let left = x.saturating_sub(1);
            let right = (x + 1).min(w - 1);
            let span_x = (right - left) as f32;
            let gx = if span_x > 0.0 {
                (luma[y * w + right] - luma[y * w + left]) / span_x
            } else {
                0.0
            };
            let gy = if span_y > 0.0 {
                (luma[down * w + x] - luma[up * w + x]) / span_y
            } else {
                0.0
            };
            gradient[y * w + x] = gx.hypot(gy);
        }
    }
    gradient
}

fn percentile(values: &[f32], quantile: f64) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f32::total_cmp);
    let index = ((sorted.len() - 1) as f64 * quantile).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

fn detail_from_gradients(truth: &[f32], test: &[f32], mask: &[bool]) -> f64 {
    let mut truth_sum = 0.0;
    let mut test_sum = 0.0;
    for (index, &selected) in mask.iter().enumerate() {
        if selected {
            truth_sum += f64::from(truth[index]);
            test_sum += f64::from(test[index]);
        }
    }
    if truth_sum <= 1.0e-12 {
        1.0
    } else {
        test_sum / truth_sum
    }
}

fn halo_from_edge_mask(
    truth: &[f32],
    test: &[f32],
    width: u32,
    height: u32,
    edges: &[bool],
) -> f64 {
    let w = width as usize;
    let h = height as usize;
    let band = 5usize;
    // The contrast window is deliberately wider than the band so that every
    // band pixel sees the edge that defines its local contrast.
    let contrast_radius = 2 * band;
    let mut overshoots = Vec::new();

    for y in 0..h {
        for x in 0..w {
            if !near_edge(edges, w, h, x, y, band) {
                continue;
            }
            let mut minimum = f32::INFINITY;
            let mut maximum = f32::NEG_INFINITY;
            for sample_y in y.saturating_sub(contrast_radius)..(y + contrast_radius + 1).min(h) {
                for sample_x in x.saturating_sub(contrast_radius)..(x + contrast_radius + 1).min(w)
                {
                    let value = truth[sample_y * w + sample_x];
                    minimum = minimum.min(value);
                    maximum = maximum.max(value);
                }
            }
            let contrast = (maximum - minimum).max(1.0e-3);
            let index = y * w + x;
            overshoots.push((test[index] - truth[index]).abs() / contrast);
        }
    }

    if overshoots.is_empty() {
        return 0.0;
    }
    overshoots.sort_by(f32::total_cmp);
    let index = ((overshoots.len() - 1) as f64 * 0.99).round() as usize;
    f64::from(overshoots[index.min(overshoots.len() - 1)])
}

fn near_edge(edges: &[bool], width: usize, height: usize, x: usize, y: usize, band: usize) -> bool {
    let x0 = x.saturating_sub(band);
    let y0 = y.saturating_sub(band);
    let x1 = (x + band + 1).min(width);
    let y1 = (y + band + 1).min(height);
    for sample_y in y0..y1 {
        for sample_x in x0..x1 {
            if edges[sample_y * width + sample_x] {
                return true;
            }
        }
    }
    false
}

fn noise_gain_from_luma(test: &[f32], input: &[f32], flat: &[bool]) -> Option<f64> {
    let mut count = 0usize;
    let mut test_sum = 0.0;
    let mut input_sum = 0.0;
    for (index, &selected) in flat.iter().enumerate() {
        if selected {
            count += 1;
            test_sum += f64::from(test[index]);
            input_sum += f64::from(input[index]);
        }
    }
    if count == 0 {
        return None;
    }
    let test_mean = test_sum / count as f64;
    let input_mean = input_sum / count as f64;
    let mut test_variance = 0.0;
    let mut input_variance = 0.0;
    for (index, &selected) in flat.iter().enumerate() {
        if selected {
            let test_delta = f64::from(test[index]) - test_mean;
            let input_delta = f64::from(input[index]) - input_mean;
            test_variance += test_delta * test_delta;
            input_variance += input_delta * input_delta;
        }
    }
    let test_std = (test_variance / count as f64).sqrt();
    let input_std = (input_variance / count as f64).sqrt();
    if input_std <= 1.0e-12 {
        return None;
    }
    Some(test_std / input_std)
}

fn delta_e00_in_mask(
    truth: &LinearImage,
    test: &LinearImage,
    mask: &[bool],
) -> Result<f64, MetricsError> {
    let mut total = 0.0;
    let mut count = 0usize;
    for (index, &selected) in mask.iter().enumerate() {
        if selected {
            let truth_rgb = &truth.rgb[index * 3..index * 3 + 3];
            let test_rgb = &test.rgb[index * 3..index * 3 + 3];
            let truth_lab = linear_rgb_to_lab([truth_rgb[0], truth_rgb[1], truth_rgb[2]]);
            let test_lab = linear_rgb_to_lab([test_rgb[0], test_rgb[1], test_rgb[2]]);
            total += delta_e00(truth_lab, test_lab);
            count += 1;
        }
    }
    if count == 0 {
        return Ok(0.0);
    }
    Ok(total / count as f64)
}

fn json_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        "null".to_string()
    }
}

fn hue_angle(b: f64, a: f64, chroma: f64) -> f64 {
    if chroma == 0.0 {
        return 0.0;
    }
    let angle = b.atan2(a).to_degrees();
    if angle < 0.0 { angle + 360.0 } else { angle }
}

fn lab_forward(value: f64) -> f64 {
    const DELTA: f64 = 6.0 / 29.0;
    const DELTA_CUBED: f64 = DELTA * DELTA * DELTA;
    if value > DELTA_CUBED {
        value.cbrt()
    } else {
        value / (3.0 * DELTA * DELTA) + 4.0 / 29.0
    }
}

fn gradient_energy(image: &LinearImage) -> f64 {
    gradient_magnitude(&image.luma(), image.width, image.height)
        .iter()
        .map(|&value| f64::from(value))
        .sum()
}
