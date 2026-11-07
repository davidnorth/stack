//! Detail transfer and halo anchoring.
//!
//! The hybrid engine keeps DMap's colour and tone and adds PMax's high
//! frequencies (`docs/stacking.md` §4.1). This module implements the guided
//! base, the soft-clamped detail plane, and the YCbCr recombination.

use crate::color::luma::{REC709, linear_luma};
use crate::focus::confidence::guided_filter;
use crate::focus::pyramid::Pyramid;
use crate::stack::pmax::reconstruct;

/// Default detail-transfer strength `k` from `docs/stacking.md` §4.1.
pub const DEFAULT_DETAIL_TRANSFER: f32 = 0.7;
/// Default soft-clamp knee `tau` in linear luma (`docs/stacking.md` §4.1).
pub const DETAIL_CLAMP_TAU: f32 = 0.10;
/// Default guided-base radius in pixels (`docs/stacking.md` §4.1).
pub const GUIDED_BASE_RADIUS: u32 = 24;
/// Default guided-base regularization (`docs/stacking.md` §4.1).
pub const GUIDED_BASE_EPSILON: f32 = 1.0e-3;
/// Confidence at which the §4.3 hybrid mask starts to open.
pub const MASK_CONFIDENCE_LOW: f32 = 0.10;
/// Confidence at which the §4.3 hybrid mask is fully open.
pub const MASK_CONFIDENCE_HIGH: f32 = 0.30;
/// Texture ramp end as a multiple of the DMap threshold `t` (§4.3).
pub const MASK_TEXTURE_SCALE: f32 = 2.0;
/// Guided-filter radius used to align the mask with image edges (§4.3).
pub const MASK_GUIDED_RADIUS: u32 = 8;
/// Guided-filter regularization used to align the mask (§4.3).
pub const MASK_GUIDED_EPSILON: f32 = 1.0e-3;
/// Coarsest Laplacian levels forced to DMap coefficients (`docs/stacking.md` §4.2).
pub const HALO_ANCHOR_LEVELS: usize = 2;
/// Width in pixels of the edge band used by the overshoot check (§4.2).
pub const HALO_BAND_RADIUS: u32 = 5;
/// Allowed overshoot as a fraction of local contrast (§4.2).
pub const HALO_OVERSHOOT_LIMIT: f32 = 0.02;
/// Radius of the low-pass anchor used inside the edge band.
pub const HALO_ANCHOR_RADIUS: u32 = 4;
/// Regularization that turns the guided filter into a plain box low-pass.
const HALO_LOWPASS_EPSILON: f32 = 1.0e3;
/// Fraction of the strongest guide gradient treated as an edge.
const HALO_EDGE_FRACTION: f32 = 0.10;

/// Options for [`guided_base`], [`detail_plane`], [`transfer_detail`], and
/// [`stack_hybrid`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DetailOptions {
    /// Detail-transfer strength `k`. Clamped to `[0, 1]` per §4.2.
    pub k: f32,
    /// Soft-clamp knee `tau` in linear luma. Must be positive.
    pub tau: f32,
    /// Guided-filter box radius in pixels.
    pub radius: u32,
    /// Guided-filter regularization.
    pub epsilon: f32,
}

impl Default for DetailOptions {
    fn default() -> Self {
        Self {
            k: DEFAULT_DETAIL_TRANSFER,
            tau: DETAIL_CLAMP_TAU,
            radius: GUIDED_BASE_RADIUS,
            epsilon: GUIDED_BASE_EPSILON,
        }
    }
}

/// Options for the §4.3 confidence/texture mask.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaskOptions {
    /// Confidence at which the mask starts to open.
    pub confidence_low: f32,
    /// Confidence at which the confidence ramp reaches one.
    pub confidence_high: f32,
    /// Texture ramp end as a multiple of the DMap threshold.
    pub texture_scale: f32,
    /// Guided-filter radius used to align the mask with image edges.
    pub radius: u32,
    /// Guided-filter regularization.
    pub epsilon: f32,
}

impl Default for MaskOptions {
    fn default() -> Self {
        Self {
            confidence_low: MASK_CONFIDENCE_LOW,
            confidence_high: MASK_CONFIDENCE_HIGH,
            texture_scale: MASK_TEXTURE_SCALE,
            radius: MASK_GUIDED_RADIUS,
            epsilon: MASK_GUIDED_EPSILON,
        }
    }
}

