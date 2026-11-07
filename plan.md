# focus-stack — implementation plan

Rust CLI that focus-stacks 3–4 16-bit TIFF frames exported from Lightroom.
Target: macOS Apple Silicon. Inputs are tripod captures of static scenes.

**One checkbox = one agent session (~1–3 h).** Technical detail lives in `docs/`; read only the section linked by the task.

**Automated:** `./scripts/ralph.py` runs one fresh session per checkbox, verifies the gates, and commits each stage. See `docs/ralph.md`.

---

## Scope

**v1**
- 16-bit RGB TIFF in/out (8-bit accepted), embedded ICC preserved.
- Automatic alignment: subpixel translation, rotation, and scale (focus breathing).
- Three engines: PMax, DMap, and Hybrid (default).
- Depth/source map output, JSON quality report, 8-bit JPEG preview.
- Scriptable retouch from stroke/mask JSON.
- Deterministic, reproducible runs.

**Deferred**
- `inspect` pre-flight command (coverage and redundancy warnings before stacking).
- Motion/deghost masking (v1 is tripod/static only).
- Local (non-rigid) alignment.
- HDR/exposure blending, lens corrections, panorama.
- GUI, GPU acceleration.
- Windows/Linux builds.
- Tiled processing for >40 MP (only if S7.5 proves necessary).

---

## Quality targets

| Metric | Target | Measured in |
|---|---|---|
| PSNR vs synthetic ground truth | ≥ 38 dB | S7.1 |
| SSIM (luma) | ≥ 0.98 | S7.1 |
| Detail recovery in GT-sharp regions | ≥ 0.95 | S7.1 |
| Halo overshoot at depth edges | ≤ 2% local contrast | S7.1 |
| Noise gain in flat regions | ≤ 1.10 | S7.1 |
| Color error ΔE00 in flat regions | ≤ 1.0 | S7.1 |
| Alignment residual p95 | ≤ 0.3 px | S1.6 |
| Runtime, 4 × 24 MP, hybrid | ≤ 8 s on M1 | S7.4 |
| Peak RAM, 4 × 24 MP | ≤ 4 GB | S7.4 |
| Visible seams in 100% crops | none | S5.4 |

---

## How to run a session

1. Pick the next unchecked box (respect dependencies).
2. Read only: this box + the linked doc section. Do not read the whole repo.
3. Implement, test, measure.
4. Update the checkbox and append an entry to `docs/session-log.md`.
5. If scope grows, split the box into sub-boxes instead of continuing.

**Definition of done (every session):**
- `cargo fmt --check` and `cargo clippy -- -D warnings` pass.
- New behavior has tests; the task's gate command passes.
- Public behavior is documented in the linked doc if it changed.
- No unrelated files touched.

**Dependency graph:** `S0 → S1 → S2 → {S3, S4} → S5 → S6 → S7 → S8`
- S7.1 may start after S0.6.
- S7.2 may start after S3.2.
- S7.3 depends on S5.4 (initial tuning) and S6.
- S8 may start after S6.
S5.4 is *initial* tuning; S7.3 is the held-out formal validation. Do not reuse
the same stacks as tuning and validation sets.

---

## S0 — Foundation and I/O (2–3 days)

- [x] **S0.1 Repository scaffold and CI**
  - Scope: Cargo project, module skeleton, pinned toolchain, fmt/clippy, macOS arm64 CI, `git init`.
  - Read: `docs/architecture.md` §1–3; `docs/agent-workflow.md`.
  - Touch: `Cargo.toml`, `rust-toolchain.toml`, `src/main.rs`, `src/lib.rs`, `.github/workflows/ci.yml`, `.gitignore`, `LICENSE`.
  - Done: CI green; `cargo test` runs; `cargo fmt --check` and clippy clean.

