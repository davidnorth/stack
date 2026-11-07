# Session log

Append-only. One entry per completed (or blocked) session. Newest at the bottom.

Format:

```markdown
## S<ID> — YYYY-MM-DD

- Scope:
- Changed:
- Evidence:
- Decisions:
- Follow-ups:
```

---

## S0.1 — 2026-11-07

- Scope: repository scaffold and CI
- Changed: `Cargo.toml`, `rust-toolchain.toml`, `src/main.rs`, `src/lib.rs`, module skeleton under `src/`, `.github/workflows/ci.yml`, `.gitignore`, `LICENSE`
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (2 passed)
- Decisions: pinned Rust 1.99.1; MIT license; module tree mirrors `docs/architecture.md` §2; no runtime dependencies yet; CI runs on `macos-15` (arm64) and asserts `uname -m`
- Follow-ups: confirm the GitHub Actions run is green after the loop pushes

## S0.2 — 2026-11-07

- Scope: TIFF reader
- Changed: `Cargo.toml`, `Cargo.lock`, `src/io/tiff_read.rs`, `src/io/exif.rs`, `tests/io_read.rs`, `fixtures/io/*`
- Evidence: `cargo fmt --all --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test` (11 unit + 11 integration passed); fixtures cover 8/16-bit RGB, strips/tiles, none/LZW/Deflate/PackBits, predictor 2, BigTIFF, ICC bytes, orientation 1/6/8, and grayscale; CMYK/planar/LZMA/truncated inputs fail with named errors
- Decisions: D011 `tiff` crate with only `deflate` and `lzw` features; orientation is baked upright while `FrameMeta.orientation` keeps the source tag; chunky unsigned 8/16-bit only, alpha dropped (associated alpha unpremultiplied); EXIF payload copy deferred to S6.2
- Follow-ups: float TIFF and planar-separate decoding remain unsupported with clear errors; a real 24 MP Lightroom export still needs `scripts/fetch_fixtures.sh`

## S0.3 — 2026-11-07

- Scope: TIFF writer and round-trip
- Changed: `src/io/tiff_write.rs`, `src/io/metadata.rs`, `tests/io_roundtrip.rs`, `src/lib.rs`, `src/io/tiff_read.rs` (resolution fields required for the copy policy)
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (13 unit + 11 read + 6 round-trip passed); all orientation=1 fixtures round-trip pixel- and ICC-identical; macOS ImageIO (`sips`) opened a generated 16-bit LZW output with the embedded sRGB profile
- Decisions: classic stripped TIFF, `Orientation = 1`, `SampleFormat = Unsigned`, default LZW and 16-bit; none/LZW/Deflate selectable; 8-bit output supported for the documented `--bits 8` round trip; metadata policy copies ICC/XMP/resolution and strips XMP with `Strip`; EXIF remains deferred to S6.2
- Follow-ups: Lightroom/Photoshop GUI opening was not available on this machine; ImageIO validation used as the automated proxy. EXIF/IPTC/Software reporting lands in S6.2.

## S0.4 — 2026-11-07