/// Options for the §4.2 halo suppression pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HaloOptions {
    /// Coarsest Laplacian levels taken unconditionally from DMap.
    pub anchor_levels: usize,
    /// Width of the band around depth discontinuities, in pixels.
    pub band_radius: u32,
    /// Maximum allowed overshoot as a fraction of local contrast.
    pub overshoot_limit: f32,
    /// Radius of the low-pass anchor inside the band.
    pub anchor_radius: u32,
}

impl Default for HaloOptions {
    fn default() -> Self {
        Self {
            anchor_levels: HALO_ANCHOR_LEVELS,
            band_radius: HALO_BAND_RADIUS,
            overshoot_limit: HALO_OVERSHOOT_LIMIT,
            anchor_radius: HALO_ANCHOR_RADIUS,
        }
    }
}

/// Result of the halo diagnostics for one luma image.
#[derive(Debug, Clone, PartialEq)]
pub struct HaloLuma {
    /// Suppressed luma, row-major.
    pub luma: Vec<f32>,
    /// 99th-percentile overshoot beyond the anchor in the edge band.
    pub overshoot: f32,
    /// Detail strength after the local reduction, `k * min gain`.
    pub k_effective: f32,
}

/// Result of the full hybrid halo pass.
#[derive(Debug, Clone, PartialEq)]
pub struct HybridHalo {
    /// Interleaved linear RGB, clamped to `[0, 1]`.
    pub rgb: Vec<f32>,
    /// Suppressed Rec. 709 luma.
    pub luma: Vec<f32>,
    /// 99th-percentile overshoot beyond the anchor in the edge band.
    pub overshoot: f32,
    /// Detail strength after the local reduction, `k * min gain`.
    pub k_effective: f32,
}

/// Build the guided base `B = guided_filter(Y_P, Y_P, r, eps)` of §4.1.
///
/// The base carries PMax's large-scale tone; subtracting it leaves the detail
/// that the hybrid transfers onto DMap.
///
/// # Panics
///
/// Panics if dimensions are zero or `pmax_luma` has the wrong length.
#[must_use]
pub fn guided_base(pmax_luma: &[f32], width: u32, height: u32, options: DetailOptions) -> Vec<f32> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(pmax_luma.len(), count, "luma buffer length mismatch");

    let options = sanitize_options(options);
    guided_filter(
        pmax_luma,
        pmax_luma,
        width,
        height,
        options.radius,
        options.epsilon,
    )
}

/// Compute the soft-clamped detail added to DMap luma.
///
/// This is `k * tau * tanh((Y_P - B) / tau)`, so the added signal never exceeds
/// `k * tau` and high-contrast edges cannot create overshoot halos.
///
/// # Panics
///
/// Panics if dimensions are zero or `pmax_luma` has the wrong length.
#[must_use]
pub fn detail_plane(
    pmax_luma: &[f32],
    width: u32,
    height: u32,
    options: DetailOptions,
) -> Vec<f32> {
    let options = sanitize_options(options);
    let base = guided_base(pmax_luma, width, height, options);
    let mut detail = Vec::with_capacity(base.len());
    for (&value, &base) in pmax_luma.iter().zip(&base) {
        let residual = value - base;
        detail.push(options.k * options.tau * (residual / options.tau).tanh());
    }
    detail
}

/// Transfer PMax detail onto DMap luma: `Y_out = Y_D + detail`.
///
/// # Panics
///
/// Panics if the buffers disagree in length or dimensions are zero.
#[must_use]
pub fn transfer_detail(
    dmap_luma: &[f32],
    pmax_luma: &[f32],
    width: u32,
    height: u32,
    options: DetailOptions,
) -> Vec<f32> {
    assert_eq!(
        dmap_luma.len(),
        pmax_luma.len(),
        "luma buffer length mismatch"
    );
    let detail = detail_plane(pmax_luma, width, height, options);
    dmap_luma
        .iter()
        .zip(&detail)
        .map(|(&dmap, &detail)| dmap + detail)
        .collect()
}

