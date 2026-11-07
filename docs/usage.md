# Usage

This guide takes you from a folder of focus-bracketed TIFFs to a stacked
image. For the complete option list, see [`cli.md`](cli.md).

## 1. Install and verify

`focus-stack` runs on macOS Apple Silicon only.

Build from source:

```bash
cargo build --release
./target/release/focus-stack --version
```

Or build the distributable tarball with `./scripts/package.sh`. Once v0.1.0 is
published, the generated `Formula/focus-stack.rb` provides the Homebrew
install.

To verify the binary with the repository fixtures:

```bash
focus-stack fixtures/io/rgb16_lzw.tif fixtures/io/rgb16_none.tif \
  -o /tmp/focus-stack-smoke.tif
```

This is a smoke test, not a real stack: any two or more readable TIFFs of the
same size exercise the pipeline.

## 2. Prepare the frames

Export the focus bracket from Lightroom as **16-bit RGB TIFF**. For a reliable
stack:

- Use a tripod and a static scene; only focus should change between frames.
- Keep the same dimensions and colour profile for every frame.
- Leave exposure constant. If it is not, see `--exposure-match` below.
- Two or more frames are required. One frame performs an identity pass.
- Frame order is recorded in the report; name the files so you can recognise
  them later.

A typical folder looks like this:

```text
run/
  near.tif
  mid.tif
  far.tif
```

## 3. First stack

```bash
focus-stack near.tif mid.tif far.tif -o all-in-focus.tif
```

The default is Hybrid, which balances PMax detail with DMap edge stability.
The output is a 16-bit LZW TIFF that preserves the input ICC profile.

To write the usual review artifacts in the same run:

```bash
focus-stack near.tif mid.tif far.tif \
  -o run/all-in-focus.tif \
  --depth-map run/depth.tif \
  --report run/report.json \
  --preview run/preview.jpg
```

- `--depth-map`: 16-bit grayscale relative depth map.
- `--preview`: 8-bit sRGB JPEG, 2048 px on the long edge by default.
- `--report`: deterministic JSON with input hashes, alignment transforms,
  focus statistics, warnings, and output details.

Open `run/preview.jpg` first. It is faster to judge than the full-resolution
TIFF and shows the same stack.

## 4. Sample workflow

1. **Stack and preview.** Start with the defaults and request the report:

   ```bash
   focus-stack near.tif mid.tif far.tif \
     -o run/all-in-focus.tif \
     --preview run/preview.jpg \
     --report run/report.json
   ```

2. **Read the report.** Check these fields before tuning:

   | Field | What to look for |
   |---|---|
   | `warnings` | Any alignment or focus warnings |
   | `alignment.residual_px` | Median and p95 residual; above 0.3 px is a warning, above `--max-residual` is a failure |
   | `focus.confident_pct` | Share of pixels with confident focus; low values mean a weak stack |
   | `focus.coverage_gap_pct` | Scene areas no frame covered sharply |
   | `output.clipped_pct` | Highlight or shadow clipping in the result |

3. **Tune only if the preview needs it.** Use the tuning guide below. Keep the
   previous command and change one option at a time.

4. **Retouch if needed.** If a small area picked the wrong source frame,
   retouch the stacked result without restacking:

   ```bash
   focus-stack retouch \
     --base run/all-in-focus.tif \
     --sources near.tif mid.tif far.tif \
     --report run/report.json \
     --mask run/mask.json \
     -o run/retouched.tif
   ```

   `--sources` must be in the same order as the report's `inputs`. The mask
   JSON accepts strokes, polygons, and PNG masks; see `outputs.md` §6. Retouch
   verifies the input hashes and never modifies the base file.

5. **Make a delivery copy.** Keep the 16-bit TIFF as the master. For screen or
   web delivery:

   ```bash
   focus-stack near.tif mid.tif far.tif \
     -o run/all-in-focus-8bit.tif \
     --bits 8 \
     --output-profile srgb
   ```

## 5. Tuning guide

Change the defaults only when the preview shows a problem. The controls below
are the photographer-facing set; the remaining flags are debug aids.

### Method

| Value | Use it when |
|---|---|
| `auto` (default) | You want the normal choice: Hybrid, or DMap when confidence is low |
| `hybrid` | You want strong detail with controlled halos |
| `pmax` | You want maximum local detail and can accept more noise and halos |
| `dmap` | Edges and flat areas matter more than fine texture; also useful for a clean depth map |

### Detail and focus measurement

