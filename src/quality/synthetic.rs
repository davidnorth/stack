//! Ground-truth stack generator.
//!
//! Implements `docs/quality.md` §2. Depth is quantized into layers, each layer
//! is blurred with its analytic circle-of-confusion radius, the layers are
//! composited front-to-back with feathered masks, and noise, exposure gain, and
//! optional known transforms are applied. All randomness comes from `seed`.
//!
//! Both PSF choices are implemented: a discrete disk (the default) and a
//! Gaussian with `sigma = c / 1.5` for fast fixtures. Disk radii are
//! interpolated linearly between the two neighbouring integer kernels, so a
//! radius may be fractional.

use crate::Similarity;
use serde::Deserialize;
use std::error::Error;
use std::fmt;
use std::fs;
use std::path::Path;

const DEFAULT_LAYERS: usize = 16;
const DEFAULT_C_MAX: f64 = 16.0;

/// Point-spread function used to defocus each depth layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Psf {
    /// Discrete disk of radius `c` (FFT-equivalent convolution).
    #[default]
    Disk,
    /// Gaussian with `sigma = c / 1.5`.
    Gaussian,
}

fn default_layers() -> usize {
    DEFAULT_LAYERS
}

fn default_c_max() -> f64 {
    DEFAULT_C_MAX
}

fn default_one() -> f64 {
    1.0
}

/// Optional known transform in a generator config.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransformConfig {
    #[serde(default = "default_one")]
    pub a: f64,
    #[serde(default)]
    pub b: f64,
    #[serde(default)]
    pub tx: f64,
    #[serde(default)]
    pub ty: f64,
}

impl From<&TransformConfig> for Similarity {
    fn from(value: &TransformConfig) -> Self {
        Self {
            a: value.a,
            b: value.b,
            tx: value.tx,
            ty: value.ty,
        }
    }
}

/// Synthetic stack generator configuration, matching `docs/quality.md` §2.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyntheticConfig {
    pub width: u32,
    pub height: u32,
    pub frames: usize,
    pub seed: u64,
    pub coc_scale: f64,
    pub focus: Vec<f64>,
    #[serde(default)]
    pub noise_read: f64,
    /// Shot-noise scale: the output standard deviation at full scale is
    /// `noise_shot`, and `sigma(value) = noise_shot * sqrt(value)`.
    #[serde(default)]
    pub noise_shot: f64,
    #[serde(default)]
    pub exposure: Vec<f64>,
    #[serde(default = "default_layers")]
    pub layers: usize,
    #[serde(default)]
    pub psf: Psf,
    #[serde(default = "default_c_max")]
    pub c_max: f64,
    #[serde(default)]
    pub transforms: Vec<TransformConfig>,
}

impl SyntheticConfig {
    /// Parse and validate a TOML generator config.
    pub fn from_toml_str(text: &str) -> Result<Self, SyntheticError> {
        let config: Self =
            toml::from_str(text).map_err(|error| SyntheticError::Parse(error.to_string()))?;
        config.validate()?;
        Ok(config)
    }

    /// Read and validate a TOML generator config from disk.
    pub fn from_toml_file(path: &Path) -> Result<Self, SyntheticError> {
        let text = fs::read_to_string(path)
            .map_err(|error| SyntheticError::Io(format!("{}: {error}", path.display())))?;
        Self::from_toml_str(&text)
    }

    /// Check the config for internal consistency.
    pub fn validate(&self) -> Result<(), SyntheticError> {
        if self.width == 0 || self.height == 0 {
            return Err(SyntheticError::Invalid(
                "width and height must be positive".into(),
            ));
        }
        if self.frames == 0 {
            return Err(SyntheticError::Invalid("frames must be positive".into()));
        }
        if self.focus.len() != self.frames {
            return Err(SyntheticError::Invalid(format!(
                "focus has {} entries but frames is {}",
                self.focus.len(),
                self.frames
            )));
        }
        if !self.exposure.is_empty() && self.exposure.len() != self.frames {
            return Err(SyntheticError::Invalid(format!(
                "exposure has {} entries but frames is {}",
                self.exposure.len(),
                self.frames
            )));
        }
        if !self.transforms.is_empty() && self.transforms.len() != self.frames {
            return Err(SyntheticError::Invalid(format!(
                "transforms has {} entries but frames is {}",
                self.transforms.len(),
                self.frames
            )));
        }
        if self.layers < 2 {
            return Err(SyntheticError::Invalid("layers must be at least 2".into()));
        }
        if !self.coc_scale.is_finite() || self.coc_scale < 0.0 {
            return Err(SyntheticError::Invalid(
                "coc_scale must be finite and non-negative".into(),
            ));
        }
        if !self.c_max.is_finite() || self.c_max < 0.0 {
            return Err(SyntheticError::Invalid(
                "c_max must be finite and non-negative".into(),
            ));
        }
        if !self.noise_read.is_finite() || self.noise_read < 0.0 {
            return Err(SyntheticError::Invalid(
                "noise_read must be finite and non-negative".into(),
            ));
        }
        if !self.noise_shot.is_finite() || self.noise_shot < 0.0 {
            return Err(SyntheticError::Invalid(
                "noise_shot must be finite and non-negative".into(),
            ));
        }
        if self.focus.iter().any(|z| !z.is_finite() || *z <= 0.0) {
            return Err(SyntheticError::Invalid(
                "focus distances must be finite and positive".into(),
            ));
        }
        if self.exposure.iter().any(|g| !g.is_finite() || *g < 0.0) {
            return Err(SyntheticError::Invalid(
                "exposure gains must be finite and non-negative".into(),
            ));
        }
        Ok(())
    }

