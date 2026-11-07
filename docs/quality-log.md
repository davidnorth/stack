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
