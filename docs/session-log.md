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
