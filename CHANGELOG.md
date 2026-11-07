# Changelog

All notable changes to `focus-stack` are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-11-07

First public pre-release. macOS Apple Silicon only (`aarch64-apple-darwin`).

### Added

- 8-bit and 16-bit RGB TIFF input and output with strips, tiles, none/LZW/
  Deflate/PackBits compression, horizontal predictor, BigTIFF, ICC profiles,
  and EXIF orientation normalization.
- Linear-light color pipeline with sRGB/linear sRGB/Adobe RGB/ProPhoto/P3
  detection, ICC preservation, and explicit output profile conversion.
- Focus measurement: Laplacian pyramids, per-level noise estimation, box-energy
  maps, confidence/coverage masks, and per-frame calibration.
- PMax, DMap, and Hybrid stacking cores, including depth-map generation and
  hybrid detail transfer with halo suppression.
- Alignment building blocks: phase-correlation translation, Fourier–Mellin
  rotation/scale, ECC similarity refinement, Lanczos3 warping, and residual
  diagnostics.
- Outputs: 16-bit TIFF, depth/source map, JSON quality report, 8-bit sRGB JPEG
  preview, and `retouch` compositing from stroke/polygon/mask JSON.
- `eval` quality metrics: PSNR, SSIM, detail recovery, halo overshoot, noise
  gain, and ΔE00.
- Deterministic runs with input/output SHA-256 hashes, embedded parameters,
  optional timings, and an explicit thread count.
- Reproducible release packaging: ad-hoc codesigned `aarch64-apple-darwin`
  tarball, SHA-256 checksums, and a generated Homebrew formula.
- macOS Apple Silicon CI for formatting, clippy, tests, regression fixtures,
  and performance benchmarks.

### Known limitations

- The CLI currently wires PMax and DMap; `auto` and `hybrid` fall back to the
  reference frame until alignment and hybrid dispatch are connected.
- Real-stack tuning and the formal held-out A/B comparison are pending; see
  `docs/quality-log.md`.
- Tiled blending is available for the 60 MP memory budget but is not yet
  dispatched automatically, and the default TIFF decoder buffer still limits
  16-bit 60 MP input.
- Windows and Linux builds, GPU acceleration, motion/deghost masking, and local
  alignment are out of scope for v0.1.0.

[Unreleased]: https://github.com/davidnorth/stack/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/davidnorth/stack/releases/tag/v0.1.0
