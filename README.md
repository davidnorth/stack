# focus-stack

Professional focus stacking for 3–4 16-bit TIFF frames, as a macOS Apple Silicon CLI.

- Input: 16-bit RGB TIFF from Lightroom, tripod captures of static scenes.
- Output: 16-bit TIFF, linear-light pipeline, ICC preserved.
- Engines: PMax (max detail), DMap (depth map), Hybrid (default).

**Status:** planning. See [`plan.md`](plan.md) for the session-by-session build plan.

## Documentation

| File | Contents |
|---|---|
| [`plan.md`](plan.md) | Roadmap, checkboxes, quality gates |
| [`docs/architecture.md`](docs/architecture.md) | Modules, data model, memory |
| [`docs/io-and-color.md`](docs/io-and-color.md) | TIFF, ICC, Lightroom specifics |
| [`docs/alignment.md`](docs/alignment.md) | Phase correlation, ECC, warping |
| [`docs/focus-measure.md`](docs/focus-measure.md) | Pyramids, noise, confidence |
| [`docs/stacking.md`](docs/stacking.md) | PMax, DMap, hybrid |
| [`docs/outputs.md`](docs/outputs.md) | Depth map, report, retouch format |
| [`docs/cli.md`](docs/cli.md) | Command-line contract |
| [`docs/quality.md`](docs/quality.md) | Synthetic ground truth, metrics |
| [`docs/decisions.md`](docs/decisions.md) | Decision log |
| [`docs/agent-workflow.md`](docs/agent-workflow.md) | How to run each session |
| [`docs/ralph.md`](docs/ralph.md) | Automated one-session-per-stage loop |
| [`docs/session-log.md`](docs/session-log.md) | Append-only progress log |
| [`docs/quality-log.md`](docs/quality-log.md) | Synthetic gates and A/B results |
| [`docs/performance.md`](docs/performance.md) | Benchmarks and memory |

## Build loop

The plan is designed to run as a structured Ralph loop: one fresh OpenCode
session per checkbox, verified and committed.

```bash
./scripts/ralph.py --list
./scripts/ralph.py --dry-run --only S0.1
./scripts/ralph.py --max-stages 1
```

See [`docs/ralph.md`](docs/ralph.md) for prerequisites, safety, state, and
troubleshooting.

