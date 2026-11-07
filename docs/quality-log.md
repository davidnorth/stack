# Quality log

Results of synthetic gates and real-stack blind A/B. Append-only.

## Template

```markdown
## <date> — <milestone>

### Synthetic
| Fixture | PSNR | SSIM | Detail | Halo | Noise | ΔE00 | Coverage |
|---|---|---|---|---|---|---|---|

### Real stacks
| Stack | Method | Sharpness | Artifacts | Naturalness | Color | Notes |
|---|---|---|---|---|---|---|

### Open issues
- ...
```

---

## 2026-11-07 — S5.4 tuning set (blocked)

### Synthetic
| Fixture | PSNR | SSIM | Detail | Halo | Noise | ΔE00 | Coverage |
|---|---|---|---|---|---|---|---|
| _not re-measured_ | | | | | | | |

Standard gates pass on the current tree (`fmt`, `clippy`, `test`), so there is
no known synthetic regression; numeric per-fixture metrics are deferred to
S7.1, which adds `focus-stack eval`.

### Real stacks
| Stack | Method | Sharpness | Artifacts | Naturalness | Color | Notes |
|---|---|---|---|---|---|---|
| _none_ | | | | | | no tuning set available |

### Open issues
- **Blocked:** `fixtures/real/` does not exist, so the 10-stack tuning set
  cannot be run. No Photoshop/Helicon/Zerene baseline outputs are present and
  no human raters are available in this session, so the blind A/B scores
  required by the Done criterion cannot be produced.
- Default constants were therefore **not** changed; tuning them without
  measured evidence would be a silent guess.
- Next: supply the tuning set and baseline renders, run the §5 protocol, then
  tune `HybridParameters` (detail/halo/mask) and the auto threshold.

---

## 2026-11-07 — S7.3 held-out set (blocked)

### Synthetic
| Fixture | PSNR | SSIM | Detail | Halo | Noise | ΔE00 | Coverage |
|---|---|---|---|---|---|---|---|
| _not re-measured_ | | | | | | | |

S7.3 is a real-stack milestone; the tiny synthetic suite is unchanged and
`cargo test` covers it (see the S7.2 regression manifest).

### Real stacks
| Stack | Method | Sharpness | Artifacts | Naturalness | Color | Notes |
|---|---|---|---|---|---|---|
| _none_ | | | | | | no held-out set available |

The §5 protocol cannot be run in this session:

- `fixtures/real/` does not exist. The tree contains only `fixtures/io/`,
  `fixtures/synthetic/`, and `fixtures/manifest.toml`; no real landscape
  images are present anywhere in the repository.
- No Photoshop Auto-Blend, Helicon Focus, Zerene PMax, or Zerene DMap baseline
  renders exist, and no human raters are available.
- Scoring held-out stacks is impossible without those artifacts, and
  synthesizing scores would violate the blind-A/B protocol.
- The tuning stacks from S5.4 must not be reused; none were available either.

### Open issues
- **Blocked:** the held-out 10-stack set, commercial baselines, and two raters
  required by `docs/quality.md` §5 do not exist, so no formal A/B results can
  be reported. The checkbox stays unchecked.
- **Prioritized artifacts to unblock S7.3:**
  1. **Held-out stacks:** 10 real landscape stacks (3–4 frames, 24 MP) that
     cover near/far, sky, water, foliage, a hard ridge line, low contrast, and
     high detail; store under `fixtures/real/` with provenance/licensing and
     keep them disjoint from the S5.4 tuning set.
  2. **Commercial baselines:** Photoshop Auto-Blend, Helicon Focus, Zerene
     PMax, and Zerene DMap renders for every held-out stack, anonymized and
     metadata-stripped.
  3. **Our renders:** PMax, DMap, and Hybrid outputs from the same stacks.
     The pipeline still falls back to the reference for `auto`/`hybrid` and
     has no alignment wiring (S6.4 follow-up), so that wiring must land before
     these outputs are meaningful.
  4. **Blind rating:** two independent raters scoring 1–5 for sharpness,
     artifacts, naturalness, and color on full frames plus 100% near/mid/far
     crops, with randomized filenames.
  5. **Artifact triage:** record every catastrophic failure (ghosting, double
     edges, color blotches) as a tracked issue with a prioritized fix.