    fn exposure_gains(&self) -> Vec<f64> {
        if self.exposure.is_empty() {
            vec![1.0; self.frames]
        } else {
            self.exposure.clone()
        }
    }

    fn similarity_transforms(&self) -> Vec<Similarity> {
        if self.transforms.is_empty() {
            vec![
                Similarity {
                    a: 1.0,
                    b: 0.0,
                    tx: 0.0,
                    ty: 0.0,
                };
                self.frames
            ]
        } else {
            self.transforms.iter().map(Similarity::from).collect()
        }
    }
}

/// Errors produced while loading or running the generator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyntheticError {
    Parse(String),
    Invalid(String),
    Io(String),
}

impl fmt::Display for SyntheticError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(message) => write!(f, "invalid synthetic config: {message}"),
            Self::Invalid(message) => write!(f, "invalid synthetic config: {message}"),
            Self::Io(message) => write!(f, "cannot read synthetic config: {message}"),
        }
    }
}

impl Error for SyntheticError {}

/// One generated stack plus the ground truth used to create it.
#[derive(Debug, Clone)]
pub struct SyntheticStack {
    pub config: SyntheticConfig,
    /// All-in-focus linear RGB scene, interleaved and in `[0, 1]`.
    pub scene: Vec<f32>,
    /// Depth map, row-major. Smaller values are nearer to the camera.
    pub depth: Vec<f32>,
    /// Defocused frames, interleaved 16-bit RGB.
    pub frames: Vec<Vec<u16>>,
    /// Known output-to-source transforms per frame.
    pub transforms: Vec<Similarity>,
    /// Per-frame exposure gains.
    pub exposure: Vec<f64>,
    /// Representative depth of each quantized layer.
    pub layer_depths: Vec<f64>,
    /// Analytic disk radius used for each frame and layer.
    pub layer_blur_radii: Vec<Vec<f64>>,
}

impl SyntheticStack {
    /// Generate a stack with a procedural scene and depth map.
    pub fn generate(config: &SyntheticConfig) -> Result<Self, SyntheticError> {
        config.validate()?;
        let (scene, depth) = procedural_scene(config);
        Self::generate_from_scene(&scene, &depth, config)
    }

    /// Generate a stack from a supplied linear scene and depth map.
    pub fn generate_from_scene(
        scene: &[f32],
        depth: &[f32],
        config: &SyntheticConfig,
    ) -> Result<Self, SyntheticError> {
        config.validate()?;
        let width = config.width as usize;
        let height = config.height as usize;
        let pixels = width
            .checked_mul(height)
            .ok_or_else(|| SyntheticError::Invalid("image dimensions overflow".into()))?;
        if scene.len() != pixels * 3 {
            return Err(SyntheticError::Invalid(format!(
                "scene has {} samples, expected {}",
                scene.len(),
                pixels * 3
            )));
        }
        if depth.len() != pixels {
            return Err(SyntheticError::Invalid(format!(
                "depth has {} samples, expected {pixels}",
                depth.len()
            )));
        }
        if scene.iter().any(|v| !v.is_finite()) {
            return Err(SyntheticError::Invalid(
                "scene contains non-finite values".into(),
            ));
        }
        if depth.iter().any(|z| !z.is_finite() || *z <= 0.0) {
            return Err(SyntheticError::Invalid(
                "depth values must be finite and positive".into(),
            ));
        }

        let (layers, layer_depths) = build_layers(scene, depth, width, config)?;
        let exposure = config.exposure_gains();
        let transforms = config.similarity_transforms();
        let mut frames = Vec::with_capacity(config.frames);
        let mut layer_blur_radii = vec![vec![0.0; layer_depths.len()]; config.frames];

        for (frame_index, &focus) in config.focus.iter().enumerate() {
            let radii = &mut layer_blur_radii[frame_index];
            let mut linear = composite_frame(
                &layers,
                config,
                focus,
                exposure[frame_index],
                config.seed.wrapping_add(frame_index as u64),
                radii,
            );
            if !is_identity(&transforms[frame_index]) {
                linear = warp_linear(&linear, width, height, &transforms[frame_index]);
            }
            let frame = linear
                .into_iter()
                .map(|v| quantize_16(v.clamp(0.0, 1.0) as f64))
                .collect();
            frames.push(frame);
        }

        Ok(Self {
            config: config.clone(),
            scene: scene.to_vec(),
            depth: depth.to_vec(),
            frames,
            transforms,
            exposure,
            layer_depths,
            layer_blur_radii,
        })
    }

    /// Number of pixels per plane.
    pub fn pixel_count(&self) -> usize {
        self.config.width as usize * self.config.height as usize
    }

