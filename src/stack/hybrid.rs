//! Detail transfer and halo anchoring.
//!
//! The hybrid engine keeps DMap's colour and tone and adds PMax's high
//! frequencies (`docs/stacking.md` §4.1). This module implements the guided
//! base, the soft-clamped detail plane, and the YCbCr recombination.

use crate::color::luma::{REC709, linear_luma};
use crate::focus::confidence::guided_filter;

/// Default detail-transfer strength `k` from `docs/stacking.md` §4.1.
pub const DEFAULT_DETAIL_TRANSFER: f32 = 0.7;
/// Default soft-clamp knee `tau` in linear luma (`docs/stacking.md` §4.1).
pub const DETAIL_CLAMP_TAU: f32 = 0.10;
/// Default guided-base radius in pixels (`docs/stacking.md` §4.1).
pub const GUIDED_BASE_RADIUS: u32 = 24;
/// Default guided-base regularization (`docs/stacking.md` §4.1).
pub const GUIDED_BASE_EPSILON: f32 = 1.0e-3;

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

    let mut out = Vec::with_capacity(count * 3);
    for (&luma, (&cb, &cr)) in out_luma.iter().zip(dmap_cb.iter().zip(&dmap_cr)) {
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