- Scope: ICC detection, linear sRGB working space, encoded luma, output profile conversion or preservation
- Changed: `Cargo.toml`, `Cargo.lock`, `src/color/transfer.rs`, `src/color/luma.rs`, `src/color/profile.rs`, `tests/color.rs`
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (21 unit + 32 integration passed; 15 color tests); sRGB decode→encode within 1e-6; luma matches the 0.2126/0.7152/0.0722 reference; known/unknown/absent profiles, `--assume-profile`, explicit output profile, and ICC-byte preservation covered
- Decisions: bundled `lcms2` static build (D012); detection by ICC description with colorant/TRC fallback; sRGB and linear sRGB use the exact internal transfer functions, wide-gamut uses lcms2 relative colorimetric; an absent profile assumes sRGB and exposes a warning per `docs/io-and-color.md` §6 (D012's stricter absent-profile wording is inconsistent and should be reconciled); unknown profiles error unless assumed; output preserves the input ICC bytes unless an explicit output profile is supplied
- Follow-ups: S0.5 must wire `--assume-profile` / `--output-profile` file paths into `ColorPipeline`; clipping accounting and `--output-profile same` CLI plumbing belong there; ICC fixtures for Adobe RGB/ProPhoto/P3 are generated in tests rather than committed

## S0.5 — 2026-11-07

- Scope: CLI skeleton and load→copy→write pipeline stub
- Changed: `Cargo.toml`, `Cargo.lock`, `src/cli.rs`, `src/pipeline.rs`, `src/main.rs`, `tests/cli.rs`
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (27 unit + 44 integration passed; 12 new CLI tests); identity/multi-input runs produce readable TIFFs; missing/corrupt/mismatched inputs exit 3; usage errors exit 2
- Decisions: clap derive for the documented option set; stub validates equal dimensions and bit depth, then copies the selected reference frame; `--reference auto` selects index 0 until gradient selection lands; report is a deterministic minimal JSON stub; progress is written to stderr only on a terminal or with `-v`
- Follow-ups: `--depth-map`/`--source-map`/`--preview` are parsed but not written yet; `--assume-profile`/`--output-profile` still need `ColorPipeline` wiring; retouch/eval subcommands land in S6.3/S7

## S0.6 — 2026-11-07

- Scope: deterministic synthetic stack generator with ground truth, TOML configs, and committed fixtures
- Changed: `Cargo.toml`, `Cargo.lock`, `src/quality/synthetic.rs`, `tests/synthetic.rs`, `fixtures/synthetic/{stack_512,gaussian_64,transforms_96}.toml`
- Evidence: `cargo fmt --all --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (36 unit + 50 integration passed; 9 synthetic unit + 6 synthetic integration); delta responses match disk `E[r²]=c²/2` and Gaussian `E[r²]=2σ²`; same seed is bit-identical, different seed differs; 512×512 fixture renders with radii equal to `coc_scale·|1/z−1/z_f|` clamped to `c_max`
- Decisions: added `serde` + `toml` for the documented TOML input; procedural scene/depth (far background plus two subject blobs) keeps layer bounding boxes small, so the 512×512 fixture renders in ~10 s in debug; layers are quantized with feathered linear weights and composited front-to-back; disk PSF is an exact discrete convolution with fractional radii interpolated between neighbouring integer kernels; Gaussian uses `sigma = c/1.5`; shot noise uses `g = noise_shot²` photon scale and a normal approximation above λ=30; known transforms are applied after defocus and map output coordinates to source coordinates (positive `tx` moves content left), matching `Similarity`
- Follow-ups: generator is not yet wired into the CLI/eval command; milestone-sized (2048×1365) rendering and per-fixture expected metrics belong to S7; disk convolution is direct rather than FFT, revisit if large fixtures become slow

## S1.1 — 2026-11-07

- Scope: alignment preprocessing — encoded luma, capped downsampling, high-pass, gradient/colour masks, Hann window
- Changed: `src/align/preprocess.rs`, `tests/align_preprocess.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (36 unit + 50 integration passed; 9 new preprocess tests); flat sky, saturated pixels, and crushed shadows excluded in synthetic cases; level buffers capped by the requested long edge
- Decisions: API takes full-resolution encoded interleaved RGB `f32`; only full-res luma plus a 1-byte colour-validity mask are retained, and each level is built directly from them (conservative validity: any invalid source pixel invalidates the destination); p20 threshold uses a histogram rather than a sorted copy; targets larger than the source collapse to native size
- Follow-ups: S1.2 consumes `AlignmentLevel`; reference-frame selection from gradient energy and pipeline wiring remain for later sessions

## S1.2 — 2026-11-07

- Scope: phase-correlation translation — cross-power spectrum, peak search, parabolic subpixel refinement
- Changed: `src/align/phase.rs`, `tests/align_phase.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (42 unit + 65 integration passed); `cargo test align_phase` (6 passed) recovers all integer and 0.25 px shifts within 0.05 px; non-power-of-two impulse test covers FFT padding
- Decisions: self-contained radix-2 FFT (no new dependency); axes zero-padded to the next power of two; cross-power normalisation uses a 1e-6 relative floor, because f32 rounding noise otherwise dominates near-zero bins; subpixel estimate is peak / second peak outside the 5×5 neighbourhood; Guizar-Sicairos upsampled DFT deferred (parabolic fit met the gate); `fft2d`/`Complex` are `pub(crate)` for S1.3 reuse
- Follow-ups: S1.3 can reuse the crate-visible 2D FFT; coarse-to-fine level selection and `AlignmentLevel` wiring remain for S1.4/S1.6

## S1.3 — 2026-11-07

- Scope: Fourier–Mellin rotation and scale — high-pass log magnitude, log-polar transform, phase correlation, 180° disambiguation, range clamps
- Changed: `src/align/logpolar.rs`, `tests/align_logpolar.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (45 unit + 68 integration passed); `cargo test --test align_logpolar` (3 passed) recovers ±2° and ±1.5% scale within 0.1°/0.1%, clamps out-of-range estimates, and accepts non-power-of-two sample counts
- Decisions: reuse the crate-visible radix-2 FFT; apply a radial Hann window before the high-pass so the window's low-frequency lobe is removed and rotation equivariance is preserved; start log-polar sampling at 7% of the FFT half-size to skip the high-pass transition; resolve the 180° ambiguity by normalized correlation of both candidate warps; clamp scale/rotation to §1 defaults with explicit flags
- Follow-ups: FM confidence is dominated by the 180° twin peak, so S1.4 should use its own final correlation for the low-confidence identity fallback


## S1.4 — 2026-11-07

- Scope: ECC similarity refinement — 4-parameter similarity, 5-level inverse-compositional Lucas–Kanade, Huber weights, masks, tile rejection, convergence
- Changed: `src/align/ecc.rs`, `tests/align_ecc.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (53 unit + 73 integration passed; 5 new ECC tests); identity start, 5% outlier pixels, and perturbed coarse start all recover the synthetic similarity within the <0.05 px gate; masked occlusion test passes
- Decisions: keep the full-resolution `Similarity` while processing each level, converting to/from level coordinates with the 2×2 box-downsample centre offset; templates use central-difference gradients and per-level steepest-descent images, while robust IRLS weights rebuild the 4×4 normal equations each iteration; the linear solve returns additive increments, converted to the group element `(1+da, db, dtx, dty)` before inverse composition; Huber delta 0.05, 16 px tiles with 3× median rejection (majority kept), ridge-damped 4×4 Gaussian elimination, edge-clamped bilinear sampling; final Pearson correlation below 0.3 sets `correlation_warns`
- Follow-ups: pipeline wiring and coarse-to-fine level feeding (`AlignmentLevel` 1024→2048) remain for S1.6; tile-level residual diagnostics belong to S1.6; final warp/crop is S1.5

## S1.5 — 2026-11-07

- Scope: Lanczos3 warp and valid-rect crop — linear-light separable gather, anti-alias pre-blur on minification, edge clamp, rayon rows
- Changed: `Cargo.toml`, `Cargo.lock` (added `rayon`), `src/align/warp.rs`, `tests/align_warp.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (all suites pass, 12 new warp tests + 1 ignored perf gate); release perf gate `cargo test --release --test align_warp warp_24mp_under_half_second -- --ignored` measured 24 MP RGB warp in 0.29 s (< 0.5 s); identity warp is bit-exact; 90° round-trip error ≤ 1 LSB; constant-image rotation + crop has no dark border
- Decisions: interleaved linear-light `f32` with an explicit channel count; one 2D gather with separable x/y Lanczos3 weights (supports rotation and avoids shear passes); 2048-entry subpixel table with linear interpolation, with exact integer taps so integer-aligned transforms stay bit-exact; pre-blur threshold uses the source-to-output scale `s` from §6, i.e. `1/hypot(a,b) < 0.98`, with `sigma = sqrt(1/s² − 1)` in source pixels; `valid_rect` solves the four source-space half-planes for the largest centred axis-aligned rectangle; crop inset is `LANCZOS_RADIUS + 1 = 4`
- Follow-ups: pipeline wiring of warp + `common_valid_rect` + crop belongs to S1.6; performance is a single measured release run, formal benchmarks land in S7.4

## S1.6 — 2026-11-07

- Scope: per-tile residual/NCC diagnostics, median/p95 aggregation, warning/failure thresholds, alignment report fields
- Changed: `src/align/diagnostics.rs`, `src/output/report.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + 68 integration passed; 6 new diagnostics unit tests + 3 new report unit tests); synthetic 0.2 px half-res shift reports p95 0.40 px (default 2× scale); 0.4 px full-res p95 warns; 1.2 px p95 fails unless `force`
- Decisions: diagnostics take already-warped source/reference luma at half resolution; 128 px tiles with local phase correlation, displacement scaled by `full_res_scale` (default 2); per-tile NCC is Pearson correlation after removing the measured displacement; low-confidence tiles are reported but excluded from medians; p95 uses linear interpolation; report alignment object keeps schema order (`reference_index`, `transforms`, `residual_px`, `correlation`, `peak_confidence`, `warnings`) and emits `null` for non-finite values
- Follow-ups: pipeline wiring of warp/crop and `AlignmentReport` still pending; `--report` continues to write the S0.5 inline stub

## S2.1 — 2026-11-07

- Scope: Gaussian/Laplacian pyramid in f32, level count policy, buffer reuse
- Changed: `src/focus/pyramid.rs`, `tests/focus_pyramid.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 5 new pyramid tests); reconstruction max error < 1e-6; level count 6 at 6000×4000 and 7 at 9504×6336; measured 1024×768 at 9.49 MiB (3.16× base), 24 MP projected at 289.86 MiB
- Decisions: downsample keeps even samples after the separable `[1,4,6,4,1]/16` blur; upsample is bilinear with source coordinate `x / 2` and edge clamp, shared by analysis and reconstruction; §2's `L` counts bands, so the stored level count is `L + 1` including the residual, matching the documented 6/7 examples; `build` reuses level and blur buffers and subtracts the upsample directly, avoiding a full-size temporary
- Follow-ups: S2.2 consumes `Pyramid::levels()` Gaussian/Laplacian buffers; mapping to the shared `FocusPyramid` shape can happen when the pipeline is wired

