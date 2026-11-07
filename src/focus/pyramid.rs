//! Gaussian and Laplacian pyramids.
//!
//! Implements the multi-scale decomposition from `docs/focus-measure.md` §2 in
//! `f32`. The Gaussian pyramid uses the fixed separable kernel
//! `[1, 4, 6, 4, 1] / 16`; downsampling keeps every second sample after the
//! blur. Upsampling is bilinear with source coordinate `x / 2`, matching the
//! even-sample phase used by the downsample, and edge-clamped like the blur.
//! The same upsample is used for analysis and reconstruction.

/// Separable Gaussian kernel from `docs/focus-measure.md` §2.
const GAUSSIAN: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];

/// One Gaussian/Laplacian level.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PyramidLevel {
    pub width: u32,
    pub height: u32,
    /// `G_l`, row-major.
    pub gaussian: Vec<f32>,
    /// `Lap_l = G_l - upsample(G_{l+1})`, row-major. The last level stores
    /// the coarsest residual `Lap_L = G_L`.
    pub laplacian: Vec<f32>,
}

impl PyramidLevel {
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

/// Reusable pyramid workspace.
///
/// Call [`Pyramid::build`] once per frame. Level and blur buffers are retained
/// between calls, so repeated builds at similar sizes do not allocate.
#[derive(Debug, Clone, Default)]
pub struct Pyramid {
    width: u32,
    height: u32,
    levels: Vec<PyramidLevel>,
    blur: Vec<f32>,
}

impl Pyramid {
    /// Create an empty workspace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the Gaussian and Laplacian pyramids for encoded luma.
    ///
    /// `luma` is row-major `f32` in `[0, 1]`.
    ///
    /// # Panics
    ///
    /// Panics if `width` or `height` is zero, or if `luma` is not
    /// `width * height` samples long.
    pub fn build(&mut self, luma: &[f32], width: u32, height: u32) {
        assert!(width > 0 && height > 0, "image dimensions must be non-zero");
        assert_eq!(
            luma.len(),
            (width as usize) * (height as usize),
            "luma buffer length mismatch"
        );

        let count = level_count(width, height);
        self.width = width;
        self.height = height;
        self.levels.resize_with(count, PyramidLevel::default);

        let mut level_width = width;
        let mut level_height = height;
        for level in &mut self.levels {
            level.width = level_width;
            level.height = level_height;
            let len = (level_width as usize) * (level_height as usize);
            level.gaussian.resize(len, 0.0);
            level.laplacian.resize(len, 0.0);
            level_width = level_width.div_ceil(2);
            level_height = level_height.div_ceil(2);
        }

        self.levels[0].gaussian.copy_from_slice(luma);

        // G_{l+1} = downsample(blur(G_l)).
        for l in 0..count - 1 {
            let (src_w, src_h) = (self.levels[l].width, self.levels[l].height);
            let (dst_w, dst_h) = (self.levels[l + 1].width, self.levels[l + 1].height);
            let (head, tail) = self.levels.split_at_mut(l + 1);
            downsample_into(
                &head[l].gaussian,
                src_w,
                src_h,
                &mut tail[0].gaussian,
                dst_w,
                dst_h,
                &mut self.blur,
            );
        }

        // Lap_l = G_l - upsample(G_{l+1}); the last level is the residual G_L.
        let last = count - 1;
        for l in 0..last {
            let (src_w, src_h) = (self.levels[l + 1].width, self.levels[l + 1].height);
            let (dst_w, dst_h) = (self.levels[l].width, self.levels[l].height);
            let (head, tail) = self.levels.split_at_mut(l + 1);
            let level = &mut head[l];
            subtract_upsample_into(
                &level.gaussian,
                &tail[0].gaussian,
                src_w,
                src_h,
                &mut level.laplacian,
                dst_w,
                dst_h,
            );
        }

        let residual = &mut self.levels[last];
        residual.laplacian.copy_from_slice(&residual.gaussian);
    }

    /// Image width of the finest level.
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Image height of the finest level.
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Levels from finest to coarsest, including the residual level.
    #[must_use]
    pub fn levels(&self) -> &[PyramidLevel] {
        &self.levels
    }

    /// Whether [`Pyramid::build`] has not run yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// Reconstruct the finest level from the stored Laplacian coefficients.
    #[must_use]
    pub fn reconstruct(&self) -> Vec<f32> {
        let Some(coarsest) = self.levels.last() else {
            return Vec::new();
        };

        let mut current = coarsest.laplacian.clone();
        let (mut current_width, mut current_height) = (coarsest.width, coarsest.height);
        let mut upsampled = Vec::new();

        for level in self.levels.iter().rev().skip(1) {
            upsample_into(
                &current,
                current_width,
                current_height,
                &mut upsampled,
                level.width,
                level.height,
            );
            for (value, &laplacian) in upsampled.iter_mut().zip(&level.laplacian) {
                *value += laplacian;
            }
            std::mem::swap(&mut current, &mut upsampled);
            current_width = level.width;
            current_height = level.height;
        }

        current
    }

