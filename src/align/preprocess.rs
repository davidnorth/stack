//! Masks, downscaling, and high-pass preprocessing.
//!
//! Alignment works on encoded luma at two capped resolutions
//! (`docs/alignment.md` §2). The full-resolution luma and a colour-validity
//! mask are the only full-resolution buffers; every level is built from those
//! so peak memory is bounded by the largest requested long edge.

use crate::color::luma;

/// Default alignment long edges, coarse to fine.
pub const DEFAULT_LONG_EDGES: [u32; 2] = [1024, 2048];

/// Preprocessing parameters from `docs/alignment.md` §2.
#[derive(Debug, Clone, PartialEq)]
pub struct PreprocessParams {
    /// Requested long edges. Duplicate sizes (including targets larger than
    /// the source) are collapsed.
    pub long_edges: [u32; 2],
    /// High-pass Gaussian sigma.
    pub high_pass_sigma: f32,
    /// Gradient Gaussian sigma.
    pub gradient_sigma: f32,
    /// Gradient percentile used as the flat-region threshold, in `0..=1`.
    pub gradient_quantile: f32,
    /// Pixels with `max(R, G, B)` at or above this value are saturated.
    pub saturation_max: f32,
    /// Pixels with encoded luma at or below this value are crushed shadows.
    pub luma_min: f32,
}

impl Default for PreprocessParams {
    fn default() -> Self {
        Self {
            long_edges: DEFAULT_LONG_EDGES,
            high_pass_sigma: 3.0,
            gradient_sigma: 1.0,
            gradient_quantile: 0.20,
            saturation_max: 0.99,
            luma_min: 0.01,
        }
    }
}

/// One alignment resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignmentLevel {
    pub width: u32,
    pub height: u32,
    /// Downsampled encoded luma.
    pub luma: Vec<f32>,
    /// `H = G - Gaussian(G, high_pass_sigma)`.
    pub high_pass: Vec<f32>,
    /// Gradient magnitude after `gradient_sigma` smoothing.
    pub gradient: Vec<f32>,
    /// Alignment weight: one where the gradient threshold, saturation, and
    /// shadow tests all pass.
    pub mask: Vec<f32>,
    /// High-passed luma multiplied by a Hann window, ready for FFTs.
    pub windowed: Vec<f32>,
}

impl AlignmentLevel {
    /// Number of pixels in the level.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.width * self.height) as usize
    }

    /// Whether the level has no pixels.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Build alignment levels from full-resolution encoded RGB.
///
/// `rgb` is interleaved and expected in encoded (display-referred) light, as
/// produced by the colour pipeline. Levels are returned in the requested
/// order with inaccessible targets skipped.
///
/// # Panics
///
/// Panics if `width` or `height` is zero, or if `rgb` is not
/// `width * height * 3` samples long.
#[must_use]
pub fn preprocess_rgb(
    rgb: &[f32],
    width: u32,
    height: u32,
    params: &PreprocessParams,
) -> Vec<AlignmentLevel> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    assert_eq!(
        rgb.len(),
        (width as usize) * (height as usize) * 3,
        "RGB buffer length mismatch"
    );

    let pixels = (width as usize) * (height as usize);
    let mut full_luma = vec![0.0; pixels];
    luma::encoded_luma_buffer(rgb, &mut full_luma);

    let mut full_valid = vec![1u8; pixels];
    for (pixel, valid) in rgb.as_chunks::<3>().0.iter().zip(&mut full_valid) {
        let max = pixel[0].max(pixel[1]).max(pixel[2]);
        let y = luma::encoded_luma(pixel[0], pixel[1], pixel[2]);
        *valid = u8::from(max < params.saturation_max && y > params.luma_min);
    }

    let mut levels = Vec::with_capacity(params.long_edges.len());
    let mut previous = None;
    for &target in &params.long_edges {
        let (level_w, level_h) = target_dims(width, height, target);
        if previous == Some((level_w, level_h)) {
            continue;
        }
        previous = Some((level_w, level_h));

        let (level_luma, level_valid) =
            downsample(&full_luma, &full_valid, width, height, level_w, level_h);
        levels.push(build_level(
            &level_luma,
            &level_valid,
            level_w,
            level_h,
            params,
        ));
    }
    levels
}