    /// A frame converted back to linear `[0, 1]` floats.
    pub fn frame_linear(&self, frame: usize) -> Vec<f32> {
        self.frames[frame]
            .iter()
            .map(|&v| v as f32 / 65535.0)
            .collect()
    }
}

/// Analytic circle-of-confusion radius from `docs/quality.md` §2.
pub fn blur_radius(coc_scale: f64, c_max: f64, depth: f64, focus: f64) -> f64 {
    if depth <= 0.0 || focus <= 0.0 {
        return c_max;
    }
    (coc_scale * (1.0 / depth - 1.0 / focus).abs()).clamp(0.0, c_max)
}

// --- Layer construction -------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bounds {
    x0: usize,
    y0: usize,
    x1: usize,
    y1: usize,
}

impl Bounds {
    fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Self {
        Self { x0, y0, x1, y1 }
    }

    fn width(self) -> usize {
        self.x1 - self.x0
    }

    fn height(self) -> usize {
        self.y1 - self.y0
    }

    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }

    fn expand(self, amount: usize, width: usize, height: usize) -> Self {
        Self::new(
            self.x0.saturating_sub(amount),
            self.y0.saturating_sub(amount),
            (self.x1 + amount).min(width),
            (self.y1 + amount).min(height),
        )
    }

    fn union(self, other: Self) -> Self {
        Self::new(
            self.x0.min(other.x0),
            self.y0.min(other.y0),
            self.x1.max(other.x1),
            self.y1.max(other.y1),
        )
    }
}

/// A depth layer cropped to its non-zero bounding box.
#[derive(Debug, Clone)]
struct Layer {
    bounds: Bounds,
    depth: f64,
    /// Interleaved RGB, length `bounds.width() * bounds.height() * 3`.
    color: Vec<f32>,
    /// Feathered layer mask, length `bounds.width() * bounds.height()`.
    mask: Vec<f32>,
}

impl Layer {
    fn get(&self, x: usize, y: usize) -> [f32; 4] {
        if x < self.bounds.x0 || x >= self.bounds.x1 || y < self.bounds.y0 || y >= self.bounds.y1 {
            return [0.0; 4];
        }
        let index = (y - self.bounds.y0) * self.bounds.width() + (x - self.bounds.x0);
        let base = index * 3;
        [
            self.color[base],
            self.color[base + 1],
            self.color[base + 2],
            self.mask[index],
        ]
    }

    fn get_clamped(&self, x: isize, y: isize, width: usize, height: usize) -> [f32; 4] {
        let cx = x.clamp(0, width as isize - 1) as usize;
        let cy = y.clamp(0, height as isize - 1) as usize;
        self.get(cx, cy)
    }
}

fn build_layers(
    scene: &[f32],
    depth: &[f32],
    width: usize,
    config: &SyntheticConfig,
) -> Result<(Vec<Layer>, Vec<f64>), SyntheticError> {
    let layer_count = config.layers;
    let z_min = depth
        .iter()
        .copied()
        .fold(f64::INFINITY, |a, b| a.min(b as f64));
    let z_max = depth
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, |a, b| a.max(b as f64));
    let span = z_max - z_min;
    let layer_depths: Vec<f64> = (0..layer_count)
        .map(|index| {
            if span <= f64::EPSILON {
                z_min
            } else {
                z_min + span * index as f64 / (layer_count - 1) as f64
            }
        })
        .collect();

    let mut bounds: Vec<Option<Bounds>> = vec![None; layer_count];
    for (index, z) in depth.iter().copied().enumerate() {
        let (low, high, t) = layer_weights(z as f64, z_min, span, layer_count);
        let x = index % width;
        let y = index / width;
        bounds[low] = Some(match bounds[low] {
            Some(b) => b.union(Bounds::new(x, y, x + 1, y + 1)),
            None => Bounds::new(x, y, x + 1, y + 1),
        });
        if t > 0.0 && high != low {
            bounds[high] = Some(match bounds[high] {
                Some(b) => b.union(Bounds::new(x, y, x + 1, y + 1)),
                None => Bounds::new(x, y, x + 1, y + 1),
            });
        }
    }

    let mut layers: Vec<Layer> = bounds
        .into_iter()
        .enumerate()
        .map(|(index, b)| {
            let bounds = b.unwrap_or_else(|| Bounds::new(0, 0, 0, 0));
            Layer {
                bounds,
                depth: layer_depths[index],
                color: vec![0.0; bounds.width() * bounds.height() * 3],
                mask: vec![0.0; bounds.width() * bounds.height()],
            }
        })
        .collect();

    for (index, z) in depth.iter().copied().enumerate() {
        let (low, high, t) = layer_weights(z as f64, z_min, span, layer_count);
        let x = index % width;
        let y = index / width;
        let base = index * 3;
        add_sample(&mut layers[low], x, y, &scene[base..base + 3], 1.0 - t);
        if t > 0.0 && high != low {
            add_sample(&mut layers[high], x, y, &scene[base..base + 3], t);
        }
    }

    Ok((layers, layer_depths))
}