## S2.2 — 2026-11-07

- Scope: per-level noise sigma via MAD on a low-gradient mask, noise floor
- Changed: `src/focus/noise.rs`, `tests/focus_noise.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 5 new noise tests); synthetic per-level sigma within 5% at 0.01/0.02/0.04; textured half does not inflate the 0.012 estimate
- Decisions: gradient magnitude is central-difference `hypot` on `G_l`; mask uses the strict `< p20` from §3 with an `<=` tie fallback so flat regions with zero gradient still contribute; MAD is centred on the median, which equals `1.4826 * median(|Lap|)` for zero-mean bands and keeps the coarsest residual from reporting its brightness as noise; sigma clamps to `1e-6`; expose `NoiseModel` per level plus `noise_floor`/`subtract_noise_floor` for S2.3
- Follow-ups: JSON report wiring of per-frame/per-level sigma remains with the report task; mask/median allocation can be pooled with the focus workspace later if profiling shows it

## S2.3 — 2026-11-07

- Scope: box energy via summed-area tables, radius schedule, contrast normalization, cross-level aggregation weights
- Changed: `src/focus/energy.rs`, `tests/focus_energy.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 9 new energy tests + 1 ignored perf gate); box filter matches brute force for radii 0–20; auto radius 12 at 24 MP and 17 at 60 MP; blur ladder (sigma 0/1/2/4) strictly decreases both finest and aggregated energy; release gate `cargo test --release --test focus_energy four_frame_24mp_energy_pass_under_one_second -- --ignored` measured 0.604 s (< 1 s)
- Decisions: box windows clip at borders and divide by the actual area; SAT is built in `f64` for cancellation-safe differences and box rows are rayon-parallel; `EnergyOptions` carries optional `r0` plus `--contrast-normalize`; aggregation excludes the coarsest residual (`Lap_L = G_L`) and applies the §6 weights to band levels 0..L-1, bilinearly upsampling with the pyramid's `x / 2` mapping; public `energy_map`/`box_filter`/`aggregate_levels` are exposed for S2.4 and the stacker
- Follow-ups: pipeline wiring and the CLI `--estimation-radius` / `--contrast-normalize` plumbing remain with the focus-measure integration task; aggregation is measured within the 4-frame gate alongside map computation