/// Symmetric Hann window of `width * height` samples.
///
/// A dimension of one maps to a constant window of one.
#[must_use]
pub fn hann_window(width: u32, height: u32) -> Vec<f32> {
    let mut window = Vec::with_capacity((width * height) as usize);
    for y in 0..height {
        let wy = axis_window(y, height);
        for x in 0..width {
            window.push(wy * axis_window(x, width));
        }
    }
    window
}

fn axis_window(index: u32, len: u32) -> f32 {
    if len <= 1 {
        return 1.0;
    }
    let phase = std::f32::consts::TAU * index as f32 / (len - 1) as f32;
    0.5 - 0.5 * phase.cos()
}

fn target_dims(width: u32, height: u32, target_long_edge: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= target_long_edge {
        return (width, height);
    }
    let scale = target_long_edge as f64 / long as f64;
    if width >= height {
        let w = target_long_edge.max(1);
        let h = ((height as f64 * scale).round() as u32).max(1);
        (w, h)
    } else {
        let w = ((width as f64 * scale).round() as u32).max(1);
        let h = target_long_edge.max(1);
        (w, h)
    }
}

/// Area-average luma and conservatively combine validity: a destination pixel
/// is valid only when every contributing source pixel was valid.
fn downsample(
    luma: &[f32],
    valid: &[u8],
    src_w: u32,
    src_h: u32,
    dst_w: u32,
    dst_h: u32,
) -> (Vec<f32>, Vec<u8>) {
    if src_w == dst_w && src_h == dst_h {
        return (luma.to_vec(), valid.to_vec());
    }

    let mut out_luma = vec![0.0; (dst_w * dst_h) as usize];
    let mut out_valid = vec![1u8; (dst_w * dst_h) as usize];
    let x_scale = src_w as f64 / dst_w as f64;
    let y_scale = src_h as f64 / dst_h as f64;

    for dy in 0..dst_h {
        let y0 = dy as f64 * y_scale;
        let y1 = (dy + 1) as f64 * y_scale;
        let iy0 = y0.floor() as u32;
        let iy1 = (y1.ceil() as u32).min(src_h);
        for dx in 0..dst_w {
            let x0 = dx as f64 * x_scale;
            let x1 = (dx + 1) as f64 * x_scale;
            let ix0 = x0.floor() as u32;
            let ix1 = (x1.ceil() as u32).min(src_w);

            let mut sum = 0.0f64;
            let mut weight_sum = 0.0f64;
            let mut all_valid = true;
            for sy in iy0..iy1 {
                let wy = y1.min((sy + 1) as f64) - y0.max(sy as f64);
                if wy <= 0.0 {
                    continue;
                }
                for sx in ix0..ix1 {
                    let wx = x1.min((sx + 1) as f64) - x0.max(sx as f64);
                    if wx <= 0.0 {
                        continue;
                    }
                    let weight = wx * wy;
                    let index = (sy * src_w + sx) as usize;
                    sum += f64::from(luma[index]) * weight;
                    weight_sum += weight;
                    all_valid &= valid[index] != 0;
                }
            }

            let index = (dy * dst_w + dx) as usize;
            out_luma[index] = if weight_sum > 0.0 {
                (sum / weight_sum) as f32
            } else {
                0.0
            };
            out_valid[index] = u8::from(all_valid);
        }
    }

    (out_luma, out_valid)
}

fn build_level(
    luma: &[f32],
    valid: &[u8],
    width: u32,
    height: u32,
    params: &PreprocessParams,
) -> AlignmentLevel {
    let high_pass = subtract_blur(luma, width, height, params.high_pass_sigma);
    let smoothed = gaussian_blur(luma, width, height, params.gradient_sigma);
    let gradient = gradient_magnitude(&smoothed, width, height);
    let threshold = percentile(&gradient, params.gradient_quantile);

    let mask = gradient
        .iter()
        .zip(valid)
        .map(|(&value, &valid)| {
            u8::from(valid != 0 && value.is_finite() && value > threshold) as f32
        })
        .collect();

    let hann = hann_window(width, height);
    let windowed = high_pass
        .iter()
        .zip(&hann)
        .map(|(&value, &window)| value * window)
        .collect();

    AlignmentLevel {
        width,
        height,
        luma: luma.to_vec(),
        high_pass,
        gradient,
        mask,
        windowed,
    }
}