/// Build the §4.3 hybrid mask from confidence and texture evidence.
///
/// The mask is the product of a confidence ramp and a texture ramp:
///
/// ```text
/// m = smoothstep(c_lo, c_hi, c) * smoothstep(t, 2t, Emax)
/// m = guided_filter(m, guide=Y_D, r=8, eps=1e-3)
/// ```
///
/// `confidence` and `best_energy` come from the DMap threshold pass, `threshold`
/// is its `t`, and `guide` is DMap luma `Y_D`. The result is in `[0, 1]`; zero
/// means pure DMap, one means full PMax detail.
///
/// # Panics
///
/// Panics if dimensions are zero or a buffer has the wrong length.
#[must_use]
pub fn hybrid_mask(
    confidence: &[f32],
    best_energy: &[f32],
    guide: &[f32],
    width: u32,
    height: u32,
    threshold: f32,
    options: MaskOptions,
) -> Vec<f32> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(confidence.len(), count, "confidence buffer length mismatch");
    assert_eq!(best_energy.len(), count, "energy buffer length mismatch");
    assert_eq!(guide.len(), count, "guide buffer length mismatch");

    let options = sanitize_mask_options(options);
    let threshold = sanitize_non_negative(threshold);
    let texture_end = (threshold * options.texture_scale).max(threshold + 1.0e-6);

    let mut raw = Vec::with_capacity(count);
    for index in 0..count {
        let confidence = smoothstep(
            options.confidence_low,
            options.confidence_high,
            confidence[index],
        );
        let texture = smoothstep(threshold, texture_end, best_energy[index]);
        raw.push(confidence * texture);
    }

    guided_filter(guide, &raw, width, height, options.radius, options.epsilon)
        .into_iter()
        .map(|value| value.clamp(0.0, 1.0))
        .collect()
}

/// Multiply a detail plane by the §4.3 mask.
///
/// # Panics
///
/// Panics if the buffers disagree in length.
#[must_use]
pub fn mask_detail_plane(detail: &[f32], mask: &[f32]) -> Vec<f32> {
    assert_eq!(detail.len(), mask.len(), "detail/mask length mismatch");
    detail
        .iter()
        .zip(mask)
        .map(|(&detail, &mask)| detail * mask.clamp(0.0, 1.0))
        .collect()
}

/// Transfer masked PMax detail onto DMap luma.
///
/// This is [`transfer_detail`] with the §4.3 mask applied after the soft clamp:
/// `Y_out = Y_D + m * k * tau * tanh((Y_P - B) / tau)`.
///
/// # Panics
///
/// Panics if the buffers disagree in length or dimensions are zero.
#[must_use]
pub fn transfer_detail_masked(
    dmap_luma: &[f32],
    pmax_luma: &[f32],
    mask: &[f32],
    width: u32,
    height: u32,
    options: DetailOptions,
) -> Vec<f32> {
    assert_eq!(
        dmap_luma.len(),
        pmax_luma.len(),
        "luma buffer length mismatch"
    );
    assert_eq!(
        dmap_luma.len(),
        mask.len(),
        "luma/mask buffer length mismatch"
    );
    let detail = mask_detail_plane(&detail_plane(pmax_luma, width, height, options), mask);
    dmap_luma
        .iter()
        .zip(&detail)
        .map(|(&dmap, &detail)| dmap + detail)
        .collect()
}

/// Low-pass anchor built from the coarsest `anchor_levels` Laplacian levels.
///
/// This is the DMap low-frequency image used by §4.2 mechanism 1: PMax
/// coefficients never control large-scale tone.
///
/// # Panics
///
/// Panics if dimensions are zero or `image` has the wrong length.
#[must_use]
pub fn pyramid_anchor(image: &[f32], width: u32, height: u32, anchor_levels: usize) -> Vec<f32> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    assert_eq!(
        image.len(),
        (width as usize) * (height as usize),
        "luma buffer length mismatch"
    );

    let mut pyramid = Pyramid::new();
    pyramid.build(image, width, height);
    let levels = pyramid.levels();
    let start = levels.len().saturating_sub(anchor_levels.max(1));
    let dimensions: Vec<(u32, u32)> = levels
        .iter()
        .map(|level| (level.width, level.height))
        .collect();
    let coefficients: Vec<Vec<f32>> = levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            if index >= start {
                level.laplacian.clone()
            } else {
                vec![0.0; level.len()]
            }
        })
        .collect();
    reconstruct(&coefficients, &dimensions)
}