- [x] **S0.2 TIFF reader**
  - Scope: decode 8/16-bit RGB, strips and tiles, none/LZW/Deflate/PackBits, horizontal predictor, ICC tag, EXIF orientation, BigTIFF read.
  - Read: `docs/io-and-color.md` §1–3, §8.
  - Touch: `src/io/tiff_read.rs`, `src/io/exif.rs`, `tests/io_read.rs`, `fixtures/io/*`.
  - Done: every fixture variant decodes to expected pixels; unsupported inputs fail with a clear error; orientation unit tests pass.

- [x] **S0.3 TIFF writer and round-trip**
  - Scope: encode 16-bit RGB, none/LZW/Deflate, ICC tag, resolution, orientation=1, metadata copy policy.
  - Read: `docs/io-and-color.md` §4–5, §7.
  - Touch: `src/io/tiff_write.rs`, `src/io/metadata.rs`, `tests/io_roundtrip.rs`.
  - Done: read→write→read is bit-exact on all orientation=1 fixtures; ICC bytes identical; output opens in Lightroom and Photoshop.

- [x] **S0.4 Color pipeline**
  - Scope: detect ICC (sRGB/linear/Adobe RGB/ProPhoto/P3/unknown), linearize to f32 linear sRGB, encoded luma, output profile conversion or preservation.
  - Read: `docs/io-and-color.md` §6.
  - Touch: `src/color/*.rs`, `tests/color.rs`.
  - Done: sRGB round-trip within 1e-6; luma matches reference; unknown-profile behavior and `--assume-profile` tested.

- [x] **S0.5 CLI skeleton and pipeline stub**
  - Scope: clap definitions, input validation (matching dimensions/depth), load→copy→write pipeline, progress, exit codes, `--report` stub.
  - Read: `docs/cli.md` §1–3, §5.
  - Touch: `src/cli.rs`, `src/pipeline.rs`, `src/main.rs`, `tests/cli.rs`.
  - Done: `focus-stack a.tif b.tif -o out.tif` writes a valid TIFF; `--help` complete; bad inputs exit 2/3; mismatched sizes rejected.

- [x] **S0.6 Synthetic stack generator**
  - Scope: scene + depth + N defocused frames with known transforms, noise, exposure gain, and ground truth; TOML config; committed tiny fixtures.
  - Read: `docs/quality.md` §2.
  - Touch: `src/quality/synthetic.rs`, `tests/synthetic.rs`, `fixtures/synthetic/*.toml`.
  - Done: deterministic seed; generated stack passes analytic PSF sanity checks; 512×512 fixture committed.

---

## S1 — Alignment (3–4 days)

- [x] **S1.1 Alignment preprocessing**
  - Scope: encoded luma, downsample, gradient mask, saturation/dark masks, Hann window.
  - Read: `docs/alignment.md` §2.
  - Touch: `src/align/preprocess.rs`, `tests/align_preprocess.rs`.
  - Done: masks exclude sky/saturation in synthetic cases; memory bounded.

- [x] **S1.2 Phase-correlation translation**
  - Scope: FFT cross-power spectrum, peak search, parabolic subpixel, optional upsampled DFT.
  - Read: `docs/alignment.md` §3.
  - Touch: `src/align/phase.rs`, `tests/align_phase.rs`.
  - Done: recovers known integer and 0.25 px shifts within 0.05 px.

- [x] **S1.3 Fourier–Mellin rotation and scale**
  - Scope: high-pass magnitude spectrum, log-polar transform, phase correlation, 180° disambiguation, range clamps.
  - Read: `docs/alignment.md` §4.
  - Touch: `src/align/logpolar.rs`, `tests/align_logpolar.rs`.
  - Done: ±2° and 1.5% scale recovered within 0.1°/0.1%.

- [x] **S1.4 ECC similarity refinement**
  - Scope: 4-parameter similarity, pyramidal inverse-compositional Lucas–Kanade, Huber weights, masks, convergence.
  - Read: `docs/alignment.md` §5.
  - Touch: `src/align/ecc.rs`, `tests/align_ecc.rs`.
  - Done: <0.05 px on synthetic; robust with 5% outlier pixels; composes with coarse estimate.

