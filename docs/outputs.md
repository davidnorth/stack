# Outputs

## 1. Main image

- 16-bit RGB TIFF by default (`--bits 8` for 8-bit).
- Classic TIFF, strips, chunky RGB, `Orientation = 1`.
- Compression: `lzw` default, `deflate` or `none` optional.
- ICC: exact bytes from the reference frame by default
  (`--output-profile same|srgb|linear`).
- Resolution tags copied from the reference frame.
- Cropped to the alignment valid rectangle by default (`--keep-borders`).
- Linear-light blending; encode to the output profile only at the end.
- No output sharpening, no denoising, no tone curve. Those belong downstream.

Depth/source maps and the report are written only when requested.

## 2. Depth map

`--depth-map <PATH>` writes a 16-bit grayscale TIFF:

```text
value = round( (d / (N - 1)) * 65535 )
```

- `d` is the continuous frame index from DMap; `N` is the input count.
- `0` = first input frame, `65535` = last input frame.
- For `N = 1`, write all zeros and warn.
- `ImageDescription`: `focus-stack depth map; frames=<N>; 0=first,65535=last`.
- No ICC, or a linear gray profile; document which.
- Interpolation between frames is intentional: the map is a height field, not a
  nearest-frame index.

Use it for:
- external retouching (select a source frame by depth),
- focus-coverage inspection,
- 3D/displacement experiments.

## 3. Source map

In v1, `--source-map` is an alias for `--depth-map`: the 16-bit value encodes
both the nearest frame and the fraction, which is all a retouch mask needs for
`N <= 4`. Decode with:

```text
d = value / 65535 * (N - 1)
nearest = floor(d)
fraction = d - nearest
```

If a future workflow needs a separate 8-bit nearest-frame map, add
`--source-index`; do not overload the depth map.

## 4. Report

`--report <PATH>` writes JSON. This is the authoritative record of what the
run did. Schema version 1:

```json
{
  "schema_version": 1,
  "tool": { "name": "focus-stack", "version": "0.1.0", "git": "abc1234" },
  "inputs": [
    {
      "path": "a.tif",
      "sha256": "...",
      "width": 6000,
      "height": 4000,
      "bits": 16,
      "orientation": 1,
      "profile": "sRGB IEC61966-2.1"
    }
  ],
  "params": {
    "method": "auto",
    "align": "similarity",
    "reference": "auto",
    "estimation_radius": 16,
    "keep_detail": null,
    "detail_transfer": 0.7,
    "exposure_match": "off",
    "compression": "lzw",
    "output_profile": "same",
    "seed": 0,
    "threads": null
  },
  "alignment": {
    "reference_index": 0,
    "transforms": [
      { "a": 1.00001, "b": -0.00002, "tx": 0.42, "ty": -1.13 }
    ],
    "residual_px": { "median": 0.04, "p95": 0.11 },
    "correlation": [1.0, 0.998, 0.997, 0.996],
    "warnings": []
  },
  "focus": {
    "levels": 6,
    "noise_sigma": [[0.0012, 0.0004, 0.0002]],
    "threshold": 0.0031,
    "confident_pct": 0.74,
    "coverage_gap_pct": 0.02,
    "coverage_boxes": []
  },
  "method": { "chosen": "hybrid", "reason": "confident_pct=0.74" },
  "output": {
    "path": "out.tif",
    "sha256": "...",
    "clipped_pct": 0.0,
    "profile": "sRGB IEC61966-2.1"
  },
  "timing_ms": { "load": 320, "align": 410, "focus": 880, "stack": 1900, "encode": 260 },
  "warnings": []
}
```

Rules:
- No wall-clock timestamp by default; add `created_at` only with `--stamp`, so
  reports are deterministic.
- `timing_ms` is emitted only with `--timings`; it is diagnostic and breaks
  byte-identical reports.
- Paths are as given on the command line, not canonicalized.
- Hashes are SHA-256 of the file bytes (inputs) and of the output image
  (decoded pixels or file bytes; document which — use file bytes).
- `warnings` is duplicated at the top level and per stage for convenience;
  the top level is the union.
- Unknown fields must be ignored by consumers.

## 5. Metadata

Main image:
- ICC: exact original bytes (`--output-profile same`).
- XMP (tag 700): raw copy.
- IPTC (tag 33723): raw copy.
- EXIF: common fields copied; MakerNote dropped; `--strip-metadata` keeps only
  ICC, orientation, resolution, Software.
- `Orientation = 1`.
- `Software = "focus-stack <version>"`.
- Resolution: copied from the reference frame.

Depth map:
- Minimal: width, height, bits, photometric gray, `ImageDescription`,
  `Software`. No EXIF.

Rationale: the main image is an editing intermediate; the report is the
machine-readable record.

## 6. Retouch format

Command:

```text
focus-stack retouch \
  --base out.tif \
  --sources a.tif b.tif c.tif \
  --report run.json \
  --mask strokes.json \
  -o out-retouched.tif
```

Mask JSON v1:

```json
{
  "version": 1,
  "strokes": [
    { "points": [[100, 200], [140, 210], [180, 205]],
      "radius": 12, "feather": 3, "source": 2 }
  ],
  "polygons": [
    { "points": [[10, 10], [400, 10], [400, 300], [10, 300]],
      "feather": 5, "source": 1 }
  ],
  "masks": [
    { "path": "mask2.png", "source": 2, "feather": 0 }
  ]
}
```

Semantics:
- Coordinates are in output pixel space, origin top-left.
- `source` is the zero-based input index.
- `radius` is a half-width in pixels; strokes have round caps and joins.
- `feather` is a Gaussian blur radius applied to that mask (0 = hard edge).
- A PNG mask is grayscale; white = take the source, black = keep the base.
  It must match the output dimensions.
- Masks are applied **in array order**: each mask's alpha `a` updates the
  current result as `out = (1 - a) * out + a * source`. Later masks override
  earlier ones, which matches brush behavior and is fully deterministic.
- Compositing happens in linear light with float weights.
- `--report` must be from the same run as `--base`; re-align using the stored
  transforms and verify input SHA-256 hashes. Refuse on mismatch.
- The retouch is a separate output file; the base is never modified.

Why this instead of a GUI brush: it is diffable, scriptable, and reproducible.
A later GUI can emit the same JSON.

## 7. Reproducibility

- Inputs are identified by SHA-256 in the report.
- Parameters are recorded in full.
- Algorithm version is recorded (`tool.version`, `tool.git`).
- Deterministic reductions and fixed seeds.
- `params.threads` records an explicit `--threads` value; `null` means the
  default pool (all cores).
- Two runs with the same thread count produce byte-identical images and
  reports (no timestamps, no `--timings`).
- Across thread counts, pixel differences are ≤ 1 LSB; report the tolerance.
  Parallel workers write disjoint pixels, so no floating-point reduction
  crosses threads.
- `--stamp` and `--timings` are the only sources of nondeterminism; both are
  off by default.

A run is reproducible from `(inputs, report.params)` alone, except for the
tool version. Pin the binary version when archiving.
