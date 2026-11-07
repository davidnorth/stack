# Focus measurement

## 1. Overview

The focus measure must answer, per pixel and per level: *which frame is
sharpest here, and how sure are we?* It drives both engines:

- **PMax** uses per-level energy directly as blend weights.
- **DMap** aggregates energy into a depth field.
- **Hybrid** uses confidence and texture to decide where each engine wins.

Requirements:
- monotonic with blur,
- robust to noise in flat regions (sky, water, fog),
- comparable across frames with slightly different exposure,
- local enough to track fine detail, smooth enough not to flip per pixel.

A multi-scale band-pass energy on encoded luma satisfies these. A single
full-resolution Laplacian does not: it is noisy and cannot bridge flat areas.

## 2. Laplacian pyramid

Input: encoded luma `Y'` (`docs/io-and-color.md` §6), `f32` in `[0,1]`.

```text
G_0 = Y'
G_{l+1} = downsample(blur(G_l))          blur = separable [1,4,6,4,1]/16
Lap_l   = G_l - upsample(G_{l+1})        l = 0..L-1
Lap_L   = G_L                            coarsest residual
```

- Downsample by taking every second pixel after the blur.
- Upsample with bilinear interpolation or the same kernel; document the choice
  and keep it consistent between analysis and reconstruction.
- Level count: `L = clamp(floor(log2(min(W,H))) - 6, 4, 8)`.
  At 24 MP that is 6 levels; at 60 MP, 7.
- Reconstruction must be exact within `1e-6` (S2.1 gate).
- Work in `f32`. Reuse buffers per frame to avoid allocation churn.

Pyramid level `l` represents detail at a scale of roughly `2^l` pixels.

## 3. Noise estimation

Noise is the main cause of "blotches" in flat regions. Never let it win.

For each level:

1. Build a low-gradient mask: `|grad G_l| < p20` (20th percentile).
2. `sigma_l = 1.4826 * median(|Lap_l|)` over that mask.
3. Noise floor: `tau_l = k * sigma_l^2`, with `k = 3` default.
4. Noise-subtracted energy: `E'_l = max(E_l - tau_l, 0)`.

Notes:
- The median is robust; texture inflates the mean but not the median over a
  low-gradient mask.
- If `sigma_l == 0` (clean synthetic input), use a small epsilon, not zero, to
  avoid division by zero.
- Report per-frame, per-level sigma in the JSON report. A sudden increase
  between frames is a red flag (exposure or noise reduction differences).
- `k` is exposed as `--noise-k` for debugging; do not expose it in normal use.

## 4. Energy maps

At each level:

```text
E_l(x) = box_filter(Lap_l(x)^2, r_l)
```

- Box filter via a summed-area table: O(1) per pixel, integer-friendly.
- Radius schedule: `r_l = max(2, round(r0 / 2^l))`.
- `r0` auto: `clamp(0.0027 * min(W, H), 12, 32)`.
  At 24 MP this is 12; at 60 MP, 16–26 depending on aspect.
- `r0` is the "estimation radius" analogue of Zerene's DMap setting. Large
  radius = more stable in flat areas, worse at edges. Small radius = tracks
  fine detail, noisier.
- Optional contrast normalization (off by default):
  `E_norm = E' / (box_filter(Y'^2, r_l) + 1e-4)`.
  Enable with `--contrast-normalize` when foreground is much darker than sky.
- Do not normalize per frame by a global scalar to "equalize" frames; that
  hides real focus differences. Fix exposure before stacking instead.

Cross-frame comparability:
- With identical exposure, profile, and aperture, raw `E'` is comparable.
- If `--exposure-match luma` is set, match frames before this stage
  (`docs/stacking.md` §6), not inside the focus measure.

## 5. Confidence and coverage

Let `E1 >= E2 >= ...` be the per-pixel energies after aggregation.

```text
margin      = (E1 - E2) / (E1 + eps)
confidence  = clamp(margin, 0, 1)
confidence  = guided_filter(confidence, guide=Y', r=8, eps=1e-3)
```

- `confidence` is high when one frame clearly wins, low when two frames are
  nearly tied (transition zone or flat region).
- Additional gate: `confidence = 0` where `E1 < tau_cov * sigma^2`, with
  `tau_cov = 4`. These pixels have no trustworthy sharp frame.
- Saturation gate: if the winning frame has any channel `>= 0.995` at that
  pixel, set `confidence = 0`. Depth will interpolate from neighbors.
- Coverage mask: `E1 < tau_cov * sigma^2`. Report its percentage and bounding
  boxes. In landscape, a large coverage-gap region usually means the focus
  step was too big; surface it as a warning even though `inspect` is deferred.

Guided filtering uses `Y'` as the guide so confidence edges align with image
edges. Implement the standard O(1) guided filter with box filters.

## 6. Aggregation

For DMap, aggregate levels into a single per-frame energy:

```text
E_i = sum_l w_l * E'_{i,l}
w_l = [1.0, 0.9, 0.8, 0.6, 0.5, 0.4, 0.3, 0.2]   (l = 0..L-1)
```

Rationale: fine levels carry the most reliable focus information; coarse
levels stabilize flat regions. For PMax, do **not** aggregate — use the
per-level energy at the matching pyramid level.

Per-level depth (choosing the frame at each scale and fusing) is a possible
future improvement; not in v1.

## 7. Parameters

| Name | Default | Notes |
|---|---|---|
| Pyramid levels | auto (4–8) | `floor(log2(min(W,H))) - 6` |
| Gaussian kernel | `[1,4,6,4,1]/16` | fixed |
| Estimation radius `r0` | auto 12–32 px | `--estimation-radius` |
| Noise multiplier `k` | 3 | `--noise-k` (debug) |
| Coverage multiplier | 4 | internal |
| Confidence guide radius | 8 px | internal |
| Confidence guide eps | `1e-3` | internal |
| Contrast normalization | off | `--contrast-normalize` |
| Aggregation weights | §6 | internal |

## 8. Alternatives considered

| Measure | Why not default |
|---|---|
| Variance of Laplacian | noisy, no scale structure, poor in flat areas |
| Tenengrad (Sobel energy) | good, but single-scale; can be added as a tie-breaker |
| Modified Laplacian (Nayeri) | better than plain Laplacian, still single-scale |
| Brenner gradient | simple, sensitive to noise and exposure |
| SML (sum-modified Laplacian) | reasonable; lacks the noise floor and confidence model |
| Wavelet/curvelet energy | good, more code; pyramid is sufficient |
| Learned focus measure | data and tuning cost; not justified for 3–4 frames |

The chosen measure is essentially a noise-aware, multi-scale band-pass energy.
It is not novel; the quality comes from the noise floor, confidence model, and
how the weights are used in `docs/stacking.md`.
