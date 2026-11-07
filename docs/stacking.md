# Stacking

Inputs: aligned linear-light frames `I_i` (`i = 0..N-1`), per-frame Laplacian
pyramids, per-level energies `E_{i,l}`, and confidence `c_i` from
`docs/focus-measure.md`. All engines output linear RGB `f32`.

Notation: `N` is 3–4 in the target case; `d` is a continuous frame index.

---

## 1. Overview

| Engine | Strength | Weakness | Use |
|---|---|---|---|
| PMax | maximum detail, resolves overlapping edges | noisy in flat areas, color shifts | explicit, or masked regions |
| DMap | clean color, low noise, smooth transitions, depth output | softer, can miss fine overlaps | smooth/low-confidence regions |
| Hybrid | DMap tone + PMax detail | more parameters | default |

The hybrid is the product. PMax and DMap exist because the hybrid needs both,
and because explicit modes are useful for diagnosis and retouching.

---

## 2. PMax (pyramid maximum contrast)

### 2.1 Weights

Normalize each level's energy so one temperature works across scales:

```text
En_{i,l} = E'_{i,l} / (p99_l + eps)
w_{i,l}   = softmax(beta * En_{i,l})
```

- `beta` default 6. Higher = closer to hard max (sharper, noisier).
- `p99_l` is the 99th percentile of energy at level `l`, computed over all
  frames and the valid region.
- Low-confidence regions: floor toward uniform,

```text
w = (1 - c) * (1/N) + c * w
```

  so flat/saturated pixels average instead of picking noise.
- Chroma weights are smoother than luma weights:

```text
w_chroma = 0.5 * w + 0.5 / N
```

  This reduces color blotches from frame switching.

### 2.2 Blend and reconstruct

```text
Lap_out[l] = sum_i w_{i,l} * Lap_i[l]           l = 0..L-1
G_out[L]   = mean_i G_i[L]                      (or DMap lowpass in hybrid)
out        = reconstruct(Lap_out, G_out)
```

For standalone PMax, use uniform weights for the coarsest two levels. This
anchors tone and avoids large-scale color shifts. The finest four levels carry
the detail selection.

### 2.3 Chroma and tone

Convert to YCbCr-like space (or use a simple luma/chroma split):

```text
Y_out  = blend with w
CbCr_out = blend with w_chroma
```

Then convert back. This is the difference between a PMax that looks sharp but
muddy and one that keeps natural color. It is not a substitute for DMap in
smooth regions.

PMax output is allowed to be slightly noisier than DMap. Do not denoise it;
the hybrid handles smooth regions.

---

## 3. DMap (depth map)

### 3.1 Cost volume and initial depth

Aggregate energies per frame (`docs/focus-measure.md` §6). Define:

```text
d0(x) = argmax_i E_i(x)
```

`d0` is the initial depth estimate in frame-index units. It is noisy in flat
areas; the solve below fixes that.

### 3.2 Automatic contrast threshold

Zerene exposes a manual "contrast threshold" slider. We automate it and allow
an override.

1. Compute `Emax(x) = max_i E_i(x)`.
2. Estimate the noise floor `sigma^2` from the focus module.
3. Compute an Otsu threshold on `log(Emax + eps)`; this separates textured from
   flat regions without assuming a fixed percentage.
4. `t = max(k * sigma^2, otsu)`, capped at `0.1 * p99(Emax)`.
5. Confidence ramp: `c = smoothstep(t, 1.5*t, Emax) * margin_confidence`.
6. Report `t`, the confident percentage, and the coverage percentage.

Override: `--keep-detail PERCENT`. It sets the threshold to the
`(1 - PERCENT)` quantile of `Emax`, capped as above. Use it when the automatic
threshold is too aggressive on low-contrast subjects.

Low-confidence pixels are not "assigned noise". They get `c = 0`, so the
smoothness term decides their depth from trustworthy neighbors.

### 3.3 Edge-aware depth solve

Energy:

```text
E(d) = sum_x alpha * c(x) * (d(x) - d0(x))^2
     + lambda * sum_{x~y} w(x,y) * rho(d(x) - d(y))
```

- `rho` is Huber with `delta = 1.0` frame.
- `w(x,y) = exp(-|Y'(x) - Y'(y)|^2 / (2 * sigma_g^2))`, `sigma_g = 0.08`.
  This is the key to avoiding halos: depth is not smoothed across a ridge/sky
  boundary.
- `alpha = 1.0`, `lambda = 1.0` by default. Larger `lambda` = smoother.
- Solve coarse-to-fine: 1/8, 1/4, 1/1.
- Jacobi update per level:

```text
d(x) = (alpha*c*d0 + lambda * sum_j w(x,j) * d(j))
     / (alpha*c    + lambda * sum_j w(x,j))
```

