#!/usr/bin/env python3
"""Build one call-depth table from same-pause DBT result directories."""

from __future__ import annotations

import argparse
import csv
import json
import statistics
from pathlib import Path

from analyze_latency import percentile, print_table


def parse_run(value: str) -> tuple[int, Path]:
    try:
        depth_text, path_text = value.split("=", 1)
        return int(depth_text), Path(path_text).expanduser().resolve()
    except (ValueError, TypeError) as exc:
        raise argparse.ArgumentTypeError("use DEPTH=RESULT_DIRECTORY") from exc


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "run",
        nargs="+",
        type=parse_run,
        help="One DEPTH=RESULT_DIRECTORY entry per call depth.",
    )
    parser.add_argument("--output", type=Path, default=Path("call-depth-summary.csv"))
    args = parser.parse_args()

    table = []
    for expected_depth, directory in sorted(args.run):
        metadata = json.loads((directory / "metadata.json").read_text())
        with (directory / "same-pause-dbt.csv").open(newline="") as stream:
            samples = list(csv.DictReader(stream))
        invalid = [
            row
            for row in samples
            if int(row["rpc_boundaries"]) != expected_depth
            or int(row["new_stop_events"]) != 0
        ]
        if invalid:
            raise SystemExit(f"{directory}: {len(invalid)} invalid DBT samples")
        # Repeat 1 primes the same-pause command path. Keep it in the raw CSV
        # for auditability, but never expose it in the aggregate results.
        selected = samples[1:]
        values = [float(row["latency_ms"]) for row in selected]
        if not values:
            raise SystemExit(f"{directory}: no selected latency samples")
        table.append(
            {
                "rpc_boundaries": expected_depth,
                "process_count": int(metadata["expected_sessions"]),
                "count": len(values),
                "mean_ms": f"{statistics.fmean(values):.3f}",
                "median_ms": f"{statistics.median(values):.3f}",
                "stddev_ms": f"{statistics.pstdev(values):.3f}",
                "p95_ms": f"{percentile(values, 95):.3f}",
                "p99_ms": f"{percentile(values, 99):.3f}",
                "min_ms": f"{min(values):.3f}",
                "max_ms": f"{max(values):.3f}",
            }
        )

    args.output = args.output.expanduser().resolve()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(table[0]))
        writer.writeheader()
        writer.writerows(table)

    print_table(table)
    if len(table) >= 2:
        xs = [float(row["rpc_boundaries"]) for row in table]
        ys = [float(row["mean_ms"]) for row in table]
        x_mean = statistics.fmean(xs)
        y_mean = statistics.fmean(ys)
        slope = sum((x - x_mean) * (y - y_mean) for x, y in zip(xs, ys)) / sum(
            (x - x_mean) ** 2 for x in xs
        )
        intercept = y_mean - slope * x_mean
        print(f"\nLinear fit: latency_ms = {intercept:.3f} + {slope:.3f} * rpc_boundaries")
    print(f"\nWrote {args.output}")


if __name__ == "__main__":
    main()