fn layer_weights(z: f64, z_min: f64, span: f64, layer_count: usize) -> (usize, usize, f64) {
    if span <= f64::EPSILON {
        return (0, 0, 0.0);
    }
    let position =
        ((z - z_min) / span * (layer_count - 1) as f64).clamp(0.0, (layer_count - 1) as f64);
    let low = position.floor() as usize;
    let high = (low + 1).min(layer_count - 1);
    (low, high, position - low as f64)
}

fn add_sample(layer: &mut Layer, x: usize, y: usize, rgb: &[f32], weight: f64) {
    if weight <= 0.0 || layer.bounds.is_empty() {
        return;
    }
    let index = (y - layer.bounds.y0) * layer.bounds.width() + (x - layer.bounds.x0);
    let base = index * 3;
    let weight = weight as f32;
    layer.color[base] += rgb[0] * weight;
    layer.color[base + 1] += rgb[1] * weight;
    layer.color[base + 2] += rgb[2] * weight;
    layer.mask[index] += weight;
}

// --- Blurring -----------------------------------------------------------

#[derive(Debug, Clone)]
struct Buffer4 {
    bounds: Bounds,
    data: Vec<[f32; 4]>,
}

impl Buffer4 {
    fn new(bounds: Bounds) -> Self {
        Self {
            bounds,
            data: vec![[0.0; 4]; bounds.width() * bounds.height()],
        }
    }

    fn get(&self, x: usize, y: usize) -> [f32; 4] {
        if x < self.bounds.x0 || x >= self.bounds.x1 || y < self.bounds.y0 || y >= self.bounds.y1 {
            return [0.0; 4];
        }
        self.data[(y - self.bounds.y0) * self.bounds.width() + (x - self.bounds.x0)]
    }

    fn get_clamped(&self, x: isize, y: isize, width: usize, height: usize) -> [f32; 4] {
        let cx = x.clamp(0, width as isize - 1) as usize;
        let cy = y.clamp(0, height as isize - 1) as usize;
        self.get(cx, cy)
    }
}

fn blur_layer(
    layer: &Layer,
    width: usize,
    height: usize,
    radius: f64,
    psf: Psf,
) -> Option<Buffer4> {
    if layer.bounds.is_empty() {
        return None;
    }
    match psf {
        Psf::Disk => Some(blur_disk(layer, width, height, radius)),
        Psf::Gaussian => Some(blur_gaussian(layer, width, height, radius / 1.5)),
    }
}

fn blur_disk(layer: &Layer, width: usize, height: usize, radius: f64) -> Buffer4 {
    let floor = radius.floor() as usize;
    let fraction = radius - floor as f64;
    let out_bounds = layer.bounds.expand(radius.ceil() as usize, width, height);
    if fraction <= 1.0e-9 {
        return disk_integer(layer, width, height, floor, out_bounds);
    }
    let lower = disk_integer(layer, width, height, floor, out_bounds);
    let upper = disk_integer(layer, width, height, floor + 1, out_bounds);
    let mut blended = Buffer4::new(out_bounds);
    for (index, value) in blended.data.iter_mut().enumerate() {
        let a = lower.data[index];
        let b = upper.data[index];
        for channel in 0..4 {
            value[channel] =
                (a[channel] as f64 * (1.0 - fraction) + b[channel] as f64 * fraction) as f32;
        }
    }
    blended
}

fn disk_widths(radius: usize) -> Vec<usize> {
    let r = radius as isize;
    (-r..=r)
        .map(|dy| {
            let remaining = radius * radius - (dy * dy) as usize;
            if remaining == 0 {
                0
            } else {
                (remaining as f64).sqrt().floor() as usize
            }
        })
        .collect()
}

fn disk_integer(
    layer: &Layer,
    width: usize,
    height: usize,
    radius: usize,
    out_bounds: Bounds,
) -> Buffer4 {
    let mut output = Buffer4::new(out_bounds);
    if out_bounds.is_empty() {
        return output;
    }
    let out_width = out_bounds.width();
    let widths = disk_widths(radius);
    let norm: f64 = widths.iter().map(|w| (2 * w + 1) as f64).sum();
    let mut prefix = vec![[0.0f64; 4]; width + 1];
    let mut accum = vec![[0.0f64; 4]; out_width];
    let radius = radius as isize;

    for (row, y) in (out_bounds.y0..out_bounds.y1).enumerate() {
        accum.fill([0.0; 4]);
        for (offset, &half_width) in widths.iter().enumerate() {
            let dy = offset as isize - radius;
            let source_y = (y as isize + dy).clamp(0, height as isize - 1) as usize;
            prefix[0] = [0.0; 4];
            let mut running = [0.0f64; 4];
            for x in 0..width {
                let sample = layer.get(x, source_y);
                for channel in 0..4 {
                    running[channel] += sample[channel] as f64;
                }
                prefix[x + 1] = running;
            }
            let half_width = half_width as isize;
            for (column, x) in (out_bounds.x0..out_bounds.x1).enumerate() {
                let mut lo = x as isize - half_width;
                let mut hi = x as isize + half_width;
                let mut sum = [0.0f64; 4];
                if lo < 0 {
                    let repeats = (-lo) as usize;
                    let edge = layer.get(0, source_y);
                    for channel in 0..4 {
                        sum[channel] += edge[channel] as f64 * repeats as f64;
                    }
                    lo = 0;
                }
                if hi >= width as isize {
                    let repeats = (hi - (width as isize - 1)) as usize;
                    let edge = layer.get(width - 1, source_y);
                    for channel in 0..4 {
                        sum[channel] += edge[channel] as f64 * repeats as f64;
                    }
                    hi = width as isize - 1;
                }
                let lo = lo as usize;
                let hi = hi as usize;
                let start = prefix[lo];
                let end = prefix[hi + 1];
                for channel in 0..4 {
                    sum[channel] += end[channel] - start[channel];
                }
                for channel in 0..4 {
                    accum[column][channel] += sum[channel];
                }
            }
        }
        let row_start = row * out_width;
        for (column, value) in accum.iter().enumerate() {
            output.data[row_start + column] = [
                (value[0] / norm) as f32,
                (value[1] / norm) as f32,
                (value[2] / norm) as f32,
                (value[3] / norm) as f32,
            ];
        }
    }
    output
}

