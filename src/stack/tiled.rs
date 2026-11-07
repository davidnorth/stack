//! Banded/tiled PMax blending for large images.
//!
//! `docs/architecture.md` §5 asks for banded processing at 60 MP and §10 names
//! tiling as an extension point. This module evaluates the same Laplacian
//! pyramid blend as [`super::pmax::blend`] one output tile at a time. Each tile
//! is expanded by an overlap before pyramid levels are cropped, and only the
//! requested rectangle is copied back after reconstruction, so seams cannot
//! appear and the result matches the untiled blend within one 16-bit LSB.
//!
//! The current public API still takes full-resolution frames and PMax weights;
//! the banding bounds the additional blend and reconstruction workspace.
//! [`working_set_bytes`] includes those caller-owned buffers, so
//! [`TiledOptions::for_memory_budget`] can be used to choose a tile size when
//! `--max-memory` is tight.

use crate::color::luma::linear_luma;
use crate::focus::pyramid::{Pyramid, level_count};
use crate::stack::pmax::{BlendOptions, LevelWeights, reconstruct};

/// Default output tile width. Values above the image width are clamped, so the
/// default is full-width horizontal bands.
pub const DEFAULT_TILE_WIDTH: u32 = u32::MAX;
/// Default output tile height.
pub const DEFAULT_TILE_HEIGHT: u32 = 2048;
/// Default pixels of context kept around each tile before reconstruction.
pub const DEFAULT_OVERLAP: u32 = 64;

/// Output rectangle and tiling parameters for [`blend`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TiledOptions {
    /// Output tile width in pixels.
    pub tile_width: u32,
    /// Output tile height in pixels.
    pub tile_height: u32,
    /// Context pixels kept around the output tile. The effective overlap is at
    /// least the coarsest-level alignment needed by the pyramid.
    pub overlap: u32,
}

impl Default for TiledOptions {
    fn default() -> Self {
        Self {
            tile_width: DEFAULT_TILE_WIDTH,
            tile_height: DEFAULT_TILE_HEIGHT,
            overlap: DEFAULT_OVERLAP,
        }
    }
}

impl TiledOptions {
    /// Tiling options with explicit dimensions.
    #[must_use]
    pub const fn new(tile_width: u32, tile_height: u32, overlap: u32) -> Self {
        Self {
            tile_width,
            tile_height,
            overlap,
        }
    }

    /// Choose the largest tile that fits `max_memory_bytes`.
    ///
    /// Returns `None` when the caller-owned frames, weights, output, and one
    /// frame pyramid alone exceed the budget; tiling cannot make that fit.
    #[must_use]
    pub fn for_memory_budget(
        max_memory_bytes: usize,
        width: u32,
        height: u32,
        frames: usize,
        overlap: u32,
    ) -> Option<Self> {
        assert!(width > 0 && height > 0, "image dimensions must be non-zero");
        assert!(frames > 0, "at least one frame is required");

        let mut options = Self {
            tile_width: width,
            tile_height: DEFAULT_TILE_HEIGHT.min(height).max(1),
            overlap,
        };
        if working_set_bytes(width, height, frames, options) <= max_memory_bytes {
            return Some(options);
        }

        while options.tile_width > 1 || options.tile_height > 1 {
            if options.tile_height > 1 {
                options.tile_height = options.tile_height.div_ceil(2);
            } else {
                options.tile_width = options.tile_width.div_ceil(2);
            }
            if working_set_bytes(width, height, frames, options) <= max_memory_bytes {
                return Some(options);
            }
        }
        None
    }
}

/// One output tile in pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tile {
    /// Left edge.
    pub x: u32,
    /// Top edge.
    pub y: u32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

/// Split an image into disjoint output tiles.
///
/// Tiles cover every pixel exactly once. Context overlap is not part of the
/// plan; [`blend`] expands each tile while processing it.
#[must_use]
pub fn plan(width: u32, height: u32, options: TiledOptions) -> Vec<Tile> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let tile_width = options.tile_width.max(1).min(width);
    let tile_height = options.tile_height.max(1).min(height);
    let mut tiles = Vec::new();

    let mut y = 0;
    while y < height {
        let tile_height = tile_height.min(height - y);
        let mut x = 0;
        while x < width {
            let tile_width = tile_width.min(width - x);
            tiles.push(Tile {
                x,
                y,
                width: tile_width,
                height: tile_height,
            });
            x += tile_width;
        }
        y += tile_height;
    }

    tiles
}

