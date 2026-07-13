#!/usr/bin/env python3
"""Summarize DDB command-latency CSV files without third-party packages."""

from __future__ import annotations

import argparse
import csv
import statistics
from collections import defaultdict
from pathlib import Path


def percentile(values: list[float], pct: float) -> float:
    """Paper-compatible lower-rank percentile.

    The original artifact notebook used int(p * (n - 1)), so retain that
    convention instead of interpolating between neighboring samples.
    """
    ordered = sorted(values)
    return ordered[int((pct / 100.0) * (len(ordered) - 1))]


def load(paths: list[Path], include_warmup: bool) -> dict[str, list[float]]:
    grouped: dict[str, list[float]] = defaultdict(list)
    for path in paths:
        with path.open(newline="") as stream:
            for row in csv.DictReader(stream):
                if not include_warmup and row.get("phase") != "measure":
                    continue
                if row.get("status") != "ok":
                    continue
                grouped[row["command_name"]].append(float(row["latency_ms"]))
    return grouped


def summarize(grouped: dict[str, list[float]]) -> list[dict[str, str | int]]:
    rows = []
    for command, values in sorted(grouped.items()):
        rows.append(
            {
                "command": command,
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
    return rows


def print_table(rows: list[dict[str, str | int]]) -> None:
    if not rows:
        print("No measured samples found.")
        return
    headers = list(rows[0])
    widths = {key: max(len(key), *(len(str(row[key])) for row in rows)) for key in headers}
    print("  ".join(key.ljust(widths[key]) for key in headers))
    print("  ".join("-" * widths[key] for key in headers))
    for row in rows:
        print("  ".join(str(row[key]).ljust(widths[key]) for key in headers))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("csv", nargs="+", type=Path)
    parser.add_argument("--include-warmup", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    rows = summarize(load(args.csv, args.include_warmup))
    print_table(rows)
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        with args.output.open("w", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=list(rows[0]) if rows else ["command"])
            writer.writeheader()
            writer.writerows(rows)


if __name__ == "__main__":
    main()