## S2.4 — 2026-11-07

- Scope: best-vs-second margin confidence, guided smoothing, coverage mask with bounding boxes, saturation exclusion, per-frame noise calibration and energy statistics
- Changed: `src/focus/confidence.rs`, `src/focus/coverage.rs`, `tests/focus_confidence.rs`, `src/focus/energy.rs` (required aggregation fix)
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 10 new confidence/coverage tests); synthetic textured-region confidence > 0.30 with deep flat sky < 0.01; blur-ladder frame ranking accuracy well above 95%; coverage gap fraction/boxes and winning-frame calibration verified
- Decisions: confidence is `clamp((E1-E2)/(E1+eps),0,1)` smoothed by the O(1) guided filter (`r=8`, `eps=1e-3`), then zeroed at coverage gaps (`E1 < 4*sigma^2`) and saturated winner pixels (`any channel >= 0.995`); `FrameCalibration` derives an §6-weighted aggregate sigma over band levels (residual excluded) plus min/max/mean/median/p95 statistics and deliberately does not rescale energies (§4); coverage reports gap fraction and deterministic 8-connected bounding boxes; `src/focus/energy.rs` `accumulate_upsampled` now uses each level's actual source/destination ratio instead of a hard-coded `x/2`, which previously clamped coarse levels at the halfway point and leaked texture into distant sky
- Follow-ups: pipeline/report wiring of confidence, coverage warnings, saturation masks, and per-frame statistics remains with later focus-measure integration; noise-floor bias on pure-noise skies is inherited from S2.2 and should be revisited if real-stack coverage proves too permissive