- 30 iterations per level, or until the max update is below `1e-3` frames.
- Post-smooth with a Gaussian whose radius is half the estimation radius
  (Zerene's default relationship), then re-anchor confident pixels once.

Do not use a plain bilateral filter on the depth map as the only smoother; it
leaves plateaus. The data term plus edge-aware smoothness is what produces a
"rubber sheet" that hugs detail and stretches smoothly elsewhere.

### 3.4 Interpolation blend

Given the continuous depth `d`:

```text
i0 = floor(d), i1 = min(i0 + 1, N - 1), f = d - i0
low = (1 - f) * I_i0 + f * I_i1
```

Naive interpolation can create double edges when `i0` and `i1` see different
occluders. Refine:

- Split into low and high frequencies (Gaussian blur radius ~8 px).
- Take low frequencies from `low`.
- Take high frequencies from the sharper frame (`argmax_i E_i`).
- If `|Y_i0 - Y_i1| > 0.08` and margin confidence is high, use the argmax
  frame for the high frequencies outright.

This gives smooth transitions without visible double edges.

Depth map output: `d / (N-1) * 65535`, 16-bit grayscale TIFF
(`docs/outputs.md` §2).

---

## 4. Hybrid (default)

### 4.1 Detail transfer

1. Run DMap -> `D` (linear RGB).
2. Run PMax -> `P` (linear RGB).
3. Convert both to luma/chroma.
4. Build a guided base of `P`:

```text
B  = guided_filter(Y_P, Y_P, r=24, eps=1e-3)
Dp = Y_P - B
Dp = tau * tanh(Dp / tau)          tau = 0.10 (linear), soft clamp
Y_out = Y_D + k * Dp               k = 0.7
CbCr_out = CbCr_D
```

Rationale: DMap supplies color and tone from actual frames; PMax supplies
micro-detail that DMap's interpolation softened. The soft clamp prevents
overshoot halos at high-contrast edges.

Alternative (evaluate in S5.1): blend Laplacian pyramids of `D` and `P` with a
per-pixel mask, forcing coarse levels to `D`. Keep whichever wins the S5 gates
and document the choice.

### 4.2 Halo suppression

Three mechanisms, applied together:

1. **Low-pass anchoring:** for pyramid levels `>= L-2`, use DMap coefficients
   unconditionally. PMax never controls large-scale tone.
2. **Detail clamp:** §4.1's `tanh` clamp. `k <= 1.0`.
3. **Overshoot check:** measure local overshoot in a 5 px band around depth
   discontinuities. If it exceeds 2% of local contrast, reduce `k` (globally or
   locally) and re-run. Report the final `k`.

Do not use unsharp masking on the final image; it recreates the halos the
pipeline is designed to avoid.

### 4.3 Hybrid mask

Where should PMax detail be used?

```text
m = smoothstep(c_lo, c_hi, c) * smoothstep(t, 2t, Emax)
m = guided_filter(m, guide=Y_D, r=8, eps=1e-3)
```

- `c_lo = 0.10`, `c_hi = 0.30`.
- `t` is the DMap threshold from §3.2.
- `m = 0` in sky/water/low-confidence regions -> pure DMap.
- `m = 1` on confident, textured detail -> full PMax high frequencies.
- The guided filter aligns the mask with image edges, so detail transfer does
  not bleed across boundaries.

### 4.4 Method selection

`--method auto` (default):

1. Run DMap.
2. Compute `confident_pct` and the coverage percentage.
3. If `confident_pct < 0.40` -> DMap only (hybrid detail has nothing reliable
   to key on).
4. Otherwise -> hybrid.

Explicit `--method pmax|dmap|hybrid` always wins. Report the chosen method and
the reason in the JSON report.

---

## 5. Parameters

| Name | Default | CLI |
|---|---|---|
| Method | auto | `--method` |
| PMax temperature `beta` | 6 | `--pmax-beta` (debug) |
| DMap `lambda` | 1.0 | `--dmap-lambda` |
| DMap `sigma_g` | 0.08 | internal |
| Detail transfer `k` | 0.7 | `--detail-transfer` |
| Detail clamp `tau` | 0.10 | internal |
| Guided base radius | 24 px | internal |
| Mask `c_lo`, `c_hi` | 0.10, 0.30 | internal |
| Keep-detail override | auto | `--keep-detail` |
| Estimation radius | auto | `--estimation-radius` |

Expose only the parameters a photographer would tune: method, detail transfer,
estimation radius, keep-detail, exposure match. The rest are debug flags.

---

## 6. Exposure and color normalization

If frames differ in exposure (flash variation, aperture drift), normalize
before focus measurement:

- `--exposure-match off` (default): no change.
- `--exposure-match luma`: estimate a robust gain per frame from the median
  ratio in overlapping, unsaturated, high-gradient regions; apply to RGB.
- `--exposure-match rgb`: per-channel gains, for white-balance drift.

Do not use histogram matching by default; it distorts scene color. Report the
gains applied.

With constant-exposure Lightroom exports, leave this off.

---

## 7. Limitations

These are optical or informational, not bugs:

- **Occlusion haze:** a background pixel may never be recorded sharp because
  the out-of-focus foreground contributes light to it. No selection method can
  recover it. PMax detail helps; retouching is the honest fix.
- **Coverage gaps:** with 3–4 frames and a large step, some region is never
  sharp. Report it; do not invent detail.
- **Motion:** out of scope in v1 (tripod/static). A later motion mask would
  fall back to a single frame in affected regions.
- **No deconvolution:** we select sharp pixels; we do not restore detail that
  no frame contains.
- **Depth map is relative:** it is a frame index, not metric distance.
