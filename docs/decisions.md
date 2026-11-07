# Decision log

Append-only. Each decision records context, the choice, and consequences.
Format: `DNNN — title`.

---

## D001 — Rust CLI, macOS Apple Silicon only

**Context:** Numeric image pipeline, 24–60 MP, needs deterministic performance
and a single distributable binary. Windows/Linux are not required.

**Decision:** Rust 2021, `clap` CLI, `aarch64-apple-darwin` target only.

**Consequences:** Fast, memory-controlled, easy to package. No cross-platform
CI matrix. A future port is possible but untested.

---

## D002 — 16-bit linear-light pipeline, ICC preserved

**Context:** Inputs are 16-bit Lightroom TIFFs; output is an editing
intermediate. Blending 16-bit data in gamma space can band and shift tone.

**Decision:** Decode to linear sRGB `f32`, blend in linear, encode at the end.
Preserve the input ICC bytes by default; convert only on request.

**Consequences:** Correct light math and no banding. ICC handling adds a
dependency (`lcms2`). Unknown profiles must error rather than silently assume.

---

## D003 — Encoded luma for alignment and focus, linear for blending

**Context:** Focus measures correlate better with perceptual contrast; linear
values over-weight highlights. Blending requires linear light.

**Decision:** Alignment and focus measurement use sRGB-encoded luma. Stacking
and all final math use linear RGB.

**Consequences:** Two luminance representations exist; name them explicitly in
code (`encoded_luma`, `linear_luma`) to avoid mix-ups.

---

## D004 — Similarity alignment by default

**Context:** Tripod captures of static scenes change only focus. That produces
translation, small rotation, and small scale change from focus breathing.

**Decision:** 4-parameter similarity transform (translation, rotation, scale).
Homography, affine, and local warp are opt-in or deferred.

**Consequences:** Four degrees of freedom are identifiable and stable. Lens
distortion and parallax are not corrected; acceptable for the target captures.

---

## D005 — Hybrid engine is the default

**Context:** PMax preserves detail but is noisy and shifts color in flat
regions. DMap preserves color and noise but softens fine overlaps. Professionals
run both and retouch between them.

**Decision:** Default `--method auto` runs DMap and uses PMax detail where
confidence is high, with low frequencies anchored to DMap.

**Consequences:** Best overall quality; more code and parameters. Explicit
`pmax`/`dmap` remain for diagnosis and retouching.

---

## D006 — Automatic DMap threshold with manual override

**Context:** Zerene's contrast-threshold slider is effective but manual. The
threshold separates trustworthy detail from noise-dominated flat regions.

**Decision:** Automatic threshold from the noise floor plus Otsu on
`log(Emax)`, capped at `0.1 * p99`. `--keep-detail` sets a percentile override.

**Consequences:** Good defaults without a GUI. The automatic threshold can be
wrong on unusual subjects; the override and the report expose it.

---

## D007 — Depth map is 16-bit; source map is an alias

**Context:** Retouching needs to know which frame contributed each pixel,
including fractional transitions. With `N <= 4`, 16 bits encode both index and
fraction.

**Decision:** One 16-bit grayscale depth map, `d/(N-1)*65535`. `--source-map`
writes the same artifact in v1.

**Consequences:** Simple, compact, enough precision. A separate nearest-frame
map can be added later without breaking the format.

---

## D008 — Scriptable retouch, no GUI

**Context:** Every professional stacker has a retouch brush. A GUI is a large
scope increase and hard to make reproducible.

**Decision:** Retouch via JSON strokes/polygons and PNG masks, applied by a
subcommand that re-aligns deterministically from the report.

**Consequences:** Diffable and automatable; covers most retouch needs. A later
GUI can emit the same JSON. Non-rigid warping of moving subjects is not
supported.

---

## D009 — No motion handling and no `inspect` in v1

**Context:** Target captures are tripod, static scenes. Motion masking and a
pre-flight inspection command are valuable but orthogonal to the core quality
work.

**Decision:** Defer both. Keep the pipeline open to a confidence multiplier so
motion masking can be added without redesign.

**Consequences:** Windy scenes and bad focus steps are reported as warnings, not
fixed. Revisit after v0.1.

---

## D010 — Deterministic output, no timestamps by default

**Context:** Reproducibility is a professional requirement and a testing aid.

**Decision:** No wall-clock in outputs; fixed seed; deterministic reductions.
`--stamp` opts into a timestamp. Cross-thread differences must be ≤ 1 LSB.

**Consequences:** Byte-identical reruns at the same thread count. Rayon
reductions must be written carefully; no float atomics.

---

## D011 — `tiff` crate first, internal writer as fallback

**Context:** The Rust `tiff` crate covers most decoding, but ICC extra-tag
writing and some metadata behavior need verification.

**Decision:** Use `tiff` for read/write in S0.2–S0.3. If ICC or predictor tags
cannot be written correctly, implement a minimal uncompressed/Deflate writer.

**Consequences:** Pure-Rust build, no system libtiff. S0.3 must prove the
round-trip gate before later work depends on it.

---

## D012 — Bundled lcms2, unknown profiles error

**Context:** Lightroom can embed sRGB, Display P3, Adobe RGB, or ProPhoto.
Silently assuming sRGB would produce wrong color.

**Decision:** Use `lcms2` with the bundled build. Convert known profiles to
linear sRGB. Error on unknown/absent profiles unless `--assume-profile` is
given.

**Consequences:** Correct color, no Homebrew dependency, slightly longer build.
The error path is intentional.

---

## D013 — No HDR or exposure blending

**Context:** The tool stacks focus, not exposure. Exposure differences are
defects in a focus stack.

**Decision:** Optional robust gain matching, no HDR merge, no tone mapping.

**Consequences:** Clear product boundary. Flash/exposure variation is
normalized, not creatively blended.

---

## D014 — Output defaults: 16-bit, LZW, crop

**Context:** Output is an editing intermediate for Lightroom/Photoshop.

**Decision:** 16-bit RGB, LZW compression, ICC preserved, cropped to the valid
alignment rectangle, orientation 1.

**Consequences:** Widely compatible and lossless. `--keep-borders`, `--bits 8`,
and `--compression deflate` cover other workflows.

---

## D015 — No deconvolution in v1

**Context:** Deconvolution could recover detail from defocused frames, but it
amplifies noise and needs an accurate PSF per depth.

**Decision:** Selection-based stacking only. PMax detail transfer is the
maximum intervention.

**Consequences:** Honest results, no ringing artifacts. Revisit only with a
measured PSF and a strong quality case.