## S3.1 — 2026-11-07

- Scope: PMax level weights — per-level p99 energy normalization, temperature softmax, confidence gating toward uniform, chroma-smoothed weights
- Changed: `src/stack/pmax.rs`, `tests/stack_weights.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 8 new weight tests); weights and chroma weights sum to 1 within 1e-5; flat/zero and NaN/infinite energies stay finite and uniform; c=0 yields exact uniform weights and c=1 yields softmax selection
- Decisions: energies are supplied as per-frame `EnergyMaps`; coarser levels sample the nearest full-resolution confidence at `x * 2^l`; p99 uses a 4096-bin streaming histogram over finite non-negative samples so 24 MP stacks need no full-energy sort; softmax subtracts the per-pixel max logit for overflow safety; chroma is `0.5*w + 0.5/N`; `level_weights` exposes the per-level core for S3.2 and tests
- Follow-ups: S3.2 consumes `LevelWeights` for Laplacian blending and coarse-level tone anchoring; beta stays internal at 6 until the `--pmax-beta` debug flag is plumbed

## S3.2 — 2026-11-07

- Scope: PMax Laplacian coefficient blend, coarse-level tone anchoring, reconstruction, `--method pmax` pipeline integration
- Changed: `src/stack/pmax.rs`, `src/pipeline.rs`, `tests/stack_pmax.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 3 new PMax tests); PMax SSIM beats naive Laplacian-max on the synthetic two-frame ridge; sky RMS noise below naive and below 0.02; CLI PMax run writes a readable 16-bit TIFF
- Decisions: blend luma with softmax weights and chroma with the smoother chroma weights; force the coarsest two levels to uniform weights for tone anchoring; PMax reconstruction keeps the analysis pyramid's bilinear `x/2` upsample and is verified against `Pyramid::reconstruct`; pipeline PMax computes encoded-luma focus pyramids, noise, energies and confidence, then decodes in place to linear RGB for blending; `--pmax-beta` now reaches the stacker
- Follow-ups: auto/hybrid method selection and alignment wiring remain with S5.3; pipeline still copies the reference for non-PMax methods

