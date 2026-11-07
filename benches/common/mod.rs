//! Shared data and configuration for the performance benches.
//!
//! The benches deliberately avoid the synthetic stack generator: its disk PSF
//! and layered compositing are far too slow for a 60 MP setup. A deterministic
//! gradient-plus-noise image is enough to exercise every stage while keeping
//! setup outside the measured region.

#![allow(dead_code)]

pub mod evidence;

use std::time::Duration;

use criterion::Criterion;

/// One benchmarked image size.
pub struct Size {
    /// Short name used in benchmark IDs.
    pub name: &'static str,
    pub width: u32,
    pub height: u32,
    /// Frame count for the stack benches.
    ///
    /// 60 MP uses two frames so the working set fits the 8 GB development
    /// machine. The 24 MP target uses the documented four-frame stack.
    pub stack_frames: usize,
}

/// The two sizes from `docs/quality.md` §6.
pub const SIZES: [Size; 2] = [
    Size {
        name: "24mp",
        width: 6000,
        height: 4000,
        stack_frames: 4,
    },
    Size {
        name: "60mp",
        width: 9500,
        height: 6333,
        stack_frames: 2,
    },
];

/// Criterion configuration shared by every bench target.
///
/// The default 100 samples are impractical for multi-second 60 MP stages.
/// Ten samples still give a stable median and keep a full local run bounded.
pub fn criterion_config() -> Criterion {
    Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .without_plots()
}

/// Deterministic interleaved RGB image in `[0, 1]`.
///
/// Contains smooth gradients, a periodic hard edge, and per-pixel noise so the
/// focus and alignment stages do real work on every pixel.
pub fn synthetic_rgb(width: u32, height: u32, seed: u64) -> Vec<f32> {
    let count = width as usize * height as usize;
    let mut out = Vec::with_capacity(count * 3);
    let inv_w = 1.0 / width as f32;
    let inv_h = 1.0 / height as f32;

    for y in 0..height {
        let fy = y as f32 * inv_h;
        let mut state = seed ^ (u64::from(y) << 32);
        for x in 0..width {
            state = xorshift(state);
            let noise = unit(state) * 0.08;
            let fx = x as f32 * inv_w;
            let stripe = if (x / 37 + y / 41) % 7 == 0 {
                0.12
            } else {
                0.0
            };
            let wave = 0.5 + 0.5 * (fx * 19.0 + fy * 11.0).sin();

            out.push((0.10 + 0.65 * fx + 0.15 * wave + noise).clamp(0.0, 1.0));
            out.push((0.10 + 0.65 * fy + 0.15 * (1.0 - wave) + noise).clamp(0.0, 1.0));
            out.push((0.15 + 0.45 * wave + stripe + noise).clamp(0.0, 1.0));
        }
    }

    out
}

/// Deterministic encoded-luma image in `[0, 1]`.
pub fn synthetic_luma(width: u32, height: u32, seed: u64) -> Vec<f32> {
    let count = width as usize * height as usize;
    let mut out = Vec::with_capacity(count);
    let inv_w = 1.0 / width as f32;
    let inv_h = 1.0 / height as f32;

    for y in 0..height {
        let fy = y as f32 * inv_h;
        let mut state = seed ^ (u64::from(y) << 32) ^ 0x9e37_79b9_7f4a_7c15;
        for x in 0..width {
            state = xorshift(state);
            let noise = unit(state) * 0.08;
            let fx = x as f32 * inv_w;
            let wave = 0.5 + 0.5 * (fx * 19.0 + fy * 11.0).sin();
            let stripe = if (x / 37 + y / 41) % 7 == 0 {
                0.15
            } else {
                0.0
            };
            out.push((0.12 + 0.55 * fx + 0.2 * wave + stripe + noise).clamp(0.0, 1.0));
        }
    }

    out
}

/// Integer shift used to make the alignment source differ from the reference.
pub fn shifted(input: &[f32], width: u32, height: u32, dx: i32, dy: i32) -> Vec<f32> {
    let w = width as i32;
    let h = height as i32;
    let mut out = vec![0.0; input.len()];
    for y in 0..h {
        let sy = (y - dy).clamp(0, h - 1);
        for x in 0..w {
            let sx = (x - dx).clamp(0, w - 1);
            out[(y * w + x) as usize] = input[(sy * w + sx) as usize];
        }
    }
    out
}

fn xorshift(mut state: u64) -> u64 {
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    state
}

fn unit(state: u64) -> f32 {
    // The low 24 bits are uniform enough for fixture data.
    (state & 0x00ff_ffff) as f32 / 16_777_216.0
}