- [x] **S1.5 Lanczos warp and crop**
  - Scope: separable Lanczos3 in linear light, edge clamp, anti-alias pre-blur on minification, valid-rect crop, rayon rows.
  - Read: `docs/alignment.md` §6–7.
  - Touch: `src/align/warp.rs`, `tests/align_warp.rs`.
  - Done: identity warp bit-exact; 90° round-trip within 1 LSB; 24 MP warp < 0.5 s/frame; no black borders.

- [x] **S1.6 Alignment diagnostics**
  - Scope: per-tile residual/NCC, median/p95, warning thresholds, report fields.
  - Read: `docs/alignment.md` §8–9.
  - Touch: `src/align/diagnostics.rs`, `src/output/report.rs`.
  - Done: known synthetic residual is reported correctly; >0.3 px p95 warns; tests pass.

---

## S2 — Focus measurement (2–3 days)

- [x] **S2.1 Laplacian pyramid**
  - Scope: Gaussian/Laplacian pyramid in f32, level count policy, buffer reuse.
  - Read: `docs/focus-measure.md` §2.
  - Touch: `src/focus/pyramid.rs`, `tests/focus_pyramid.rs`.
  - Done: reconstruct identity within 1e-6; level count correct for 24 MP; memory measured.

- [x] **S2.2 Noise estimation**
  - Scope: per-level sigma via MAD on a low-gradient mask; noise floor.
  - Read: `docs/focus-measure.md` §3.
  - Touch: `src/focus/noise.rs`, `tests/focus_noise.rs`.
  - Done: synthetic sigma within 5%; texture does not inflate the estimate.

- [x] **S2.3 Energy maps**
  - Scope: box energy via summed-area tables, radius schedule, contrast normalization, cross-level aggregation weights.
  - Read: `docs/focus-measure.md` §4, §6.
  - Touch: `src/focus/energy.rs`, `tests/focus_energy.rs`.
  - Done: monotonic response to a known blur ladder; 4-frame 24 MP energy pass < 1 s.

- [x] **S2.4 Confidence, coverage, and calibration**
  - Scope: best-vs-second margin, guided smoothing, coverage mask, saturation exclusion, per-frame calibration, statistics.
  - Read: `docs/focus-measure.md` §5–6.
  - Touch: `src/focus/confidence.rs`, `src/focus/coverage.rs`, `tests/focus_confidence.rs`.
  - Done: confident on textured regions, near-zero on synthetic sky; frame ranking accuracy > 95% on a blur ladder.

---

## S3 — PMax engine (1–2 days)

- [x] **S3.1 Level weights**
  - Scope: temperature softmax, noise/confidence gating, chroma-smoothed weights.
  - Read: `docs/stacking.md` §2.1.
  - Touch: `src/stack/pmax.rs`, `tests/stack_weights.rs`.
  - Done: weights sum to 1; no NaN on flat regions; confidence gating verified.

- [x] **S3.2 Pyramid blend and integration**
  - Scope: Laplacian coefficient blend, coarse-level tone anchoring, reconstruct, `--method pmax`.
  - Read: `docs/stacking.md` §2.2–2.3.
  - Touch: `src/stack/pmax.rs`, `src/pipeline.rs`, `tests/stack_pmax.rs`.
  - Done: synthetic PMax beats naive Laplacian-max on SSIM; no sky noise blow-up; valid 16-bit output.

---

## S4 — DMap engine (3–4 days)

- [x] **S4.1 Cost volume and argmax**
  - Scope: per-pixel cost, confidence-weighted argmax, initial depth field.
  - Read: `docs/stacking.md` §3.1.
  - Touch: `src/stack/dmap.rs`, `tests/stack_dmap_cost.rs`.
  - Done: argmax matches synthetic depth within one frame; flat regions flagged.