/// Estimated peak bytes for [`blend`], including caller-owned inputs.
///
/// The estimate covers the frame and weight buffers, the output, one
/// full-resolution channel and pyramid, and the band-sized blend scratch.
/// Allocator headers and `Vec` metadata are ignored.
#[must_use]
pub fn working_set_bytes(width: u32, height: u32, frames: usize, options: TiledOptions) -> usize {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    assert!(frames > 0, "at least one frame is required");

    let pixels = (width as usize) * (height as usize);
    let levels = level_count(width, height);
    let level_pixels = level_pixel_count(width, height);
    let bytes = std::mem::size_of::<f32>();

    let frames_bytes = frames * pixels * 3 * bytes;
    let weights_bytes = frames * level_pixels * 2 * bytes;
    let output_bytes = pixels * 3 * bytes;
    let channel_bytes = pixels * bytes;
    let blur_samples = ((width as usize).div_ceil(2)) * (height as usize);
    let pyramid_bytes = (2 * level_pixels + blur_samples) * bytes;

    let tile_width = options.tile_width.max(1).min(width) as usize;
    let tile_height = options.tile_height.max(1).min(height) as usize;
    let align = 1usize << (levels - 1);
    let halo = (options.overlap as usize).max(align);
    let expanded_width = (tile_width + 2 * halo).min(width as usize);
    let expanded_height = (tile_height + 2 * halo).min(height as usize);
    let expanded = expanded_width * expanded_height;
    // Cropped pyramid levels sum to about 4/3 of the finest crop; use 2x for
    // the temporary upsampling buffers too.
    let band_bytes = expanded * 2 * bytes;
    let tile_output_bytes = 3 * tile_width * tile_height * bytes;

    frames_bytes
        + weights_bytes
        + output_bytes
        + channel_bytes
        + pyramid_bytes
        + band_bytes
        + tile_output_bytes
}

