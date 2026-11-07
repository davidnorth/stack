# Architecture

## 1. Goals and constraints

- 3–4 frames, 16-bit RGB TIFF, 24 MP typical, up to ~60 MP.
- Tripod, static scene. v1 has no motion model and no local warp.
- macOS Apple Silicon, single binary, no external runtime.
- Deterministic output for identical inputs, parameters, and thread count.
- Peak memory is bounded; fail clearly if `--max-memory` would be exceeded.
- 16-bit end to end; blending in linear light; ICC preserved by default.

## 2. Crate layout

```text
src/
  main.rs              entry point, exit codes
  lib.rs               public library surface
  cli.rs               clap definitions, validation
  pipeline.rs          load -> align -> measure -> stack -> write
  io/
    tiff_read.rs       decode strips/tiles, compression, predictor
    tiff_write.rs      encode 16-bit, compression, extra tags
    exif.rs            orientation + metadata extraction
    metadata.rs        metadata copy policy
  color/
    profile.rs         ICC detect/convert (lcms2)
    transfer.rs        sRGB <-> linear
    luma.rs            encoded luma, linear luma
  align/
    preprocess.rs      masks, downscale, high-pass
    phase.rs           phase correlation, subpixel
    logpolar.rs        Fourier-Mellin rotation/scale
    ecc.rs             similarity refinement
    warp.rs            Lanczos3, valid rect, crop
    diagnostics.rs     residual estimates, warnings
  focus/
    pyramid.rs         Gaussian/Laplacian pyramid
    noise.rs           per-level sigma, floor
    energy.rs          box energy, normalization
    confidence.rs      margin confidence, guided smoothing
    coverage.rs        no-sharp-frame mask
  stack/
    pmax.rs            pyramid maximum contrast
    dmap.rs            cost volume, depth solve, interpolation
    hybrid.rs          detail transfer, halo anchoring
    method.rs          auto selection
  output/
    depth.rs           16-bit depth/source map
    preview.rs         8-bit sRGB JPEG
    report.rs          JSON report
  retouch.rs           mask JSON -> recomposite
  quality/
    synthetic.rs       ground-truth stack generator
    metrics.rs         PSNR/SSIM/detail/halo/noise/ΔE00
```

Rules:
- `io` is the only module that knows TIFF tags.
- `color` is the only module that knows transfer functions and ICC.
- `stack` consumes `focus` outputs; it never reads files or decodes TIFF.
- `pipeline` is the only module that wires everything together.

## 3. Data model

```rust
pub struct FrameMeta {
    pub path: PathBuf,
    pub width: u32,
    pub height: u32,
    pub bits: u8,                 // 8 or 16
    pub icc: Option<Vec<u8>>,     // raw profile bytes
    pub orientation: u16,         // EXIF 274
    pub exif: Option<Vec<u8>>,    // policy-dependent
    pub xmp: Option<Vec<u8>>,     // tag 700, raw
}

pub struct Frame {
    pub meta: FrameMeta,
    pub rgb: Vec<u16>,            // interleaved RGB, len = w*h*3
}

/// Similarity transform. Maps output pixel coords -> source pixel coords.
/// x' = a*x - b*y + tx
/// y' = b*x + a*y + ty
pub struct Similarity {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

pub struct Rect { pub x: u32, pub y: u32, pub w: u32, pub h: u32 }

pub struct AlignedSet {
    pub reference: usize,
    pub transforms: Vec<Similarity>,
    pub valid: Rect,
}

pub struct FocusPyramid {
    pub levels: usize,
    pub laplacian: Vec<Vec<f32>>, // per level, one channel
}

pub struct DepthField {
    pub d: Vec<f32>,              // continuous frame index, 0..N-1
    pub confidence: Vec<f32>,     // 0..1
}
```

All pixel buffers are row-major. `f32` image values are normalized to `[0, 1]`
in linear sRGB unless a field name says otherwise (`encoded`).

## 4. Pipeline

| Stage | Input | Output | Notes |
|---|---|---|---|
| Load | paths | `Vec<Frame>` | validate same dimensions, bit depth, profile |
| Color | `Frame` | linear luma + linear RGB | encoded luma kept for alignment/focus |
| Align | encoded luma | `AlignedSet` | reference chosen by gradient energy |
| Warp | `Frame` + transforms | aligned linear frames | band-cached; edge clamp |
| Focus | aligned luma | `FocusPyramid` per frame | noise floor subtracted |
| Stack | pyramids + confidence | linear RGB f32 | PMax, DMap, or hybrid |
| Encode | linear RGB | 16-bit TIFF | ICC, metadata, depth map, report |

