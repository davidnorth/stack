# Performance

Reference numbers recorded by S7.4 on 2026-11-07. Targets are from
`docs/quality.md` §6.

## Machine and build

| Field | Value |
|---|---|
| Chip | Apple M1, 8 cores |
| RAM | 8 GB |
| OS | macOS 26.5.2 (25F84) |
| Toolchain | rustc 1.99.1 |
| Profile | cargo bench (release, debug assertions off) |
| Commit | `351ab68` + S7.4 working tree |
| Threads | default (rayon uses all cores) |

## End-to-end

`/usr/bin/time -l target/release/focus-stack --method pmax ...`, 4 frames,
16-bit LZW output. Hybrid and alignment are not wired into the pipeline yet, so
the PMax path is the only end-to-end stack available. The 60 MP inputs are
8-bit because the TIFF decoder currently rejects 16-bit 60 MP frames (see
Known gaps).

| Case | Target | Measured |
|---|---|---|
| 4 × 24 MP, hybrid, total | ≤ 8 s | **20.0 s** PMax proxy (25.7 s with `--report --timings`; timed stages 21.1 s) |
| 4 × 24 MP, peak RSS | ≤ 4 GB | **2.70 GB** PMax proxy |
| 4 × 60 MP, hybrid, total | ≤ 25 s | **94.7 s** PMax, 4 frames, 8-bit input (137.3 s with `--report --timings`) |
| 4 × 60 MP, peak RSS | ≤ 8 GB | **3.12 GB max RSS** but **13.03 GB peak footprint** (PMax, 8-bit input; the process swapped) |

“Peak RSS” is the maximum resident set size from `/usr/bin/time -l`. The 60 MP
run's much larger peak memory footprint shows that resident size alone hides
compressed/swapped memory and should not be treated as proof that the 8 GB
budget holds.

## Stage budgets at 24 MP

| Stage | Target | Measured |
|---|---|---|
| load | 0.5 s | **4.27 s** (4 × 24 MP, 16-bit LZW) |
| align | 0.5 s | **3.11 s** (preprocess 0.293 + phase 0.137 + ECC 2.682; not wired into the pipeline) |
| focus | 1.0 s | **0.479 s per frame** (pyramid 0.103 + noise 0.262 + energy 0.061 + aggregate 0.052), ≈ **1.92 s** for 4 frames |
| stack | 3.0 s | **3.79 s** PMax (4 frames); **21.27 s** DMap (4 frames) |
| encode | 0.5 s | **2.21 s** (24 MP, 16-bit LZW) |

The pipeline's `timing_ms.stack` includes focus measurement, confidence, and
blending: 14.62 s for 4 × 24 MP PMax, of which `pmax::stack` alone is 3.79 s.
The difference is full-resolution luma construction, confidence filtering, and
allocation churn, which the component benches do not cover.

## 60 MP component benchmarks

Alignment is capped at 2048 px long edge, so its correlation/ECC cost is
essentially independent of the source size. Stack benches use two frames so
the working set fits in 8 GB.

| Benchmark | Frames | Median |
|---|---|---|
| align/preprocess_60mp | — | 0.436 s |
| align/phase_60mp | — | 0.137 s |
| align/ecc_60mp | — | 2.305 s |
| measure/pyramid_60mp | 1 | 0.259 s |
| measure/noise_60mp | 1 | 0.702 s |
| measure/energy_60mp | 1 | 0.150 s |
| measure/aggregate_60mp | 1 | 0.163 s |
| stack/pmax_60mp | 2 | 8.71 s |
| stack/dmap_60mp | 2 | 64.53 s |
| stack/hybrid_merge_60mp | — | 16.17 s |

Peak RSS of the benchmark processes (setup included): PMax 5.02 GB, DMap
4.51 GB, hybrid merge 4.19 GB.

## Regression visibility

- `cargo bench --bench align --bench measure --bench stack_pmax
  --bench stack_dmap --bench stack_hybrid -- --noplot` reproduces the numbers
  above. Each stack engine is a separate process so a 60 MP run does not leave
  earlier engine buffers resident.
- `benches/thresholds.json` stores a limit of 2× the reference median for every
  benchmark. `benches/check_performance.py` compares the latest Criterion
  estimates against those limits and exits non-zero on a breach.
- The `bench` CI job runs on the nightly schedule and on manual dispatch,
  then uploads `target/criterion` as an artifact. A gross regression fails the
  run. The 20 % review policy from `docs/quality.md` §6 still applies to PRs;
  the 2× CI limit is deliberately loose enough for runner variation.

## Method

Fixtures for the end-to-end runs were generated with ImageMagick:

```sh
magick -seed 0 -size 6000x4000 plasma:fractal -depth 16 -compress lzw f0.tif
magick -seed 0 -size 9500x6333 plasma:fractal -depth 8  -compress lzw g0.tif
```

The other three frames use a different seed. Component benches build the same
kind of deterministic gradient-plus-noise image in memory
(`benches/common/mod.rs`), so no fixture files are committed.

## Known gaps

- **16-bit 60 MP input is rejected.** The `tiff` crate defaults to a 256 MiB
  decoding buffer (`decoding_buffer_size`). A 60 MP RGB16 frame is about
  343 MiB, so `read_tiff` fails with “decoder limits exceeded”. The 60 MP
  end-to-end number above therefore uses 8-bit input; the benchmark stages are
  unaffected. This needs an I/O change (or the tiled path in S7.5) before the
  60 MP target can be measured on production inputs.
- **Hybrid is not wired.** `--method hybrid` still falls back to the reference
  frame. The measured hybrid proxy is the sum of the PMax, DMap, and detail
  transfer stages; at 24 MP that is 25.7 s and at 60 MP (two frames) it is
  89.4 s, both far outside the targets.
- **DMap dominates.** The depth solve is 5.6× PMax at 24 MP and 7.4× at 60 MP
  (2 frames). No hybrid target is reachable until DMap is optimised or
  scaled down.
- **Alignment and encoding are over budget** even though alignment is absent
  from the current pipeline. ECC (2.3–2.7 s) and LZW encoding (2.2 s at 24 MP,
  7.6 s at 60 MP) are the largest single-stage gaps.
- **Memory footprint, not RSS, is the 60 MP risk.** The 4 × 60 MP PMax runs
  took 94.7–137.3 s and compressed/swapped heavily; the peak footprint was
  13.03 GB. Reaching the 8 GB target in practice likely needs the S7.5 tiled
  path even though max RSS stayed low.