/// Force the coarsest Laplacian levels of `fine` to `coarse` coefficients.
///
/// The fine levels (micro-detail) stay untouched, so this is a gradient-domain
/// blend rather than a blur of the whole image.
fn anchor_coarse(
    fine: &[f32],
    coarse: &[f32],
    width: u32,
    height: u32,
    anchor_levels: usize,
) -> Vec<f32> {
    let mut fine_pyramid = Pyramid::new();
    fine_pyramid.build(fine, width, height);
    let mut coarse_pyramid = Pyramid::new();
    coarse_pyramid.build(coarse, width, height);

    let levels = fine_pyramid.levels();
    let coarse_levels = coarse_pyramid.levels();
    let start = levels.len().saturating_sub(anchor_levels.max(1));
    let dimensions: Vec<(u32, u32)> = levels
        .iter()
        .map(|level| (level.width, level.height))
        .collect();
    let coefficients: Vec<Vec<f32>> = levels
        .iter()
        .enumerate()
        .map(|(index, level)| {
            if index >= start {
                coarse_levels[index].laplacian.clone()
            } else {
                level.laplacian.clone()
            }
        })
        .collect();
    reconstruct(&coefficients, &dimensions)
}

/// Measure halo overshoot beyond `reference` in a band around guide edges.
///
/// Edges come from the gradient of `guide` (a depth map or guide luma). The
/// band is `band_radius` pixels wide. A pixel overshoots when its value leaves
/// the local `[min, max]` range of `reference`; the overshoot is normalised by
/// that local contrast. The 99th percentile suppresses single-pixel outliers,
/// matching `docs/quality.md` §3.
///
/// # Panics
///
/// Panics if dimensions are zero or a buffer has the wrong length.
#[must_use]
pub fn halo_overshoot(
    output: &[f32],
    reference: &[f32],
    guide: &[f32],
    width: u32,
    height: u32,
    band_radius: u32,
) -> f32 {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(output.len(), count, "luma buffer length mismatch");
    assert_eq!(reference.len(), count, "luma buffer length mismatch");
    assert_eq!(guide.len(), count, "luma buffer length mismatch");

    let radius = band_radius.max(1);
    let band = edge_band(guide, width, height, radius);
    let (low, high) = local_min_max(reference, width, height, radius);

    let mut ratios = Vec::new();
    for index in 0..count {
        if !band[index] {
            continue;
        }
        let contrast = high[index] - low[index];
        if contrast <= 1.0e-6 {
            continue;
        }
        let overshoot = (output[index] - high[index])
            .max(low[index] - output[index])
            .max(0.0);
        ratios.push(overshoot / contrast);
    }
    percentile_99(ratios)
}

/// Apply the §4.2 halo suppression to a detail-transferred luma image.
///
/// Three mechanisms run together:
/// 1. the coarsest Laplacian levels are re-anchored to `dmap_luma`;
/// 2. inside the edge band, high frequencies are soft-clamped against a
///    low-pass anchor, which locally reduces the effective detail strength;
/// 3. the overshoot diagnostic reports the residual and the effective `k`.
///
/// # Panics
///
/// Panics if dimensions are zero or a buffer has the wrong length.
#[must_use]
pub fn suppress_halo(
    hybrid_luma: &[f32],
    dmap_luma: &[f32],
    width: u32,
    height: u32,
    detail: DetailOptions,
    halo: HaloOptions,
) -> HaloLuma {
    assert_eq!(
        hybrid_luma.len(),
        dmap_luma.len(),
        "luma buffer length mismatch"
    );
    let detail = sanitize_options(detail);
    let halo = sanitize_halo(halo);
    let count = hybrid_luma.len();

    let mut luma = anchor_coarse(hybrid_luma, dmap_luma, width, height, halo.anchor_levels);
    let anchor = lowpass_anchor(dmap_luma, width, height, halo.anchor_radius);
    let band = edge_band(dmap_luma, width, height, halo.band_radius);
    let (low, high) = local_min_max(dmap_luma, width, height, halo.band_radius);

    // Feather the band so the local clamp does not leave a visible seam.
    let band_mask: Vec<f32> = band.iter().map(|&edge| f32::from(edge)).collect();
    let weight = guided_filter(
        &band_mask,
        &band_mask,
        width,
        height,
        halo.band_radius,
        HALO_LOWPASS_EPSILON,
    );

    let mut min_gain = 1.0f32;
    for index in 0..count {
        if !band[index] {
            continue;
        }
        let contrast = (high[index] - low[index]).max(0.0);
        let allowed = (halo.overshoot_limit * contrast).max(1.0e-6);
        let high_frequency = luma[index] - anchor[index];
        let clamped = allowed * (high_frequency / allowed).tanh();
        let gain = if high_frequency.abs() > 1.0e-6 {
            (clamped / high_frequency).abs().clamp(0.0, 1.0)
        } else {
            1.0
        };
        min_gain = min_gain.min(gain);
        let clamped_pixel = anchor[index] + clamped;
        luma[index] += weight[index] * (clamped_pixel - luma[index]);
    }

    let overshoot = halo_overshoot(&luma, &anchor, dmap_luma, width, height, halo.band_radius);
    HaloLuma {
        luma,
        overshoot,
        k_effective: detail.k * min_gain,
    }
}