fn gaussian_weights(sigma: f64) -> Vec<f64> {
    let radius = (sigma * 3.0).ceil().max(1.0) as usize;
    let mut weights = Vec::with_capacity(radius * 2 + 1);
    for offset in -(radius as isize)..=(radius as isize) {
        let distance = offset as f64;
        weights.push((-distance * distance / (2.0 * sigma * sigma)).exp());
    }
    let total: f64 = weights.iter().sum();
    weights.iter().map(|w| w / total).collect()
}

fn blur_gaussian(layer: &Layer, width: usize, height: usize, sigma: f64) -> Buffer4 {
    if sigma <= 0.05 {
        let mut output = Buffer4::new(layer.bounds);
        for y in layer.bounds.y0..layer.bounds.y1 {
            for x in layer.bounds.x0..layer.bounds.x1 {
                let index = (y - layer.bounds.y0) * layer.bounds.width() + (x - layer.bounds.x0);
                output.data[index] = layer.get(x, y);
            }
        }
        return output;
    }
    let weights = gaussian_weights(sigma);
    let radius = (weights.len() / 2) as isize;
    let out_bounds = layer.bounds.expand(radius as usize, width, height);
    let temp_bounds = layer.bounds.expand(radius as usize * 2, width, height);
    let mut temp = Buffer4::new(temp_bounds);

    for y in temp_bounds.y0..temp_bounds.y1 {
        for x in temp_bounds.x0..temp_bounds.x1 {
            let mut sum = [0.0f64; 4];
            for (offset, &weight) in weights.iter().enumerate() {
                let dx = offset as isize - radius;
                let sample = layer.get_clamped(x as isize + dx, y as isize, width, height);
                for channel in 0..4 {
                    sum[channel] += sample[channel] as f64 * weight;
                }
            }
            let index = (y - temp_bounds.y0) * temp_bounds.width() + (x - temp_bounds.x0);
            temp.data[index] = [sum[0] as f32, sum[1] as f32, sum[2] as f32, sum[3] as f32];
        }
    }

    let mut output = Buffer4::new(out_bounds);
    for y in out_bounds.y0..out_bounds.y1 {
        for x in out_bounds.x0..out_bounds.x1 {
            let mut sum = [0.0f64; 4];
            for (offset, &weight) in weights.iter().enumerate() {
                let dy = offset as isize - radius;
                let sample = temp.get_clamped(x as isize, y as isize + dy, width, height);
                for channel in 0..4 {
                    sum[channel] += sample[channel] as f64 * weight;
                }
            }
            let index = (y - out_bounds.y0) * out_bounds.width() + (x - out_bounds.x0);
            output.data[index] = [sum[0] as f32, sum[1] as f32, sum[2] as f32, sum[3] as f32];
        }
    }
    output
}

// --- Compositing, noise, transforms ------------------------------------

fn composite_frame(
    layers: &[Layer],
    config: &SyntheticConfig,
    focus: f64,
    exposure: f64,
    seed: u64,
    radii: &mut [f64],
) -> Vec<f32> {
    let width = config.width as usize;
    let height = config.height as usize;
    let mut output = vec![0.0f32; width * height * 3];
    let mut alpha = vec![0.0f32; width * height];
    for (layer_index, layer) in layers.iter().enumerate() {
        if layer.bounds.is_empty() {
            continue;
        }
        let radius = blur_radius(config.coc_scale, config.c_max, layer.depth, focus);
        radii[layer_index] = radius;
        let Some(blurred) = blur_layer(layer, width, height, radius, config.psf) else {
            continue;
        };
        for y in blurred.bounds.y0..blurred.bounds.y1 {
            for x in blurred.bounds.x0..blurred.bounds.x1 {
                let index = y * width + x;
                let sample = blurred.get(x, y);
                let current_alpha = alpha[index];
                let remaining = 1.0 - current_alpha;
                output[index * 3] += sample[0] * remaining;
                output[index * 3 + 1] += sample[1] * remaining;
                output[index * 3 + 2] += sample[2] * remaining;
                alpha[index] = (current_alpha + sample[3] * remaining).min(1.0);
            }
        }
    }

    let mut rng = Rng::new(seed);
    for value in &mut output {
        let mut linear = *value as f64;
        if config.noise_shot > 0.0 {
            let grain = config.noise_shot * config.noise_shot;
            let photons = (linear.max(0.0) / grain).round();
            let count = poisson(&mut rng, photons);
            linear = count * grain;
        }
        if config.noise_read > 0.0 {
            linear += config.noise_read * rng.gaussian();
        }
        *value = (linear * exposure) as f32;
    }
    output
}

