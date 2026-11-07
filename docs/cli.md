# CLI

Binary name: `focus-stack`.

## 1. Synopsis

```text
focus-stack [OPTIONS] <INPUT>...
focus-stack retouch [OPTIONS] --base <TIFF> --sources <TIFF>... --report <JSON> --mask <JSON> -o <TIFF>
focus-stack eval [OPTIONS] --truth <TIFF> --test <TIFF>
```

One input is accepted and performs an identity pass (useful for I/O tests).
Two or more inputs are stacked.

## 2. Options

### Core

| Option | Default | Notes |
|---|---|---|
| `-o, --output <PATH>` | required | main TIFF |
| `--method <auto\|pmax\|dmap\|hybrid>` | `auto` | auto = hybrid unless confidence is low |
| `--align <auto\|translation\|similarity\|off>` | `similarity` | auto = similarity |
| `--reference <auto\|INDEX>` | `auto` | max gradient energy |
| `--max-rotation <DEG>` | `3.0` | alignment clamp |
| `--scale-range <MIN,MAX>` | `0.97,1.03` | focus-breathing clamp |
| `--max-residual <PX>` | `1.0` | fail threshold; warns at 0.3 |
| `--force` | off | continue after alignment failure |
| `--keep-borders` | off | no crop; edge-clamp instead |

### Focus and stacking

| Option | Default | Notes |
|---|---|---|
| `--estimation-radius <PX>` | auto 12–32 | detail scale |
| `--keep-detail <PERCENT>` | auto | DMap confidence override |
| `--detail-transfer <FLOAT>` | `0.7` | hybrid PMax detail strength |
| `--exposure-match <off\|luma\|rgb>` | `off` | normalize before measuring |
| `--pmax-beta <FLOAT>` | `6.0` | debug |
| `--dmap-lambda <FLOAT>` | `1.0` | debug |
| `--contrast-normalize` | off | debug |

### Outputs

| Option | Default | Notes |
|---|---|---|
| `--depth-map <PATH>` | none | 16-bit grayscale TIFF |
| `--source-map <PATH>` | none | alias for depth map in v1 |
| `--report <PATH>` | none | JSON |
| `--preview <PATH>` | none | 8-bit sRGB JPEG |
| `--preview-size <PX>` | `2048` | long edge |
| `--compression <none\|lzw\|deflate>` | `lzw` | |
| `--bits <8\|16>` | `16` | |
| `--output-profile <same\|srgb\|linear>` | `same` | |
| `--assume-profile <PATH>` | none | for untagged/unknown inputs |
| `--allow-profile-mismatch` | off | otherwise error |
| `--strip-metadata` | off | keep ICC/orientation/resolution only |
| `--stamp` | off | add timestamp to report (breaks determinism) |
| `--timings` | off | add stage timings to report (breaks determinism) |

### Runtime

| Option | Default | Notes |
|---|---|---|
| `--threads <N>` | all cores | |
| `--max-memory <SIZE>` | unlimited | e.g. `4G`, `4096M`; preflight estimate |
| `--seed <N>` | `0` | synthetic/retouch determinism |
| `--strict` | off | warnings become exit code 5 |
| `-v, --verbose` | off | debug logging |
| `-q, --quiet` | off | no progress bar |
| `--version` | | |
| `--help` | | |

Do not expose internal tuning knobs in normal use. The documented set is:
method, detail transfer, estimation radius, keep-detail, exposure match,
alignment range, compression, profile.

## 3. Examples

Basic stack:

```bash
focus-stack near.tif mid.tif far.tif -o all-in-focus.tif
```

Explicit DMap with artifacts:

```bash
focus-stack *.tif -o out.tif \
  --method dmap \
  --depth-map depth.tif \
  --report run.json \
  --preview preview.jpg
```

Fast preview pass at lower quality:

```bash
focus-stack *.tif -o preview.tif \
  --estimation-radius 24 \
  --compression none \
  --preview preview.jpg
```

8-bit delivery copy:

```bash
focus-stack *.tif -o out.tif --bits 8 --output-profile srgb
```

Constrained memory:

```bash
focus-stack *.tif -o out.tif --max-memory 4G --threads 8
```

## 4. Subcommands

### `retouch`

```text
focus-stack retouch \
  --base <TIFF> \
  --sources <TIFF>... \
  --report <JSON> \
  --mask <JSON> \
  [-o <TIFF>]
```

- Re-aligns deterministically using the report's transforms.
- Verifies input hashes.
- Applies stroke/polygon/PNG masks (`docs/outputs.md` §6).
- Writes a new TIFF; never modifies the base.

### `eval`

Development command for the quality harness:

```text
focus-stack eval --truth <TIFF> --test <TIFF> [--json]
```

Prints PSNR, SSIM, detail recovery, halo overshoot, noise gain, and ΔE00.
Not part of the user-facing workflow.

### Deferred

- `inspect`: pre-flight sharpness, coverage, and redundant-frame report.
- `stack --project`: save intermediate pyramids for repeated retouching.

## 5. Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | unexpected internal error |
| 2 | usage error (bad arguments) |
| 3 | input/IO error (missing, corrupt, unsupported, mismatch) |
| 4 | processing failure (alignment, memory budget, convergence) |
| 5 | quality warning escalated by `--strict` |
| 130 | interrupted (SIGINT) |

Warnings (coverage gaps, low confidence, high residual below the fail
threshold) do not change the exit code unless `--strict` is set.

## 6. Distribution

- macOS Apple Silicon only (`aarch64-apple-darwin`).
- Release build with `lto = "thin"`, `codegen-units = 1`, stripped symbols.
- Ad-hoc codesign (`codesign --sign -`) so Gatekeeper does not quarantine the
  local build; a Developer ID is out of scope for v1.
- Artifact: `focus-stack-<version>-aarch64-apple-darwin.tar.gz`.
- Homebrew formula in `Formula/focus-stack.rb` pointing at the release tarball.
- `--version` prints `focus-stack <semver> (<git short hash>)`.
- No runtime dependencies; `lcms2` is statically built.

## 7. Configuration

No config file in v1. All behavior is explicit on the command line, which keeps
runs reproducible. Environment variables are limited to standard Rust logging
(`RUST_LOG`).