/// Run detail transfer followed by halo suppression on two linear RGB images.
///
/// `dmap` supplies tone and chroma; `pmax` supplies fine detail. Large-scale
/// tone is re-anchored to DMap and overshoot at depth edges is clamped per §4.2.
///
/// # Panics
///
/// Panics if dimensions are zero or either RGB buffer has the wrong length.
#[must_use]
pub fn stack_hybrid_halo(
    dmap: &[f32],
    pmax: &[f32],
    width: u32,
    height: u32,
    detail: DetailOptions,
    halo: HaloOptions,
) -> HybridHalo {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(dmap.len(), count * 3, "RGB buffer length mismatch");
    assert_eq!(pmax.len(), count * 3, "RGB buffer length mismatch");

    let (dmap_luma, dmap_cb, dmap_cr) = split_luma_chroma(dmap);
    let (pmax_luma, _, _) = split_luma_chroma(pmax);
    let hybrid = transfer_detail(&dmap_luma, &pmax_luma, width, height, detail);
    let suppressed = suppress_halo(&hybrid, &dmap_luma, width, height, detail, halo);
    let rgb = recombine_luma_chroma(&suppressed.luma, &dmap_cb, &dmap_cr);

    HybridHalo {
        rgb,
        luma: suppressed.luma,
        overshoot: suppressed.overshoot,
        k_effective: suppressed.k_effective,
    }
}

/// Run masked detail transfer followed by halo suppression.
///
/// This is the §4.1–§4.3 pipeline: the mask limits PMax detail to confident,
/// textured regions before §4.2 anchors the coarse levels back to DMap.
///
/// # Panics
///
/// Panics if dimensions are zero or a buffer has the wrong length.
#[must_use]
pub fn stack_hybrid_halo_masked(
    dmap: &[f32],
    pmax: &[f32],
    mask: &[f32],
    width: u32,
    height: u32,
    detail: DetailOptions,
    halo: HaloOptions,
) -> HybridHalo {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(dmap.len(), count * 3, "RGB buffer length mismatch");
    assert_eq!(pmax.len(), count * 3, "RGB buffer length mismatch");
    assert_eq!(mask.len(), count, "mask buffer length mismatch");

    let (dmap_luma, dmap_cb, dmap_cr) = split_luma_chroma(dmap);
    let (pmax_luma, _, _) = split_luma_chroma(pmax);
    let hybrid = transfer_detail_masked(&dmap_luma, &pmax_luma, mask, width, height, detail);
    let suppressed = suppress_halo(&hybrid, &dmap_luma, width, height, detail, halo);
    let rgb = recombine_luma_chroma(&suppressed.luma, &dmap_cb, &dmap_cr);

    HybridHalo {
        rgb,
        luma: suppressed.luma,
        overshoot: suppressed.overshoot,
        k_effective: suppressed.k_effective,
    }
}

/// Low-pass anchor for the edge-band clamp.
fn lowpass_anchor(dmap_luma: &[f32], width: u32, height: u32, radius: u32) -> Vec<f32> {
    guided_filter(
        dmap_luma,
        dmap_luma,
        width,
        height,
        radius,
        HALO_LOWPASS_EPSILON,
    )
}