/// Blend linear-light RGB frames with PMax weights, one tile at a time.
///
/// The result is equivalent to [`super::pmax::blend`] with the same inputs and
/// [`BlendOptions`]. Each tile is processed with an overlap of at least the
/// coarsest pyramid alignment so no seam is visible and the reconstructed
/// interior matches the untiled path within one 16-bit LSB.
///
/// # Panics
///
/// Panics if `frames` is empty, dimensions are zero, a buffer has the wrong
/// length, or the weight levels do not match the full-image pyramid.
#[must_use]
pub fn blend(
    frames: &[&[f32]],
    width: u32,
    height: u32,
    weights: &LevelWeights,
    options: BlendOptions,
    tiled: TiledOptions,
) -> Vec<f32> {
    assert!(!frames.is_empty(), "at least one frame is required");
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    for frame in frames {
        assert_eq!(frame.len(), count * 3, "RGB buffer length mismatch");
    }
    assert_eq!(
        weights.len(),
        level_count(width, height),
        "weight and pyramid level count mismatch"
    );

    let mut out = vec![0.0f32; count * 3];
    for tile in plan(width, height, tiled) {
        let luma = blend_channel_tile(
            frames,
            (width, height),
            weights,
            Channel::Luma,
            options,
            tiled,
            tile,
        );
        let cb = blend_channel_tile(
            frames,
            (width, height),
            weights,
            Channel::Cb,
            options,
            tiled,
            tile,
        );
        let cr = blend_channel_tile(
            frames,
            (width, height),
            weights,
            Channel::Cr,
            options,
            tiled,
            tile,
        );

        let tile_width = tile.width as usize;
        for dy in 0..tile.height as usize {
            let out_row = ((tile.y as usize + dy) * width as usize + tile.x as usize) * 3;
            let band_row = dy * tile_width;
            for dx in 0..tile_width {
                let index = band_row + dx;
                let y = luma[index];
                let cb = cb[index];
                let cr = cr[index];
                let r = y + cr;
                let b = y + cb;
                let g = y - (0.2126 * cr + 0.0722 * cb) / 0.7152;
                out[out_row + dx * 3] = r.clamp(0.0, 1.0);
                out[out_row + dx * 3 + 1] = g.clamp(0.0, 1.0);
                out[out_row + dx * 3 + 2] = b.clamp(0.0, 1.0);
            }
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Channel {
    Luma,
    Cb,
    Cr,
}

/// Blend one channel for `tile` and return only the requested output rectangle.
fn blend_channel_tile(
    frames: &[&[f32]],
    (width, height): (u32, u32),
    weights: &LevelWeights,
    channel: Channel,
    options: BlendOptions,
    tiled: TiledOptions,
    tile: Tile,
) -> Vec<f32> {
    let levels = weights.len();
    let align = 1u32 << (levels - 1);
    let halo = tiled.overlap.max(align);

    let expanded_x0 = tile.x.saturating_sub(halo) / align * align;
    let expanded_y0 = tile.y.saturating_sub(halo) / align * align;
    let expanded_x1 = ((tile.x + tile.width).saturating_add(halo)).min(width);
    let expanded_y1 = ((tile.y + tile.height).saturating_add(halo)).min(height);
    let expanded_x1 = expanded_x1.div_ceil(align) * align;
    let expanded_y1 = expanded_y1.div_ceil(align) * align;
    let expanded_x1 = expanded_x1.min(width);
    let expanded_y1 = expanded_y1.min(height);

    let mut dimensions = Vec::with_capacity(levels);
    let mut bounds = Vec::with_capacity(levels);
    for level in 0..levels {
        let weight_level = weights.level(level).expect("weight level exists");
        let step = 1u32 << level;
        let x0 = expanded_x0 / step;
        let y0 = expanded_y0 / step;
        let x1 = expanded_x1.div_ceil(step).min(weight_level.width);
        let y1 = expanded_y1.div_ceil(step).min(weight_level.height);
        assert!(x1 > x0 && y1 > y0, "tile crop is empty");
        dimensions.push((x1 - x0, y1 - y0));
        bounds.push((x0, y0));
    }

    let mut blended: Vec<Vec<f32>> = dimensions
        .iter()
        .map(|&(level_width, level_height)| {
            vec![0.0f32; (level_width as usize) * (level_height as usize)]
        })
        .collect();
    let inv_n = 1.0 / frames.len() as f32;
    let anchor_start = levels.saturating_sub(options.tone_anchor_levels.min(levels));

    for (frame, rgb) in frames.iter().enumerate() {
        let values = channel_buffer(rgb, channel);
        let mut pyramid = Pyramid::new();
        pyramid.build(&values, width, height);
        assert_eq!(
            pyramid.levels().len(),
            levels,
            "weight and pyramid level count mismatch"
        );

        for (level_index, (out, &(level_width, level_height))) in
            blended.iter_mut().zip(&dimensions).enumerate()
        {
            let source = &pyramid.levels()[level_index];
            let (x0, y0) = bounds[level_index];
            let weight_level = weights.level(level_index).expect("weight level exists");
            let frame_weights = if level_index >= anchor_start {
                None
            } else if channel == Channel::Luma {
                weight_level.frame_weights(frame)
            } else {
                weight_level.frame_chroma(frame)
            };

            let source_stride = source.width as usize;
            let width_usize = level_width as usize;
            let height_usize = level_height as usize;
            match frame_weights {
                Some(frame_weights) => {
                    let weight_stride = weight_level.width as usize;
                    for y in 0..height_usize {
                        let source_row = (y0 as usize + y) * source_stride + x0 as usize;
                        let weight_row = (y0 as usize + y) * weight_stride + x0 as usize;
                        let out_row = y * width_usize;
                        for x in 0..width_usize {
                            out[out_row + x] +=
                                frame_weights[weight_row + x] * source.laplacian[source_row + x];
                        }
                    }
                }
                None => {
                    for y in 0..height_usize {
                        let source_row = (y0 as usize + y) * source_stride + x0 as usize;
                        let out_row = y * width_usize;
                        for x in 0..width_usize {
                            out[out_row + x] += inv_n * source.laplacian[source_row + x];
                        }
                    }
                }
            }
        }
    }

    let band = reconstruct(&blended, &dimensions);
    let band_width = dimensions[0].0 as usize;
    let origin_x = (tile.x - expanded_x0) as usize;
    let origin_y = (tile.y - expanded_y0) as usize;
    let mut tile_values = vec![0.0f32; (tile.width as usize) * (tile.height as usize)];
    for y in 0..tile.height as usize {
        let source_row = (origin_y + y) * band_width + origin_x;
        let dest_row = y * tile.width as usize;
        tile_values[dest_row..dest_row + tile.width as usize]
            .copy_from_slice(&band[source_row..source_row + tile.width as usize]);
    }
    tile_values
}

/// Extract one luma or chroma channel from interleaved linear RGB.
fn channel_buffer(rgb: &[f32], channel: Channel) -> Vec<f32> {
    let mut out = Vec::with_capacity(rgb.len() / 3);
    for pixel in rgb.as_chunks::<3>().0 {
        let y = linear_luma(pixel[0], pixel[1], pixel[2]);
        out.push(match channel {
            Channel::Luma => y,
            Channel::Cb => pixel[2] - y,
            Channel::Cr => pixel[0] - y,
        });
    }
    out
}

fn level_pixel_count(width: u32, height: u32) -> usize {
    let levels = level_count(width, height);
    let mut width = width as usize;
    let mut height = height as usize;
    let mut total = 0usize;
    for _ in 0..levels {
        total += width * height;
        width = width.div_ceil(2);
        height = height.div_ceil(2);
    }
    total
}