fn subtract_blur(src: &[f32], width: u32, height: u32, sigma: f32) -> Vec<f32> {
    let blurred = gaussian_blur(src, width, height, sigma);
    src.iter().zip(blurred).map(|(&a, b)| a - b).collect()
}

fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (3.0 * sigma).ceil().max(1.0) as i32;
    let denominator = 2.0 * sigma * sigma;
    let mut kernel = Vec::with_capacity((2 * radius + 1) as usize);
    let mut sum = 0.0;
    for offset in -radius..=radius {
        let value = (-(offset * offset) as f32 / denominator).exp();
        kernel.push(value);
        sum += value;
    }
    for value in &mut kernel {
        *value /= sum;
    }
    kernel
}

fn gaussian_blur(src: &[f32], width: u32, height: u32, sigma: f32) -> Vec<f32> {
    if sigma.is_nan() || sigma <= 0.0 {
        return src.to_vec();
    }

    let kernel = gaussian_kernel(sigma);
    let radius = (kernel.len() / 2) as i32;
    let max_x = width as i32 - 1;
    let max_y = height as i32 - 1;

    let mut horizontal = vec![0.0; src.len()];
    for y in 0..height {
        let row = (y * width) as usize;
        for x in 0..width {
            let mut sum = 0.0;
            for (tap, &weight) in kernel.iter().enumerate() {
                let sx = (x as i32 + tap as i32 - radius).clamp(0, max_x) as usize;
                sum += src[row + sx] * weight;
            }
            horizontal[row + x as usize] = sum;
        }
    }

    let mut out = vec![0.0; src.len()];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0;
            for (tap, &weight) in kernel.iter().enumerate() {
                let sy = (y as i32 + tap as i32 - radius).clamp(0, max_y) as usize;
                sum += horizontal[sy * width as usize + x as usize] * weight;
            }
            out[(y * width + x) as usize] = sum;
        }
    }
    out
}

fn gradient_magnitude(src: &[f32], width: u32, height: u32) -> Vec<f32> {
    let mut out = vec![0.0; src.len()];
    let width_usize = width as usize;
    let max_x = width as i32 - 1;
    let max_y = height as i32 - 1;
    for y in 0..height as usize {
        let ym = (y as i32 - 1).clamp(0, max_y) as usize;
        let yp = (y as i32 + 1).clamp(0, max_y) as usize;
        for x in 0..width_usize {
            let xm = (x as i32 - 1).clamp(0, max_x) as usize;
            let xp = (x as i32 + 1).clamp(0, max_x) as usize;
            let sample = |sx: usize, sy: usize| src[sy * width_usize + sx];

            let gx = (sample(xp, ym) + 2.0 * sample(xp, y) + sample(xp, yp))
                - (sample(xm, ym) + 2.0 * sample(xm, y) + sample(xm, yp));
            let gy = (sample(xm, yp) + 2.0 * sample(x, yp) + sample(xp, yp))
                - (sample(xm, ym) + 2.0 * sample(x, ym) + sample(xp, ym));
            out[y * width_usize + x] = (gx * gx + gy * gy).sqrt() / 8.0;
        }
    }
    out
}

/// Histogram percentile, avoiding a sorted copy of the gradient buffer.
fn percentile(values: &[f32], quantile: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut min = f32::INFINITY;
    let mut max = f32::NEG_INFINITY;
    for &value in values {
        min = min.min(value);
        max = max.max(value);
    }
    if max <= min {
        return min;
    }

    const BINS: usize = 4096;
    let scale = (BINS as f32 - 1.0) / (max - min);
    let mut histogram = vec![0u32; BINS];
    for &value in values {
        let bin = ((value - min) * scale).clamp(0.0, (BINS - 1) as f32) as usize;
        histogram[bin] += 1;
    }

    let target = ((values.len() as f64) * f64::from(quantile.clamp(0.0, 1.0))).ceil();
    let target = (target as usize).max(1);
    let mut cumulative = 0usize;
    for (bin, &count) in histogram.iter().enumerate() {
        cumulative += count as usize;
        if cumulative >= target {
            let upper = min + (bin as f32 + 1.0) / scale;
            return upper.min(max);
        }
    }
    max
}
