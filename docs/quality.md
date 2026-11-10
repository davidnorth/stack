# Quality and validation

## 1. Philosophy

Professional quality is a claim that must be measured. The plan builds the
measurement harness in S0.6, before the stacking engines, so every later change
has a number attached.

Two kinds of evidence:

1. **Synthetic ground truth** — known scene, known depth, known blur, known
   misalignment. Objective metrics, fast enough for CI.
2. **Real stacks** — blind A/B against commercial outputs. Human judgment,
   slow, run at milestones.

Neither alone is sufficient. Synthetic catches regressions; real stacks catch
assumptions that synthetic scenes do not contain.

## 2. Synthetic stack generator

Inputs:
- an all-in-focus scene (linear RGB, `[0,1]`),
- a depth map `z(x,y)` (relative or metric),
- `N` focus distances `z_f`,
- a circle-of-confusion scale,
- noise and exposure parameters,
- optional known transforms.

Render:

1. Quantize depth into `K` layers (`K = 16` for small fixtures, 32 for larger).
2. For each layer and frame, compute the blur radius:

```text
c = coc_scale * |1/z - 1/z_f|
```

   Clamp to `[0, c_max]`. Use a disk PSF of radius `c` (FFT convolution) for
   realism, or a Gaussian with `sigma = c/1.5` for speed. Document the choice
   per fixture.
3. Composite layers front-to-back with feathered layer masks.
4. Add noise: Gaussian read noise plus Poisson shot noise.
5. Apply per-frame exposure gain.
6. Clip to `[0,1]` and quantize to 16-bit.
7. Optionally apply a known similarity transform (separate alignment fixtures).

Ground truth:
- the all-in-focus scene (clean, or with a known noise realization),
- the depth map,
- the exact transforms and exposure gains,
- the generator config.

Config (TOML):

```toml
width = 512
height = 512
frames = 4
seed = 42
coc_scale = 0.004
focus = [0.20, 0.35, 0.55, 0.90]
noise_read = 0.002
noise_shot = 0.001
exposure = [1.0, 1.0, 1.0, 1.0]
layers = 16
psf = "disk"
```

Determinism: all randomness from `seed`. No wall-clock, no unseeded RNG.

Fixture sizes:
- 512×512 for unit tests and CI (seconds).
- 2048×1365 for milestone validation.
- The generator is not used to produce 24 MP fixtures in CI; use real crops.

## 3. Metrics

All metrics are computed on the output versus ground truth, in linear light
unless stated otherwise.

| Metric | Definition | Target |
|---|---|---|
| PSNR | `10 log10(1 / MSE)` on luma `[0,1]` | ≥ 38 dB |
| SSIM | standard 11×11 Gaussian, luma | ≥ 0.98 |
| Detail recovery | `sum |grad out| / sum |grad truth|` over GT-sharp mask | 0.95–1.10 |
| Halo overshoot | max deviation beyond GT in a 5 px band around depth edges, normalized by local contrast | ≤ 0.02 |
| Noise gain | std in GT-flat regions / std of sharpest input there | ≤ 1.10 |
| Color error | mean ΔE00 in GT-flat regions, D65 | ≤ 1.0 |
| Alignment RMSE | known transform vs estimated, control points | ≤ 0.15 px |
| Coverage loss | % pixels with `|grad out| < 0.5 * |grad truth|`, excluding GT-flat | < 2% |

Notes:
- The GT-sharp mask is `|grad truth| > p70`; flat regions are the complement.
- Halo overshoot uses a signed local difference; take the 99th percentile of
  absolute overshoot to avoid single-pixel outliers.
- ΔE00 uses the CIEDE2000 formula; implement and test against published pairs.
- Coverage loss is the metric that catches focus-step mistakes.

`focus-stack eval --truth <TIFF> --test <TIFF>` prints the table and can emit
JSON for CI.

## 4. Fixtures and CI

Directories:

```text
fixtures/
  io/            TIFF conformance files (small, committed)
  synthetic/     generator configs + tiny outputs (committed)
  real/          10 landscape stacks (fetched; 100% crops committed)
  manifest.toml  fixture list, expected metrics, tolerances
```