## S4.1 — 2026-11-07

- Scope: cost volume from aggregated per-frame energies, confidence-gated argmax, initial depth field, flat-region flags
- Changed: `src/stack/dmap.rs`, `tests/stack_dmap_cost.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 5 new cost/argmax tests); synthetic three-band depth recovered exactly at ≥95% of pixels and within one frame everywhere; tied energies and zero confidence flag flat
- Decisions: `CostVolume` stores frame-major aggregated energies (larger = stronger focus) with `Emax - E` exposed as the derived cost; `argmax_depth` selects `d0 = argmax_i E_i` with lowest-index tie-breaking and carries optional `ConfidenceMap` values into `DepthField::confidence`; flat is `Emax <= eps`, relative best-vs-second margin <= `FLAT_MARGIN`, or confidence zero; non-finite energies/confidence sanitize to zero; automatic threshold remains S4.2
- Follow-ups: S4.2 supplies the Otsu/noise-floor confidence ramp and threshold reporting; S4.3 uses `d0` and the flat mask in the edge-aware solve

## S4.2 — 2026-11-07

- Scope: automatic DMap contrast threshold from the log-energy histogram and focus noise floor, `--keep-detail` quantile override, confidence ramp, threshold reporting
- Changed: `src/stack/dmap.rs`, `tests/stack_dmap_threshold.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 5 new threshold tests); synthetic sky/texture split keeps sky confidence at 0 and texture > 0.30; normalized threshold within 5% across three scaled fixture stacks; noise floor and cap bounds verified
- Decisions: `ContrastThreshold` reports `threshold`, Otsu value, `k*sigma^2` floor, `0.1*p99` cap, override percent, confident percentage, and coverage percentage; Otsu runs over a 256-bin `log(Emax+eps)` histogram; automatic `t = min(max(otsu, floor), cap)`, while `--keep-detail` uses the `(1-PERCENT)` quantile capped the same way; the ramp is `smoothstep(t, 1.5*t, Emax) * margin` and zeroes confidence below `t`; coverage uses the focus module's `4*sigma^2` floor
- Follow-ups: pipeline `--method dmap` wiring and the JSON report object remain with S4.4/S6.1; `--keep-detail` is parsed by the CLI but reaches DMap once the engine is wired

## S4.3 — 2026-11-07

