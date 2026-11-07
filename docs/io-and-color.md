# I/O and color

## 1. Input assumptions

Inputs are TIFF files exported from Lightroom Classic. Treat every detail as
unknown and read it from tags; do not assume sRGB or no compression.

Typical Lightroom export:
- RGB, unsigned 16-bit, chunky (`PlanarConfiguration = 1`), no alpha.
- Embedded ICC profile (sRGB, Display P3, Adobe RGB, or ProPhoto RGB).
- Compression None, LZW, or ZIP (Deflate), depending on export settings.
- EXIF/XMP retained; output sharpening usually off.
- Pixels are usually upright and `Orientation = 1`, but still honor the tag.

The conformance suite must cover every combination Lightroom can emit, plus
the variants other tools produce (tiled, PackBits, predictor).

## 2. TIFF feature matrix

| Feature | Tag | v1 |
|---|---|---|
| Photometric RGB | 262 = 2 | required |
| Photometric grayscale | 262 = 1 | read, replicate to RGB |
| Bits per sample | 258 | 8, 16 |
| Sample format | 339 | 1 (unsigned) required; 3 (float) read with warning and conversion |
| Samples per pixel | 277 | 3; 1 accepted; 4 accepted with alpha dropped and warned |
| Planar config | 284 | chunky required; planar read optional |
| Compression | 259 | 1, 5 (LZW), 8/32946 (Deflate), 32773 (PackBits) |
| Predictor | 317 | 1, 2 (horizontal differencing) |
| Strips/tiles | 273/279, 322/323 | both required |
| ICC profile | 34675 | preserve exact bytes |
| Orientation | 274 | 1–8 honored; output 1 |
| XMP | 700 | copy raw |
| IPTC | 33723 | copy raw |
| EXIF IFD | 34665 | policy below |
| BigTIFF | magic 43 | read; classic write |
| Multi-page | — | first page + warning |
| Resolution | 282/283/296 | copy |

## 3. Read path

1. Open and parse the header. Reject BigTIFF writes, not reads.
2. Read the first IFD. Validate photometric, sample format, samples per pixel.
3. Decode strips or tiles; undo predictor; decompress.
4. Convert to interleaved `u16` RGB. Grayscale is replicated. 8-bit is
   scaled by `257` (`v * 65535 / 255`), not by `256`.
5. Apply EXIF orientation to pixels and set the in-memory orientation to 1.
   Keep the original tag value in `FrameMeta` for the report.
6. Capture ICC bytes, XMP, IPTC, EXIF policy fields, resolution.
7. Validate that all inputs share width, height, bit depth, and profile hash.
   Mismatches are errors with the offending paths named.

Memory: decode directly into a preallocated `Vec<u16>`; never hold a second
full copy. For tiled files, decode tile-by-tile and copy into the destination.

## 4. Write path

1. Convert linear f32 output to the target profile and bit depth.
2. Write a classic TIFF with strips, `PlanarConfiguration = 1`,
   `SampleFormat = 1`, `Orientation = 1`.
3. Compression: `lzw` default; `deflate` and `none` options.
4. Write ICC tag 34675 with the exact original bytes when `--output-profile same`.
5. Write resolution tags copied from the reference frame.
6. Write metadata per §7.
7. Set `Software = "focus-stack <version>"`.
8. Quantize to `u16` with round-to-nearest-even. No dithering by default.

Order of operations matters: color conversion before quantization; metadata
after the pixel payload so offsets are known.

## 5. Round-trip invariant

`read -> write -> read` with no processing must be **pixel-identical** and
**ICC-identical** for `Orientation = 1` inputs. This is a hard gate in S0.3.

Documented exceptions:
- Inputs with `Orientation != 1` are baked upright, so raw pixel bytes differ
  even though the visual image is identical. Test these separately.
- 8-bit input is promoted to 16-bit for the round trip by default; use
  `--bits 8` to keep 8-bit output.
- Metadata that cannot be safely relocated (e.g., MakerNote inside EXIF) may
  be dropped; this is reported, not silent.

## 6. Color management

**Working space:** linear sRGB, D65, `f32`, values `[0, 1]`.

**sRGB transfer function:**

```text
decode: c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055)^2.4
encode: c <= 0.0031308 ? 12.92 * c : 1.055 * c^(1/2.4) - 0.055
```

**Profile handling (S0.4):**

| Input ICC | Behavior |
|---|---|
| sRGB / Display P3 / Adobe RGB / ProPhoto | convert to linear sRGB via lcms2 |
| linear sRGB | use directly |
| absent | assume sRGB, warn; `--assume-profile <path>` overrides |
| unknown/unsupported | error by default; `--assume-profile` required |

Use the `lcms2` crate with the bundled build so the binary does not depend on
Homebrew. If the bundled build is problematic, fall back to known-profile
transfer functions for sRGB/linear and error on everything else. Record the
decision.

**Focus measurement uses encoded luma**, not linear luma:

```text
Y' = 0.2126 R' + 0.7152 G' + 0.0722 B'      (R', G', B' encoded [0,1])
```

Reason: focus measures correlate better with perceptual contrast, and encoded
values compress highlights less aggressively than linear ones for this task.
Alignment also uses encoded luma. Blending uses linear RGB.

**Clipping:** after output conversion, count values outside `[0, 1]`, clamp,
and report the percentage. Do not silently wrap. Hybrid detail transfer applies
a soft clamp before reconstruction (see `docs/stacking.md` §4.1).

**Dithering:** not needed at 16 bits. Add `--dither` only if banding is ever
observed in 8-bit previews.

## 7. Metadata policy

- **ICC:** copy exact bytes by default. Convert only with `--output-profile`.
- **XMP (700):** copy raw; it is self-contained.
- **IPTC (33723):** copy raw.
- **EXIF:** v1 copies common fields (camera, lens, exposure, ISO, date) with a
  pure-Rust writer and drops MakerNote. `--strip-metadata` keeps only ICC,
  orientation, resolution, and Software. This is documented in the report.
- **Orientation:** always write 1 after baking.
- **Software:** `focus-stack <version>`.
- **Resolution:** copy from the reference frame.
- **Report sidecar:** the JSON report is the authoritative record of parameters
  and diagnostics; do not try to encode it into TIFF tags.

Rationale: output TIFFs are intermediates for further editing. ICC and
resolution matter; MakerNote does not.

## 8. Edge cases

| Case | Behavior |
|---|---|
| CMYK or Lab TIFF | error: unsupported |
| 32-bit float TIFF | read and convert with warning; optional later |
| 4 samples (alpha) | drop alpha with warning; unpremultiply if associated |
| Mixed bit depth across inputs | error; `--promote-8bit` not in v1 |
| Mixed ICC across inputs | error unless `--ignore-profile-mismatch` |
| Mixed dimensions | error, list sizes |
| Corrupt/truncated file | error with file name and TIFF error |
| Multi-page TIFF | read first page, warn |
| BigTIFF > 4 GB | read; output classic TIFF |
| Huge image > memory budget | error before allocation, suggest `--max-memory` |

## 9. Test fixtures

`fixtures/io/` must include:
- 16-bit uncompressed, LZW, Deflate, PackBits.
- Predictor 1 and 2.
- Striped and tiled.
- ICC: sRGB, linear, Adobe RGB, ProPhoto, Display P3, absent, malformed.
- Orientation 1, 6, 8.
- Grayscale 16-bit.
- 8-bit RGB.
- Truncated file and a CMYK file (expected failures).
- A real Lightroom 24 MP export.

Keep small files committed; put the 24 MP file behind
`scripts/fetch_fixtures.sh` if repository size matters.