| Option | Default | Effect |
|---|---|---|
| `--detail-transfer <FLOAT>` | `0.7` | Hybrid only. Lower (for example `0.5`) suppresses halos; higher (for example `0.9`) keeps more PMax detail. |
| `--estimation-radius <PX>` | auto 12–32 | Focus detail scale. Decrease for fine texture and high magnification; increase for soft, noisy, or low-detail scenes. |
| `--keep-detail <PERCENT>` | auto | DMap confidence override. Raise it when DMap washes out detail; leave it unset for automatic behaviour. |

### Exposure and alignment

| Option | Default | Effect |
|---|---|---|
| `--exposure-match off` | off | Use for constant-exposure Lightroom exports. |
| `--exposure-match luma` | | Corrects brightness drift (flash variation, aperture drift). |
| `--exposure-match rgb` | | Corrects per-channel drift such as white-balance variation. |
| `--align similarity` | similarity | Allows translation, rotation, and scale; the normal tripod choice. |
| `--align translation` | | Use when the camera did not rotate or change scale. |
| `--align off` | | Only for a perfectly rigid, pre-aligned capture. |
| `--max-rotation <DEG>` | `3.0` | Raise cautiously if alignment fails on a looser tripod. |
| `--scale-range <MIN,MAX>` | `0.97,1.03` | Focus-breathing clamp. Widen only when the lens breathes more. |
| `--reference <auto\|INDEX>` | auto | `auto` picks the frame with the most gradient energy. Override with a zero-based index if that frame is unsuitable. |
| `--max-residual <PX>` | `1.0` | Alignment failure threshold. The run warns above 0.3 px. |

Do not use histogram matching by default; it distorts scene colour. If you use
`--exposure-match`, check the reported gains.

### Output

| Option | Default | Effect |
|---|---|---|
| `--compression <none\|lzw\|deflate>` | `lzw` | `none` is fastest for intermediates; `deflate` trades speed for size. |
| `--bits <8\|16>` | `16` | Keep 16 for editing; use 8 for delivery. |
| `--output-profile <same\|srgb\|linear>` | `same` | `same` preserves the input ICC. Use `srgb` for screen delivery. |
| `--assume-profile <PATH>` | none | Assign a profile to untagged or unknown inputs. |
| `--allow-profile-mismatch` | off | Accept mixed input profiles instead of failing. |
| `--strip-metadata` | off | Keep only ICC, orientation, resolution, and the software tag. |
| `--max-memory <SIZE>` | unlimited | Budget such as `4G` or `4096M`; checked before processing. |
| `--threads <N>` | all cores | Limit CPU use. |

## 6. Troubleshooting

Exit codes are listed in [`cli.md`](cli.md) §5.

| Symptom | What to try |
|---|---|
| Exit `2`, usage error | Check the spelling and required values with `focus-stack --help` or `focus-stack <subcommand> --help`. `retouch` requires `--base`, `--sources`, `--report`, and `--mask`. |
| Exit `3`, input/IO error | Make sure every file exists and opens in Preview. Check dimensions with `sips -g pixelWidth -g pixelHeight file.tif`. For mixed profiles, use `--allow-profile-mismatch`; for untagged or unknown profiles, use `--assume-profile <icc>`. |
| Exit `4`, processing failure | The usual cause is alignment. Confirm the scene was static and tripod-mounted. Try `--align translation`, a larger `--max-rotation`, or a wider `--scale-range`. Use `--force` only to inspect a failed run, not to ship it. |
| Exit `5`, quality warning | A warning was escalated by `--strict`. Re-run without `--strict`, then inspect the report and preview. |
| Halos around in-focus edges | Lower `--detail-transfer` (try `0.5`), switch to `--method dmap`, or retouch the affected area. |
| Result looks soft or washed out | Raise `--keep-detail`, lower `--estimation-radius` for fine detail, or try `--method pmax`. |
| Background or foreground is blurred | Check `focus.coverage_gap_pct` in the report; the bracket may not cover that depth range. Add frames. |
| Colours shift after stacking | Keep `--output-profile same` for the master. Use `--output-profile srgb` only for delivery. Check for mixed input profiles. |
| Too much noise | Prefer `--method dmap` or lower `--detail-transfer`; PMax amplifies fine noise. |
| Out of memory | Add `--max-memory 4G` and lower `--threads`. The estimate is checked before the heavy stages. |
| `retouch` refuses to run | Use the report from the same stacking run as `--base`, pass `--sources` in report order, and do not modify or rename the source files after stacking. |
| Output is cropped | This is the default border crop after alignment. Use `--keep-borders` to retain the full frame with edge clamping. |

## 7. Getting help

```bash
focus-stack --help
focus-stack retouch --help
focus-stack eval --help
```

`retouch` and `eval` are subcommands; the main stack is the default command.
