#!/usr/bin/env python3
"""Fail when a Criterion benchmark exceeds its committed performance limit.

The limits in ``thresholds.json`` are generous multiples of the reference
medians recorded in ``docs/performance.md``. They are a gross-regression gate
for the scheduled CI job, not the 20 % review policy from
``docs/quality.md`` section 6.
"""

from __future__ import annotations

import argparse
import json
import pathlib
import sys


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--criterion-dir",
        type=pathlib.Path,
        default=pathlib.Path("target/criterion"),
        help="Criterion output directory (default: target/criterion)",
    )
    parser.add_argument(
        "--limits",
        type=pathlib.Path,
        default=pathlib.Path(__file__).with_name("thresholds.json"),
        help="JSON file with limits in nanoseconds",
    )
    parser.add_argument(
        "--factor",
        type=float,
        default=1.0,
        help="Extra multiplier applied to every limit (default: 1.0)",
    )
    parser.add_argument(
        "--allow-missing",
        action="store_true",
        help="Do not fail when a limited benchmark produced no result",
    )
    return parser.parse_args()


def measured_means(criterion_dir: pathlib.Path) -> dict[str, float]:
    results: dict[str, float] = {}
    for estimates in criterion_dir.rglob("estimates.json"):
        if estimates.parent.name != "new":
            continue
        key = estimates.parent.parent.name
        data = json.loads(estimates.read_text())
        results[key] = float(data["mean"]["point_estimate"])
    return results


def main() -> int:
    args = parse_args()
    limits = json.loads(args.limits.read_text())["limits_ns"]
    measured = measured_means(args.criterion_dir)

    if not measured:
        print(f"no Criterion results under {args.criterion_dir}", file=sys.stderr)
        return 1

    missing = sorted(set(limits) - set(measured))
    failed = False

    print(f"{'benchmark':32s} {'measured':>12s} {'limit':>12s}  status")
    for key in sorted(limits):
        limit = float(limits[key]) * args.factor
        if key not in measured:
            print(f"{key:32s} {'missing':>12s} {limit / 1e9:10.3f} s  MISSING")
            failed = failed or not args.allow_missing
            continue
        value = measured[key]
        status = "ok" if value <= limit else "REGRESSION"
        failed = failed or value > limit
        print(f"{key:32s} {value / 1e9:10.3f} s {limit / 1e9:10.3f} s  {status}")

    extra = sorted(set(measured) - set(limits))
    for key in extra:
        print(f"{key:32s} {measured[key] / 1e9:10.3f} s {'unlimited':>12s}  (no limit)")

    if failed:
        print("\nperformance limits exceeded", file=sys.stderr)
        return 1
    print("\nall benchmarks within limits")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