/// Pixels within `radius` of a strong guide edge.
fn edge_band(guide: &[f32], width: u32, height: u32, radius: u32) -> Vec<bool> {
    let count = (width as usize) * (height as usize);
    let w = width as usize;
    let h = height as usize;

    let mut gradient = Vec::with_capacity(count);
    for y in 0..h {
        let y0 = y.saturating_sub(1);
        let y1 = (y + 1).min(h - 1);
        for x in 0..w {
            let x0 = x.saturating_sub(1);
            let x1 = (x + 1).min(w - 1);
            let gx = guide[y * w + x1] - guide[y * w + x0];
            let gy = guide[y1 * w + x] - guide[y0 * w + x];
            gradient.push((gx * gx + gy * gy).sqrt());
        }
    }

    let maximum = gradient.iter().copied().fold(0.0f32, f32::max);
    let threshold = (maximum * HALO_EDGE_FRACTION).max(1.0e-6);
    let edges: Vec<f32> = gradient
        .iter()
        .map(|&value| f32::from(value >= threshold))
        .collect();
    let (_, dilated) = local_min_max(&edges, width, height, radius);
    dilated.into_iter().map(|value| value > 0.5).collect()
}

/// Separable local minimum and maximum with a clamped square window.
fn local_min_max(values: &[f32], width: u32, height: u32, radius: u32) -> (Vec<f32>, Vec<f32>) {
    let w = width as usize;
    let h = height as usize;
    let r = radius as usize;

    let mut horizontal_min = vec![0.0f32; w * h];
    let mut horizontal_max = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let start = x.saturating_sub(r);
            let end = (x + r).min(w - 1);
            let mut low = f32::INFINITY;
            let mut high = f32::NEG_INFINITY;
            for sample in start..=end {
                let value = values[y * w + sample];
                low = low.min(value);
                high = high.max(value);
            }
            horizontal_min[y * w + x] = low;
            horizontal_max[y * w + x] = high;
        }
    }

    let mut low = vec![0.0f32; w * h];
    let mut high = vec![0.0f32; w * h];
    for y in 0..h {
        let start = y.saturating_sub(r);
        let end = (y + r).min(h - 1);
        for x in 0..w {
            let mut minimum = f32::INFINITY;
            let mut maximum = f32::NEG_INFINITY;
            for sample_y in start..=end {
                minimum = minimum.min(horizontal_min[sample_y * w + x]);
                maximum = maximum.max(horizontal_max[sample_y * w + x]);
            }
            low[y * w + x] = minimum;
            high[y * w + x] = maximum;
        }
    }
    (low, high)
}

/// 99th percentile of a set of non-negative ratios.
fn percentile_99(mut values: Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_unstable_by(f32::total_cmp);
    let index = (((values.len() - 1) as f32) * 0.99).round() as usize;
    values[index.min(values.len() - 1)]
}

/// Run the §4.1 detail transfer on two linear RGB images.
///
/// `dmap` supplies luma, chroma, and tone; `pmax` supplies only the guided
/// high-frequency detail. The result is linear RGB clamped to `[0, 1]`.
///
/// # Panics
///
/// Panics if dimensions are zero or either RGB buffer has the wrong length.
#[must_use]
pub fn stack_hybrid(
    dmap: &[f32],
    pmax: &[f32],
    width: u32,
    height: u32,
    options: DetailOptions,
) -> Vec<f32> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(dmap.len(), count * 3, "RGB buffer length mismatch");
    assert_eq!(pmax.len(), count * 3, "RGB buffer length mismatch");

    let (dmap_luma, dmap_cb, dmap_cr) = split_luma_chroma(dmap);
    let (pmax_luma, _, _) = split_luma_chroma(pmax);
    let out_luma = transfer_detail(&dmap_luma, &pmax_luma, width, height, options);
    recombine_luma_chroma(&out_luma, &dmap_cb, &dmap_cr)
}