- Scope: edge-aware depth solve — guide-weighted neighbours, coarse-to-fine Jacobi, Huber, Gaussian post-smooth, re-anchor
- Changed: `src/stack/dmap.rs`, `tests/stack_dmap_solve.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 5 new solve tests, 1 ignored); flat unconfident bands smooth to <0.75-frame steps; guide ridge holds a >2.5-frame jump while a flat guide blends; convergence below 1e-3; `cargo test --release --test stack_dmap_solve -- --ignored` (24 MP in 1.01 s, gate 2 s)
- Decisions: `solve_depth` takes a caller-supplied guide luma and operates on `InitialDepth::field`; edge weights use a 1024-entry linear-interpolated exp LUT over [0,16]; pyramid is 1/8→1/4→1/1 with confidence-weighted depth downsampling; Huber is a clamped neighbour difference inside the Jacobi step; post-smooth is skipped when every pixel is confident, then positive-confidence pixels are re-anchored to `d0`
- Follow-ups: S4.4 supplies the real luma guide and consumes `SolvedDepth`; pipeline wiring remains with S4.4

## S4.4 — 2026-11-07

- Scope: depth interpolation blend, edge-aware hard selection, 16-bit `--depth-map` writer, `--method dmap` end to end
- Changed: `src/stack/dmap.rs`, `src/output/depth.rs`, `src/pipeline.rs`, `tests/output_depth.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 6 new depth/output tests); synthetic ridge shows two rising edges from the naive blend and one from the interpolation; depth map decodes to the original frame positions; CLI `--method dmap --depth-map` writes readable 16-bit RGB + grayscale TIFFs
- Decisions: low frequencies come from the `i0`/`i1` blend, high frequencies from the sharper bracketing frame; when the bracket luma gap exceeds 0.08 and confidence >= 0.5 the global argmax frame supplies the high frequencies; depth encodes as `round(d/(N-1)*65535)` with LZW grayscale and the documented `ImageDescription`, no ICC; single-frame maps are all zeros and warn; `--source-map` remains an alias; `--depth-map` with a non-DMap method solves the DMap depth solely to write the map
- Follow-ups: auto/hybrid method selection and the full JSON report remain with S5/S6

## S5.1 — 2026-11-07

- Scope: guided PMax detail transfer onto DMap, soft clamp, YCbCr recombination
- Changed: `src/stack/hybrid.rs`, `tests/stack_hybrid.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 6 new hybrid tests); native-resolution synthetic detail recovery inside the 0.95–1.10 gate with the soft DMap baseline below 0.95; added luma bounded by `k*tau`; output chroma matches DMap
- Decisions: reuse `focus::confidence::guided_filter` for the §4.1 base (`r=24`, `eps=1e-3`); transfer `k*tau*tanh(Dp/tau)` on Rec.709 luma only; carry DMap `Cb=B-Y`/`Cr=R-Y` and clamp final RGB; sanitize `k` to `[0,1]`; chose the guided-base/YCbCr variant, leaving the Laplacian-mask alternative to the S5.2 pyramid path
- Follow-ups: S5.2 adds low-pass anchoring and overshoot diagnostics; S5.3 wires the hybrid into method selection; pipeline integration was not touched here

## S5.2 — 2026-11-07

- Scope: halo suppression via DMap low-pass anchoring, gradient-domain/pyramid blend, edge-band overshoot diagnostics
- Changed: `src/stack/hybrid.rs`, `tests/stack_halo.rs`
- Evidence: `cargo fmt --check` clean; `cargo clippy --all-targets -- -D warnings` clean; `cargo test` (62 unit + all integration passed; 10 new halo tests); synthetic ridge halo normalized overshoot DMap 0.3750, PMax 0.7500, unsuppressed hybrid 0.3982, suppressed 0.0000, effective k 0.0231
- Decisions: `pyramid_anchor` reconstructs only the coarsest `anchor_levels` Laplacian bands and `anchor_coarse` swaps those bands from DMap unconditionally; the edge-band clamp soft-limits high frequencies against a box low-pass anchor over a feathered 5 px band; `halo_overshoot` measures values leaving the reference local range, normalised by local contrast and reduced with p99; `stack_hybrid_halo` returns `rgb`, suppressed `luma`, residual `overshoot`, and `k_effective`
- Follow-ups: S5.3 chooses/smooths the hybrid mask and wires halo suppression into method selection and the pipeline; S5.4 tunes the anchor/limit constants against real stacks