fn is_identity(transform: &Similarity) -> bool {
    (transform.a - 1.0).abs() < 1.0e-12
        && transform.b.abs() < 1.0e-12
        && transform.tx.abs() < 1.0e-12
        && transform.ty.abs() < 1.0e-12
}

fn warp_linear(source: &[f32], width: usize, height: usize, transform: &Similarity) -> Vec<f32> {
    let mut output = vec![0.0f32; source.len()];
    for y in 0..height {
        for x in 0..width {
            let sx = transform.a * x as f64 - transform.b * y as f64 + transform.tx;
            let sy = transform.b * x as f64 + transform.a * y as f64 + transform.ty;
            let mut pixel = [0.0f32; 3];
            sample_bilinear(source, width, height, sx, sy, &mut pixel);
            let index = (y * width + x) * 3;
            output[index..index + 3].copy_from_slice(&pixel);
        }
    }
    output
}

fn sample_bilinear(
    source: &[f32],
    width: usize,
    height: usize,
    x: f64,
    y: f64,
    out: &mut [f32; 3],
) {
    let x = x.clamp(0.0, (width - 1) as f64);
    let y = y.clamp(0.0, (height - 1) as f64);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;
    for channel in 0..3 {
        let p00 = source[(y0 * width + x0) * 3 + channel];
        let p10 = source[(y0 * width + x1) * 3 + channel];
        let p01 = source[(y1 * width + x0) * 3 + channel];
        let p11 = source[(y1 * width + x1) * 3 + channel];
        let top = p00 + (p10 - p00) * fx;
        let bottom = p01 + (p11 - p01) * fx;
        out[channel] = top + (bottom - top) * fy;
    }
}

fn quantize_16(value: f64) -> u16 {
    (value * 65535.0).round().clamp(0.0, 65535.0) as u16
}

// --- Deterministic random numbers --------------------------------------

