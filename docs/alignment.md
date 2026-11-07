# Alignment

## 1. Model and defaults

The camera is on a tripod; the scene is static. Only focus changes between
frames. That produces:

- translation (camera settle, rail play),
- small rotation (tripod/rail),
- small scale change (**focus breathing**).

A 4-parameter similarity transform is the correct model:

```text
x' = a*x - b*y + tx
y' = b*x + a*y + ty
    where a = s*cos(theta), b = s*sin(theta)
```

Maps output coordinates to source coordinates. Stored as `f64`.

| Parameter | Default | Range |
|---|---|---|
| Max rotation | 3° | configurable |
| Scale range | 0.97–1.03 | configurable |
| Translation | full frame | — |
| Reference | frame with max gradient energy | `--reference` override |
| Residual warning | p95 > 0.3 px | report warning |
| Residual failure | p95 > 1.0 px | abort unless `--force` |

Homography and affine are **not** defaults. Tripod data does not justify the
extra degrees of freedom, and they can introduce shear that looks like
misalignment. Local warp is deferred (see §10).

## 2. Preprocessing

For each frame:

1. Build encoded luma at full resolution (`docs/io-and-color.md` §6).
2. Produce two alignment resolutions: long edge 1024 and 2048.
3. High-pass: `H = G - Gaussian(G, sigma=3)` to remove exposure and
   illumination differences.
4. Gradient magnitude `|grad G|` with `sigma=1`.
5. Build a mask:
   - `|grad G| > p20` (exclude flat sky/water),
   - `max(R,G,B) < 0.99` (exclude saturated),
   - `Y' > 0.01` (exclude crushed shadows).
6. Apply a Hann window before FFTs.

The reference frame is the one with the largest sum of gradient magnitude.
This is usually the foreground-focused frame and gives the most registration
information. Ties go to the middle index.

Alignment always uses encoded luma. It never uses linear light, because
highlights would dominate the correlation.

## 3. Translation via phase correlation

Given two same-size windows `f1`, `f2`:

```text
F1 = FFT(f1), F2 = FFT(f2)
R  = F1 * conj(F2) / (|F1 * conj(F2)| + eps)
r  = IFFT(R)
peak = argmax(r)
```

Subpixel refinement uses a separable parabolic fit on the 3×3 neighborhood:

```text
dx = 0.5 * (r[x-1] - r[x+1]) / (r[x-1] - 2*r[x] + r[x+1])
dy = 0.5 * (r[y-1] - r[y+1]) / (r[y-1] - 2*r[y] + r[y+1])
```

If the peak is within 1 pixel of the border, discard and retry at a coarser
level. Confidence is `peak / second_peak`; below 1.5, mark the estimate
unreliable and let ECC start from zero.

Coarse-to-fine: 1024 px -> 2048 px -> full resolution. At each level, refine
the previous estimate; do not search the full frame again.

The optional upsampled-DFT subpixel method (Guizar-Sicairos) is a fallback if
parabolic fitting fails the S1.2 gate. Keep it behind a flag until measured.

## 4. Rotation and scale via Fourier–Mellin

1. High-pass the luma, then take `M = log(|FFT(I)| + eps)`.
2. Resample `M` into log-polar coordinates:
   `rho = exp(u)`, `theta = v`.
3. Phase-correlate the two log-polar images.
4. Convert the peak `(du, dv)` to scale and rotation:
   `s = exp(du * du_scale)`, `theta = dv * dv_scale`.
5. Resolve the 180° ambiguity: test `theta` and `theta + pi`, warp a
   downsampled frame with each, and keep the one with the better normalized
   correlation.
6. Clamp to the allowed range (§1). If the estimate hits a clamp, warn.

This stage is coarse. It only needs to get within ~0.2° and ~0.2% so ECC can
converge.

## 5. ECC refinement

Minimize the robust photometric error over the 4-parameter similarity:

```text
E(p) = sum_x w(x) * rho( I_src(W(x; p)) - I_ref(x) )
```

- `rho` is Huber with `delta = 0.05` on normalized encoded luma.
- `w(x)` is the alignment mask from §2.
- Pyramidal inverse-compositional Lucas–Kanade, 5 levels.
- Max 50 iterations per level; stop when the update is below `1e-4` px.
- Initialization: Fourier–Mellin result, or identity if confidence is low.

Implementation notes:
- Precompute the steepest-descent images per level.
- Use bilinear sampling inside ECC; use Lanczos3 only for the final warp.
- Reject tiles whose robust residual exceeds 3× the median; these are usually
  moving branches or specular highlights.
- Report the final normalized correlation. Below 0.3, warn and continue.

Do not run ECC on linear light. Use encoded luma.

## 6. Warping

Final warps use the Lanczos3 kernel:

```text
L(x) = sinc(x) * sinc(x/3)   for |x| < 3, else 0
```

- Apply a pre-blur when minifying: if `s < 0.98`,
  `sigma = sqrt(1/s^2 - 1)` before sampling. This prevents aliasing.
- Edge handling: clamp to the nearest source pixel. Do **not** fill with black;
  black borders poison focus measures and blending.
- Parallelize over output rows. Precompute weights.
- Identity transform must be bit-exact (test in S1.5).

Because the transform is a similarity, sampling is a rotated grid. A strictly
separable two-pass implementation is not possible for arbitrary rotation; use a
scanline gather with separable x/y weights, or decompose into
shear–scale–shear passes. The gather is simpler and fast enough at 24 MP.

## 7. Valid region and cropping

1. Map the output rectangle corners through the inverse transform.
2. Compute the axis-aligned intersection with the source bounds.
3. Inset by the Lanczos radius (3 px) plus 1 px.
4. Crop all frames to the same rectangle; write output at that size.

Default is crop. `--keep-borders` keeps the full frame and edge-clamps; use it
only when the crop would remove important content.

Crop is applied after warping, before pyramid construction, so all downstream
buffers share one size.

## 8. Diagnostics

- Per-tile residual: 128 px tiles at half resolution, local phase correlation,
  displacement magnitude in full-res pixels.
- Report median and p95.
- Warn if p95 > 0.3 px; fail if p95 > 1.0 px unless `--force`.
- Report per-frame peak confidence and ECC correlation.
- A residual map preview (optional) helps diagnose wind or rail play.

Warnings are advisory; never silently crop away a misaligned frame.

## 9. Parameters

| Name | Default | CLI |
|---|---|---|
| Reference frame | max gradient energy | `--reference auto\|INDEX` |
| Max rotation | 3° | `--max-rotation DEG` |
| Scale range | 0.97–1.03 | `--scale-range MIN,MAX` |
| ECC levels | 5 | internal |
| ECC iterations | 50 | internal |
| ECC Huber delta | 0.05 | internal |
| Residual warn / fail | 0.3 / 1.0 px | `--max-residual PX` |
| Warp kernel | Lanczos3 | internal |
| Crop | on | `--keep-borders` |

## 10. Alternatives considered

| Approach | Why not default |
|---|---|
| Translation only | fails on focus breathing and tiny tripod rotation |
| Affine | adds shear/aspect that tripod data cannot identify; warps straight lines |
| Homography | overfits; useful only for handheld/parallax, deferred |
| Feature matching (SIFT/ORB) | more code, needs tuning for low-texture landscapes; phase correlation + ECC is sufficient here |
| Optical flow / local warp | can distort a rigid scene; deferred for handheld |
| Align after focus selection | selection depends on alignment; wrong order |
