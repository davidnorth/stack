# Performance

Filled in by S7.4. Record release-build numbers on Apple Silicon.

## End-to-end

| Case | Target | Measured |
|---|---|---|
| 4 × 24 MP, hybrid | ≤ 8 s | — |
| 4 × 24 MP, peak RSS | ≤ 4 GB | — |
| 4 × 60 MP, hybrid | ≤ 25 s | — |
| 4 × 60 MP, peak RSS | ≤ 8 GB | — |

## Stage budgets (24 MP)

| Stage | Target | Measured |
|---|---|---|
| load | 0.5 s | — |
| align | 0.5 s | — |
| focus | 1.0 s | — |
| stack | 3.0 s | — |
| encode | 0.5 s | — |

## Method

- `cargo build --release`
- `criterion` benches in `benches/`
- Peak RSS via `/usr/bin/time -l`
- Report the machine (chip, RAM, macOS version), commit hash, and thread count.