/// Run the §4.3 masked detail transfer on two linear RGB images.
///
/// `mask` is one sample per pixel from [`hybrid_mask`]. DMap supplies luma,
/// chroma, and tone; PMax detail is added only where the mask is positive.
///
/// # Panics
///
/// Panics if dimensions are zero or a buffer has the wrong length.
#[must_use]
pub fn stack_hybrid_masked(
    dmap: &[f32],
    pmax: &[f32],
    mask: &[f32],
    width: u32,
    height: u32,
    options: DetailOptions,
) -> Vec<f32> {
    assert!(width > 0 && height > 0, "image dimensions must be non-zero");
    let count = (width as usize) * (height as usize);
    assert_eq!(dmap.len(), count * 3, "RGB buffer length mismatch");
    assert_eq!(pmax.len(), count * 3, "RGB buffer length mismatch");
    assert_eq!(mask.len(), count, "mask buffer length mismatch");

    let (dmap_luma, dmap_cb, dmap_cr) = split_luma_chroma(dmap);
    let (pmax_luma, _, _) = split_luma_chroma(pmax);
    let out_luma = transfer_detail_masked(&dmap_luma, &pmax_luma, mask, width, height, options);
    recombine_luma_chroma(&out_luma, &dmap_cb, &dmap_cr)
}

/// Recombine DMap chroma with a luma plane into interleaved linear RGB.
fn recombine_luma_chroma(luma: &[f32], cb: &[f32], cr: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(luma.len() * 3);
    for (&luma, (&cb, &cr)) in luma.iter().zip(cb.iter().zip(cr)) {
        let r = luma + cr;
        let b = luma + cb;
        let g = luma - (REC709[0] * cr + REC709[2] * cb) / REC709[1];
        out.extend_from_slice(&[r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0)]);
    }
    out
}

/// Split interleaved linear RGB into Rec. 709 luma and `Cb = B - Y`,
/// `Cr = R - Y`, matching the PMax engine's chroma convention.
fn split_luma_chroma(rgb: &[f32]) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let mut luma = Vec::with_capacity(rgb.len() / 3);
    let mut cb = Vec::with_capacity(rgb.len() / 3);
    let mut cr = Vec::with_capacity(rgb.len() / 3);
    for pixel in rgb.as_chunks::<3>().0 {
        let y = linear_luma(pixel[0], pixel[1], pixel[2]);
        luma.push(y);
        cb.push(pixel[2] - y);
        cr.push(pixel[0] - y);
    }
    (luma, cb, cr)
}

fn sanitize_options(options: DetailOptions) -> DetailOptions {
    DetailOptions {
        k: if options.k.is_finite() {
            options.k.clamp(0.0, 1.0)
        } else {
            DEFAULT_DETAIL_TRANSFER
        },
        tau: if options.tau.is_finite() {
            options.tau.max(1.0e-4)
        } else {
            DETAIL_CLAMP_TAU
        },
        radius: options.radius,
        epsilon: if options.epsilon.is_finite() {
            options.epsilon.max(f32::EPSILON)
        } else {
            GUIDED_BASE_EPSILON
        },
    }
}

fn sanitize_halo(options: HaloOptions) -> HaloOptions {
    HaloOptions {
        anchor_levels: options.anchor_levels.max(1),
        band_radius: options.band_radius.max(1),
        overshoot_limit: if options.overshoot_limit.is_finite() {
            options.overshoot_limit.max(0.0)
        } else {
            HALO_OVERSHOOT_LIMIT
        },
        anchor_radius: options.anchor_radius.max(1),
    }
}

fn sanitize_mask_options(options: MaskOptions) -> MaskOptions {
    let confidence_low = sanitize_non_negative(options.confidence_low);
    let confidence_high = if options.confidence_high.is_finite() {
        options.confidence_high.max(confidence_low)
    } else {
        MASK_CONFIDENCE_HIGH.max(confidence_low)
    };
    MaskOptions {
        confidence_low,
        confidence_high,
        texture_scale: if options.texture_scale.is_finite() {
            options.texture_scale.max(1.0)
        } else {
            MASK_TEXTURE_SCALE
        },
        radius: options.radius,
        epsilon: if options.epsilon.is_finite() {
            options.epsilon.max(f32::EPSILON)
        } else {
            MASK_GUIDED_EPSILON
        },
    }
}

/// Hermite smoothstep between `edge0` and `edge1`.
fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if !edge0.is_finite() || !edge1.is_finite() || edge1 <= edge0 {
        return if value >= edge1 { 1.0 } else { 0.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn sanitize_non_negative(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}