- [x] **S4.2 Automatic contrast threshold**
  - Scope: threshold from histogram + noise floor, `--keep-detail` override, confidence ramp, reporting.
  - Read: `docs/stacking.md` §3.2.
  - Touch: `src/stack/dmap.rs`, `tests/stack_dmap_threshold.rs`.
  - Done: threshold stable across fixture stacks; sky marked low-confidence; chosen value in report.

- [x] **S4.3 Edge-aware depth optimization**
  - Scope: edge-aware neighbor weights, coarse-to-fine Jacobi/multigrid solve, smoothing radius.
  - Read: `docs/stacking.md` §3.3.
  - Touch: `src/stack/dmap.rs`, `tests/stack_dmap_solve.rs`.
  - Done: smooth depth in flat areas, sharp at edges; convergence test; 24 MP solve < 2 s.

- [x] **S4.4 Depth interpolation blend and map output**
  - Scope: interpolate bracketing frames, edge-aware hard selection, `--depth-map` writer, `--method dmap`.
  - Read: `docs/stacking.md` §3.4; `docs/outputs.md` §2.
  - Touch: `src/stack/dmap.rs`, `src/output/depth.rs`, `tests/output_depth.rs`.
  - Done: no double edges on synthetic ridge; depth map decodes to frame positions; CLI works end to end.

---

## S5 — Hybrid engine and quality core (3–4 days)

- [ ] **S5.1 Detail transfer**
  - Scope: guided base of PMax, high-frequency transfer onto DMap, soft clamp, YCbCr variant.
  - Read: `docs/stacking.md` §4.1.
  - Touch: `src/stack/hybrid.rs`, `tests/stack_hybrid.rs`.
  - Done: detail recovery ≥ 0.95 on synthetic; overshoot within limit; visual check at 100%.

- [ ] **S5.2 Halo suppression**
  - Scope: low-pass anchoring from DMap, gradient-domain/pyramid blend, edge-band diagnostics.
  - Read: `docs/stacking.md` §4.2.
  - Touch: `src/stack/hybrid.rs`, `tests/stack_halo.rs`.
  - Done: synthetic ridge halo measurably below DMap-alone and PMax-alone.

- [ ] **S5.3 Hybrid mask and method selection**
  - Scope: confidence/texture mask, guided smoothing, `auto|pmax|dmap|hybrid` selection, parameter plumbing.
  - Read: `docs/stacking.md` §4.3–4.4.
  - Touch: `src/stack/hybrid.rs`, `src/stack/method.rs`, `tests/stack_method.rs`.
  - Done: auto picks hybrid on textured stacks, DMap on smooth/low-confidence stacks; explicit methods override.

- [ ] **S5.4 Initial blind tuning**
  - Scope: run the 10-stack **tuning set** blind against baseline outputs, tune defaults, document open issues.
  - Read: `docs/quality.md` §5, §7.
  - Touch: `docs/quality-log.md`, default constants.
  - Done: median score ≥ Helicon − 0.5 and > Photoshop baseline; no regression on synthetic gates.

---

## S6 — Outputs, metadata, retouch (2–3 days)

- [ ] **S6.1 Quality report**
  - Scope: JSON schema, input hashes, parameters, alignment/focus diagnostics, timings, warnings.
  - Read: `docs/outputs.md` §4.
  - Touch: `src/output/report.rs`, `tests/report.rs`.
  - Done: schema validates; report deterministic across runs with `--timings` off.

- [ ] **S6.2 Preview and metadata**
  - Scope: 8-bit sRGB JPEG preview, EXIF/XMP/IPTC policy, Software tag, resolution tags.
  - Read: `docs/outputs.md` §1, §5.
  - Touch: `src/output/preview.rs`, `src/io/metadata.rs`, `tests/metadata.rs`.
  - Done: preview color matches output; metadata policy tested; orientation=1.