Ordering matters: alignment and focus measurement use **encoded** luma
(perceptual), while blending and output use **linear** light. See
`docs/io-and-color.md` §6.

## 5. Memory budget

At 24 MP (6000 × 4000 = 24.0 Mpx):

| Buffer | Size |
|---|---|
| Source `u16` RGB, per frame | 144 MB |
| Source, 4 frames | 576 MB |
| `f32` luma, per frame | 96 MB |
| Warped `f32` RGB, per frame | 288 MB |
| Laplacian + energy, per frame | ~128 MB |
| Output `f32` RGB | 288 MB |

A naive full-frame implementation peaks around 2.5–3 GB for 4 × 24 MP. The
v1 budget is 4 GB. 60 MP is roughly 2.5× larger (~6–7 GB), acceptable on
32 GB machines; S7.5 adds banded processing if needed.

Band strategy (when implemented): process the warped frames and pyramids in
horizontal bands with `2 * max_pyramid_radius` rows of overlap. All per-pixel
operations are local, so bands compose exactly; only the coarse pyramid levels
need a final global pass.

## 6. Concurrency and determinism

- `rayon` parallelizes over output rows or tiles. Each task writes a disjoint
  slice; no shared accumulation.
- Reductions (median, percentiles, sums) run serially over precomputed
  per-row partials in a fixed order, or use `f64` and a stable sort.
- FFTs are deterministic for a fixed size and library version.
- Transforms use `f64`; pixel math uses `f32`.
- Output is byte-identical for the same thread count. Across thread counts,
  differences are allowed to be ≤ 1 LSB and must be documented.
- The synthetic generator uses an explicit seed; no wall-clock or unseeded RNG.

## 7. Errors, logging, progress

```rust
pub enum Error {
    Io(std::io::Error),
    Tiff(tiff::TiffError),
    Unsupported(String),
    DimensionMismatch { expected: (u32, u32), got: (u32, u32) },
    ProfileMismatch(String),
    AlignmentFailed { frame: usize, residual_px: f64 },
    MemoryBudget { needed: u64, limit: u64 },
    ColorProfile(String),
}

pub type Result<T> = std::result::Result<T, Error>;
```

- `thiserror` in the library, `anyhow` only in `main`.
- `tracing` to stderr; `indicatif` progress bar only when stderr is a TTY.
- `--quiet` suppresses progress, not warnings.
- Exit codes in `docs/cli.md` §5.

## 8. Dependencies

| Crate | Purpose | Notes |
|---|---|---|
| `clap` (derive) | CLI | |
| `anyhow`, `thiserror` | errors | |
| `tiff` | TIFF decode/encode | verify ICC extra-tag write in S0.3 |
| `rayon` | parallelism | |
| `rustfft`, `realfft` | phase correlation, log-polar | |
| `lcms2` | ICC parse/convert | enable bundled build; avoid Homebrew |
| `serde`, `serde_json`, `toml` | report, config, fixtures | |
| `png` | mask input, optional maps | |
| `jpeg-encoder` | preview | |
| `sha2` | input/output hashes | |
| `indicatif` | progress | |
| `tracing`, `tracing-subscriber` | logging | |
| `num_cpus` | default thread count | |
| `approx`, `criterion`, `proptest` (dev) | tests/benches | |

If `tiff` cannot write the ICC tag or preserve predictor metadata, S0.3 falls
back to a minimal internal writer for uncompressed and Deflate TIFF. Record the
decision in `docs/decisions.md`.

## 9. Coordinate conventions

- Origin is top-left; `x` right, `y` down.
- Pixel `(x, y)` has its center at continuous coordinate `(x, y)`.
- `Similarity` maps **output** coordinates to **source** coordinates.
- Positive rotation is clockwise in image coordinates (because `y` is down).
- Warp sampling: `p_src = T * (x, y, 1)`; sample with Lanczos3.
- Depth `d = 0` is frame 0; `d = N-1` is the last frame; fractional values
  interpolate between adjacent frames.
- All of the above have unit tests with synthetic shifts at S1.2/S1.5.

## 10. Extension points

- **Tiling:** `stack` functions operate on `&[f32]` plus a stride and a band
  rectangle. S7.5 adds band overlap; no algorithm change.
- **Motion mask:** a per-pixel confidence multiplier can be threaded through
  `focus` and `stack` without changing their interfaces.
- **GPU:** keep hot loops in pure functions over slices so a Metal backend can
  replace them later.
- **Local warp:** `Similarity` is a special case of a mesh warp; reserve a
  `WarpField` enum rather than hard-coding the type in the pipeline.