Local gates (this project runs no hosted CI):
- `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test` before each commit.
- Tiny regression suite (< 2 min): I/O round-trip, alignment, one 512×512
  synthetic stack per method, report schema.
- On demand: full synthetic suite and 2 MP fixtures.
- Release: full real-stack A/B is manual; the fixture loader is checked locally.

Tolerance policy:
- PSNR ±0.2 dB, SSIM ±0.001, detail ±0.01, halo ±0.002, noise ±0.02,
  ΔE00 ±0.1.
- A regression beyond tolerance fails the PR unless the PR explicitly updates
  the expected metrics and explains why.
- Golden image hashes are used only for determinism tests, never for quality.

## 5. Blind A/B protocol

At S5.4 (tuning set) and S7.3 (held-out set). Never tune and validate on the
same stacks.

1. Select 10 real landscape stacks per set (3–4 frames, 24 MP) covering:
   near/far, sky, water, foliage, a hard ridge line, low contrast, high detail.
2. Produce outputs: Photoshop Auto-Blend, Helicon Focus, Zerene PMax, Zerene
   DMap, ours (PMax, DMap, Hybrid).
3. Anonymize filenames and strip metadata.
4. Show two raters: full frame, plus 100% crops of near, mid, and far regions.
5. Score 1–5 for sharpness, artifacts, naturalness, and color.
6. Record medians and the artifact list in `docs/quality-log.md`.

Targets:
- Hybrid median ≥ Helicon median − 0.5.
- Hybrid median > Photoshop median on artifacts and naturalness.
- No catastrophic failure (ghosting, double edges, color blotches) on any
  stack; if one occurs, it becomes a tracked issue.

Do not tune on the test stacks and then report the same stacks as validation.
Use a held-out set for the final numbers.

## 6. Performance

Targets (Apple Silicon, release build):

| Case | Target |
|---|---|
| 4 × 24 MP, hybrid, total | ≤ 8 s |
| 4 × 24 MP, peak RSS | ≤ 4 GB |
| 4 × 60 MP, hybrid, total | ≤ 25 s |
| 4 × 60 MP, peak RSS | ≤ 8 GB (tiled if needed) |

Stage budgets at 24 MP:
- load: 0.5 s
- align: 0.5 s
- focus: 1.0 s
- stack: 3.0 s
- encode: 0.5 s

Measure with `criterion` for CPU stages and `peak RSS` (e.g., `/usr/bin/time -l`)
for end-to-end runs. Record in `docs/performance.md`. Regressions above 20%
should be explained in the PR.

## 7. Competitive baseline

| Capability | Photoshop Auto-Blend | Helicon Focus | Zerene Stacker | This project (target) |
|---|---|---|---|---|
| Alignment | projective; unreliable at high magnification | translation/rotation/scale | translation/rotation/scale + brightness | subpixel similarity + focus-breathing scale |
| Engines | masked pyramid blend | A weighted, B pyramid, C depth | PMax + DMap | PMax + DMap + automatic hybrid |
| Mask generation | single feathered contrast mask | local contrast | contrast threshold + depth | noise-aware multi-scale + confidence |
| Halos at depth edges | common | reduced | reduced, retouch often needed | edge-aware depth + low-pass anchoring |
| Noise in sky/flat | blotchy if threshold wrong | moderate | DMap good, PMax noisy | noise-floor subtraction + confidence |
| Color/tone | can shift | good | DMap best | linear blend + DMap low-pass |
| Depth map | no | yes | yes | yes (16-bit) |
| Retouch | brush layers | brush | brush | scriptable JSON/PNG masks + source map |
| CLI / automation | no native CLI | limited | limited | first-class, deterministic |
| 16-bit + ICC | yes | yes | yes | yes, ICC preserved |
| Validation | none public | internal | internal | synthetic GT + blind A/B |

The honest positioning: match Helicon/Zerene on typical 3–4 frame landscape
stacks, clearly beat Photoshop on alignment, halos, and sky noise, and match
retouched commercial results only where the optics allow it. Occlusion haze and
coverage gaps remain physical limits.