- [ ] **S6.3 Retouch subcommand**
  - Scope: mask JSON parser, stroke rasterization and feathering, mask PNG input, deterministic re-alignment from report, linear-light composite.
  - Read: `docs/outputs.md` §6; `docs/cli.md` §4.
  - Touch: `src/retouch.rs`, `src/cli.rs`, `tests/retouch.rs`.
  - Done: retouch from JSON alone reproduces the patch; no seams; invalid masks rejected.

- [ ] **S6.4 Reproducibility**
  - Scope: embed params/version/input hashes, `--seed`, deterministic reductions, output hash.
  - Read: `docs/outputs.md` §7.
  - Touch: `src/pipeline.rs`, `src/output/report.rs`, `tests/determinism.rs`.
  - Done: two runs byte-identical at the same thread count; cross-thread differences documented and below 1 LSB.

---

## S7 — Validation and performance (2–3 days)

- [ ] **S7.1 Metrics**
  - Scope: PSNR, SSIM, detail recovery, halo overshoot, noise gain, ΔE00; `focus-stack eval` for fixtures.
  - Read: `docs/quality.md` §3.
  - Touch: `src/quality/metrics.rs`, `tests/metrics.rs`.
  - Done: metrics match reference values on known images; CLI reports a table.

- [ ] **S7.2 Regression suite and CI gates**
  - Scope: fixture manifest, golden outputs, tolerance policy, CI jobs.
  - Read: `docs/quality.md` §4.
  - Touch: `tests/regression.rs`, `.github/workflows/ci.yml`, `fixtures/manifest.toml`.
  - Done: tiny suite < 2 min; PR fails on regression beyond tolerance.

- [ ] **S7.3 Formal real-stack A/B**
  - Scope: run the 10-stack **held-out set** with the full protocol; do not reuse tuning stacks.
  - Read: `docs/quality.md` §5, §7.
  - Touch: `docs/quality-log.md`.
  - Done: documented results and prioritized artifact list.

- [ ] **S7.4 Benchmarks and memory profile**
  - Scope: criterion benches for align, measure, stack; 24 MP and 60 MP; peak RSS.
  - Read: `docs/quality.md` §6.
  - Touch: `benches/*`, `docs/performance.md`.
  - Done: numbers recorded against targets; regressions visible in CI.

- [ ] **S7.5 Tiled path for 60 MP (optional)**
  - Scope: banded/tiled blending with overlap when `--max-memory` is tight.
  - Read: `docs/architecture.md` §5, §10.
  - Touch: `src/stack/tiled.rs`, `tests/tiled.rs`.
  - Done: 4 × 60 MP within memory; output matches untiled within 1 LSB.

---

## S8 — Packaging and release (1–2 days)

- [ ] **S8.1 Packaging**
  - Scope: release build, ad-hoc codesign, tarball, Homebrew formula.
  - Read: `docs/cli.md` §6.
  - Touch: `scripts/package.sh`, `Formula/focus-stack.rb`.
  - Done: clean-machine install; `focus-stack --version` works.

- [ ] **S8.2 User documentation**
  - Scope: README usage, sample workflow, tuning guide, troubleshooting.
  - Read: `docs/cli.md`; `docs/stacking.md` §6.
  - Touch: `README.md`, `docs/usage.md`.
  - Done: a new user can stack a sample without reading source.

- [ ] **S8.3 v0.1.0 release**
  - Scope: changelog, version bump, tag, CI release artifacts and checksums.
  - Touch: `CHANGELOG.md`, `Cargo.toml`, `.github/workflows/release.yml`.
  - Done: GitHub release published; artifact smoke test passes.

---

## Deferred backlog

Tracked here so they do not creep into v1:
- `inspect` pre-flight: per-frame sharpness, coverage estimate, redundant-frame warning.
- Motion/deghost: temporal consistency mask for wind.
- Local alignment: tile flow for handheld stacks.
- HDR/exposure blending, lens corrections, panorama stitching.
- GUI, GPU (Metal), Windows/Linux.
- Learned focus measures or deconvolution.