#[derive(Debug, Clone)]
struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self {
            state: seed ^ 0x9E37_79B9_7F4A_7C15,
        }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn gaussian(&mut self) -> f64 {
        let u1 = self.next_f64().max(1.0e-12);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// Poisson sampler. Large means use the normal approximation, which keeps
/// high-photon-count fixtures fast while remaining deterministic.
fn poisson(rng: &mut Rng, lambda: f64) -> f64 {
    if !lambda.is_finite() || lambda <= 0.0 {
        return 0.0;
    }
    if lambda < 30.0 {
        let limit = (-lambda).exp();
        let mut product = 1.0;
        let mut count = 0.0;
        loop {
            product *= rng.next_f64();
            if product <= limit {
                return count;
            }
            count += 1.0;
            if count > 1_000.0 {
                return count;
            }
        }
    }
    let value = lambda + lambda.sqrt() * rng.gaussian();
    value.max(0.0).round()
}

// --- Procedural scene ---------------------------------------------------

/// A deterministic, textured all-in-focus scene and wavy depth map.
fn procedural_scene(config: &SyntheticConfig) -> (Vec<f32>, Vec<f32>) {
    let width = config.width as usize;
    let height = config.height as usize;
    let mut rng = Rng::new(config.seed ^ 0xA5A5_5A5A_1234_5678);
    let mut scene = Vec::with_capacity(width * height * 3);
    let mut depth = Vec::with_capacity(width * height);
    let w = width.max(2) as f64;
    let h = height.max(2) as f64;

    for y in 0..height {
        let v = y as f64 / (h - 1.0);
        for x in 0..width {
            let u = x as f64 / (w - 1.0);
            let grain = rng.next_f64();
            let stripes = 0.5 + 0.5 * (u * 47.0 + v * 13.0).sin();
            let rings = 0.5 + 0.5 * ((u - 0.62).hypot(v - 0.35) * 61.0).sin();
            let checker = if ((x / 9) + (y / 9)) % 2 == 0 {
                1.0
            } else {
                0.0
            };
            let texture = 0.35 * stripes + 0.25 * rings + 0.20 * checker + 0.20 * grain;
            scene.push((0.10 + 0.75 * texture) as f32);
            scene.push((0.08 + 0.65 * (1.0 - texture) + 0.10 * rings) as f32);
            scene.push((0.12 + 0.55 * stripes * (0.5 + 0.5 * grain)) as f32);

            // A far background with two localized subject blobs keeps most
            // layers' bounding boxes small, so fixture rendering stays fast.
            let near = (-((u - 0.31).powi(2) + (v - 0.63).powi(2)) / 0.014).exp();
            let middle = (-((u - 0.73).powi(2) + (v - 0.36).powi(2)) / 0.020).exp();
            let mut z = 1.70;
            z += (0.18 - z) * near;
            z += (0.62 - z) * middle * (1.0 - near);
            depth.push(z as f32);
        }
    }
    (scene, depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn small_config() -> SyntheticConfig {
        SyntheticConfig {
            width: 32,
            height: 32,
            frames: 2,
            seed: 7,
            coc_scale: 2.0,
            focus: vec![1.0, 0.25],
            noise_read: 0.0,
            noise_shot: 0.0,
            exposure: vec![1.0, 1.0],
            layers: 8,
            psf: Psf::Disk,
            c_max: 12.0,
            transforms: Vec::new(),
        }
    }

    #[test]
    fn radius_follows_analytic_formula_and_clamps() {
        assert!((blur_radius(2.0, 12.0, 0.5, 1.0) - 2.0).abs() < 1.0e-12);
        assert!((blur_radius(2.0, 1.0, 0.1, 1.0) - 1.0).abs() < 1.0e-12);
        assert_eq!(blur_radius(2.0, 12.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn disk_kernel_is_normalized_and_grows_with_radius() {
        for radius in [1usize, 2, 3, 5] {
            let widths = disk_widths(radius);
            let total: f64 = widths.iter().map(|w| (2 * w + 1) as f64).sum();
            assert!(total > 0.0);
        }
        let moment = |radius: usize| {
            let widths = disk_widths(radius);
            let norm: f64 = widths.iter().map(|w| (2 * w + 1) as f64).sum();
            let mut sum = 0.0;
            for (offset, &half) in widths.iter().enumerate() {
                let dy = offset as isize - radius as isize;
                for dx in -(half as isize)..=(half as isize) {
                    sum += (dx * dx + dy * dy) as f64;
                }
            }
            sum / norm
        };
        assert!(moment(5) > moment(3));
        assert!(moment(3) > moment(1));
    }

    #[test]
    fn constant_depth_delta_has_disk_second_moment() {
        let width = 64;
        let height = 64;
        let mut scene = vec![0.0f32; width * height * 3];
        let center = ((height / 2) * width + width / 2) * 3;
        scene[center] = 1.0;
        scene[center + 1] = 1.0;
        scene[center + 2] = 1.0;
        let depth = vec![1.0f32; width * height];
        let config = SyntheticConfig {
            width: width as u32,
            height: height as u32,
            frames: 1,
            seed: 3,
            coc_scale: 4.0,
            focus: vec![0.5],
            noise_read: 0.0,
            noise_shot: 0.0,
            exposure: vec![1.0],
            layers: 4,
            psf: Psf::Disk,
            c_max: 8.0,
            transforms: Vec::new(),
        };
        let stack = SyntheticStack::generate_from_scene(&scene, &depth, &config).unwrap();
        assert!((stack.layer_blur_radii[0][0] - 4.0).abs() < 1.0e-12);
        let frame = stack.frame_linear(0);
        let total: f64 = frame.iter().step_by(3).map(|&v| v as f64).sum();
        assert!((total - 1.0).abs() < 0.002, "energy = {total}");
        let mut moment = 0.0;
        for y in 0..height {
            for x in 0..width {
                let dx = x as f64 - (width / 2) as f64;
                let dy = y as f64 - (height / 2) as f64;
                moment += frame[(y * width + x) * 3] as f64 * (dx * dx + dy * dy);
            }
        }
        // Continuous uniform disk: E[r^2] = r^2 / 2.
        let expected = 16.0 / 2.0;
        assert!(
            (moment - expected).abs() < expected * 0.15,
            "moment = {moment}"
        );
    }

    #[test]
    fn constant_depth_delta_has_gaussian_second_moment() {
        let width = 64;
        let height = 64;
        let mut scene = vec![0.0f32; width * height * 3];
        let center = ((height / 2) * width + width / 2) * 3;
        scene[center..center + 3].fill(1.0);
        let depth = vec![1.0f32; width * height];
        let config = SyntheticConfig {
            width: width as u32,
            height: height as u32,
            frames: 1,
            seed: 3,
            coc_scale: 2.0,
            focus: vec![0.5],
            noise_read: 0.0,
            noise_shot: 0.0,
            exposure: vec![1.0],
            layers: 4,
            psf: Psf::Gaussian,
            c_max: 8.0,
            transforms: Vec::new(),
        };
        let stack = SyntheticStack::generate_from_scene(&scene, &depth, &config).unwrap();
        let frame = stack.frame_linear(0);
        let total: f64 = frame.iter().step_by(3).map(|&v| v as f64).sum();
        assert!((total - 1.0).abs() < 0.01, "energy = {total}");
        let mut moment = 0.0;
        for y in 0..height {
            for x in 0..width {
                let dx = x as f64 - (width / 2) as f64;
                let dy = y as f64 - (height / 2) as f64;
                moment += frame[(y * width + x) * 3] as f64 * (dx * dx + dy * dy);
            }
        }
        // 2D Gaussian: E[r^2] = 2 * sigma^2, sigma = c / 1.5.
        let sigma = 2.0 / 1.5;
        let expected = 2.0 * sigma * sigma;
        assert!(
            (moment - expected).abs() < expected * 0.15,
            "moment = {moment}"
        );
    }

    #[test]
    fn in_focus_frame_is_sharper_than_defocused_frame() {
        let width = 48;
        let height = 48;
        let mut scene = vec![0.0f32; width * height * 3];
        for y in 0..height {
            for x in 0..width {
                let value = if ((x / 6) + (y / 6)) % 2 == 0 {
                    0.9
                } else {
                    0.1
                };
                let index = (y * width + x) * 3;
                scene[index] = value;
                scene[index + 1] = value;
                scene[index + 2] = value;
            }
        }
        let depth = vec![1.0f32; width * height];
        let config = SyntheticConfig {
            width: width as u32,
            height: height as u32,
            frames: 2,
            seed: 11,
            coc_scale: 6.0,
            focus: vec![1.0, 0.08],
            noise_read: 0.0,
            noise_shot: 0.0,
            exposure: vec![1.0, 1.0],
            layers: 4,
            psf: Psf::Disk,
            c_max: 8.0,
            transforms: Vec::new(),
        };
        let stack = SyntheticStack::generate_from_scene(&scene, &depth, &config).unwrap();
        let sharp = gradient_energy(&stack.frame_linear(0), width, height);
        let soft = gradient_energy(&stack.frame_linear(1), width, height);
        assert!(sharp > soft * 5.0, "sharp = {sharp}, soft = {soft}");
    }

    fn gradient_energy(frame: &[f32], width: usize, height: usize) -> f64 {
        let mut total = 0.0;
        for y in 1..height - 1 {
            for x in 1..width - 1 {
                let index = (y * width + x) * 3;
                let dx = (frame[index + 3] - frame[index - 3]) as f64;
                let dy = (frame[index + width * 3] - frame[index - width * 3]) as f64;
                total += dx * dx + dy * dy;
            }
        }
        total
    }

    #[test]
    fn generation_is_deterministic_and_seed_sensitive() {
        let config = small_config();
        let first = SyntheticStack::generate(&config).unwrap();
        let second = SyntheticStack::generate(&config).unwrap();
        assert_eq!(first.frames, second.frames);
        assert_eq!(first.scene, second.scene);
        let mut other = config;
        other.seed = 8;
        let third = SyntheticStack::generate(&other).unwrap();
        assert_ne!(first.frames, third.frames);
    }

    #[test]
    fn exposure_gain_scales_linear_output() {
        let width = 24;
        let height = 24;
        let scene = vec![0.4f32; width * height * 3];
        let depth = vec![1.0f32; width * height];
        let mut config = small_config();
        config.width = width as u32;
        config.height = height as u32;
        config.frames = 1;
        config.focus = vec![1.0];
        config.layers = 2;
        config.coc_scale = 0.0;
        config.c_max = 0.0;
        config.exposure = vec![0.5];
        let stack = SyntheticStack::generate_from_scene(&scene, &depth, &config).unwrap();
        let frame = stack.frame_linear(0);
        let mean =
            frame.iter().step_by(3).map(|&v| v as f64).sum::<f64>() / (width * height) as f64;
        assert!((mean - 0.2).abs() < 0.002, "mean = {mean}");
    }

    #[test]
    fn parse_toml_and_reject_invalid_configs() {
        let config = SyntheticConfig::from_toml_str(
            r#"
width = 16
height = 12
frames = 2
seed = 42
coc_scale = 1.0
focus = [0.5, 1.0]
layers = 4
psf = "gaussian"
"#,
        )
        .unwrap();
        assert_eq!(config.width, 16);
        assert_eq!(config.psf, Psf::Gaussian);
        assert_eq!(config.exposure, Vec::<f64>::new());
        assert!(SyntheticConfig::from_toml_str("width = 0\n").is_err());
        assert!(
            SyntheticConfig::from_toml_str(
                "width = 2\nheight = 2\nframes = 2\nseed = 1\ncoc_scale = 1\nfocus = [1.0]"
            )
            .is_err()
        );
    }

    #[test]
    fn known_transform_moves_the_frame() {
        let width = 32;
        let height = 32;
        let mut scene = vec![0.0f32; width * height * 3];
        let center = ((height / 2) * width + width / 2) * 3;
        scene[center] = 1.0;
        scene[center + 1] = 1.0;
        scene[center + 2] = 1.0;
        let depth = vec![1.0f32; width * height];
        let mut config = small_config();
        config.width = width as u32;
        config.height = height as u32;
        config.frames = 2;
        config.focus = vec![1.0, 1.0];
        config.layers = 2;
        config.coc_scale = 0.0;
        config.c_max = 0.0;
        config.transforms = vec![
            TransformConfig {
                a: 1.0,
                b: 0.0,
                tx: 0.0,
                ty: 0.0,
            },
            TransformConfig {
                a: 1.0,
                b: 0.0,
                tx: 2.0,
                ty: 0.0,
            },
        ];
        let stack = SyntheticStack::generate_from_scene(&scene, &depth, &config).unwrap();
        assert_eq!(stack.transforms[1].tx, 2.0);
        let frame = stack.frame_linear(1);
        let peak = frame
            .iter()
            .step_by(3)
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(index, _)| index)
            .unwrap();
        // `Similarity` maps output coordinates to source coordinates, so a
        // positive `tx` moves the rendered content left.
        assert_eq!(peak % width, width / 2 - 2);
    }
}