    /// Heap bytes currently held by the level and blur buffers.
    ///
    /// Struct and `Vec` headers are not included; this is the pixel storage
    /// that dominates peak memory.
    #[must_use]
    pub fn memory_bytes(&self) -> usize {
        let samples = self
            .levels
            .iter()
            .map(|level| level.gaussian.capacity() + level.laplacian.capacity())
            .sum::<usize>()
            + self.blur.capacity();
        samples * std::mem::size_of::<f32>()
    }
}

/// Number of stored Laplacian levels, including the coarsest residual.
///
/// `docs/focus-measure.md` §2 defines the coarsest band index as
/// `L = clamp(floor(log2(min(W, H))) - 6, 4, 8)`. The stored pyramid also
/// keeps `Lap_L = G_L`, hence the extra residual level. This gives six levels
/// at 24 MP (6000×4000) and seven at 60 MP (9504×6336), as documented.
#[must_use]
pub fn level_count(width: u32, height: u32) -> usize {
    let min_side = width.min(height).max(1);
    let floor_log2 = u32::BITS - 1 - min_side.leading_zeros();
    let bands = floor_log2.saturating_sub(6).clamp(4, 8);
    (bands + 1) as usize
}

/// Blur with the fixed kernel, then keep every second row and column.
fn downsample_into(
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst: &mut [f32],
    dst_w: u32,
    dst_h: u32,
    tmp: &mut Vec<f32>,
) {
    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;

    // Horizontal pass, evaluated only at the columns that survive downsampling.
    tmp.resize(sh * dw, 0.0);
    for y in 0..sh {
        let row = &src[y * sw..(y + 1) * sw];
        let out = &mut tmp[y * dw..(y + 1) * dw];
        for (dx, value) in out.iter_mut().enumerate() {
            let sx = 2 * dx;
            let mut sum = 0.0;
            for (k, &weight) in GAUSSIAN.iter().enumerate() {
                let x = (sx + k).saturating_sub(2).min(sw - 1);
                sum += weight * row[x];
            }
            *value = sum;
        }
    }

    // Vertical pass at the surviving rows.
    for dy in 0..dh {
        let sy = 2 * dy;
        let out = &mut dst[dy * dw..(dy + 1) * dw];
        for (dx, value) in out.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (k, &weight) in GAUSSIAN.iter().enumerate() {
                let y = (sy + k).saturating_sub(2).min(sh - 1);
                sum += weight * tmp[y * dw + dx];
            }
            *value = sum;
        }
    }
}

/// Compute `Lap = G - upsample(coarser)` directly, avoiding a full-size
/// temporary upsampled buffer.
fn subtract_upsample_into(
    gaussian: &[f32],
    src: &[f32],
    src_w: u32,
    src_h: u32,
    dst: &mut [f32],
    dst_w: u32,
    dst_h: u32,
) {
    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;

    for dy in 0..dh {
        let fy = dy as f32 * 0.5;
        let y0 = fy.floor() as usize;
        let ty = fy - y0 as f32;
        let y1 = (y0 + 1).min(sh - 1);
        let y0 = y0.min(sh - 1);

        for dx in 0..dw {
            let fx = dx as f32 * 0.5;
            let x0 = fx.floor() as usize;
            let tx = fx - x0 as f32;
            let x1 = (x0 + 1).min(sw - 1);
            let x0 = x0.min(sw - 1);

            let top = src[y0 * sw + x0] * (1.0 - tx) + src[y0 * sw + x1] * tx;
            let bottom = src[y1 * sw + x0] * (1.0 - tx) + src[y1 * sw + x1] * tx;
            let up = top * (1.0 - ty) + bottom * ty;
            let index = dy * dw + dx;
            dst[index] = gaussian[index] - up;
        }
    }
}

/// Bilinear upsample with source coordinate `x / 2` and edge clamping.
fn upsample_into(src: &[f32], src_w: u32, src_h: u32, dst: &mut Vec<f32>, dst_w: u32, dst_h: u32) {
    let sw = src_w as usize;
    let sh = src_h as usize;
    let dw = dst_w as usize;
    let dh = dst_h as usize;
    dst.resize(dw * dh, 0.0);

    for dy in 0..dh {
        let fy = dy as f32 * 0.5;
        let y0 = fy.floor() as usize;
        let ty = fy - y0 as f32;
        let y1 = (y0 + 1).min(sh - 1);
        let y0 = y0.min(sh - 1);

        for dx in 0..dw {
            let fx = dx as f32 * 0.5;
            let x0 = fx.floor() as usize;
            let tx = fx - x0 as f32;
            let x1 = (x0 + 1).min(sw - 1);
            let x0 = x0.min(sw - 1);

            let top = src[y0 * sw + x0] * (1.0 - tx) + src[y0 * sw + x1] * tx;
            let bottom = src[y1 * sw + x0] * (1.0 - tx) + src[y1 * sw + x1] * tx;
            dst[dy * dw + dx] = top * (1.0 - ty) + bottom * ty;
        }
    }
}
