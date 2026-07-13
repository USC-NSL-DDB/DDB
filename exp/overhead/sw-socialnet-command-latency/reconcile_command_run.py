#!/usr/bin/env python3
"""Recompute an all-thread command run from authoritative DDB log timestamps."""

from __future__ import annotations

import argparse
import csv
import json
import shutil
from pathlib import Path

from ddb_session import clean_line, source_timestamp_ns
from parse_ddb_log import OUTPUT, RECEIVED
from run_command_latency import write_csv, write_summaries


def log_timings(path: Path) -> dict[int, tuple[int, int]]:
    starts: dict[int, int] = {}
    finishes: dict[int, int] = {}
    with path.open(errors="replace") as stream:
        for raw in stream:
            line = clean_line(raw)
            timestamp = source_timestamp_ns(line)
            if timestamp is None:
                continue
            received = RECEIVED.search(line)
            if received:
                starts.setdefault(int(received.group("token")), timestamp)
            output = OUTPUT.search(line)
            if output:
                token = output.group("token") or output.group("legacy_token")
                finishes[int(token)] = timestamp
    return {
        token: (started, finishes[token])
        for token, started in starts.items()
        if token in finishes
    }


def read_csv(path: Path) -> list[dict]:
    with path.open(newline="") as stream:
        return list(csv.DictReader(stream))


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("result_dir", type=Path)
    args = parser.parse_args()
    result_dir = args.result_dir.expanduser().resolve()
    samples_path = result_dir / "dbt-thread-samples.csv"
    backup_path = result_dir / "dbt-thread-samples-online.csv"
    rows = read_csv(samples_path)
    inventory = read_csv(result_dir / "thread-inventory.csv")
    metadata = json.loads((result_dir / "metadata.json").read_text())
    timings = log_timings(result_dir / "ddb-console.log")

    missing = [row["token"] for row in rows if int(row["token"]) not in timings]
    if missing:
        raise SystemExit(f"missing timestamp pairs for {len(missing)} DBTs")
    old_values = [float(row["latency_ms"]) for row in rows]
    for row in rows:
        started, finished = timings[int(row["token"])]
        row["latency_ms"] = f"{(finished - started) / 1_000_000:.6f}"
    new_values = [float(row["latency_ms"]) for row in rows]

    if not backup_path.exists():
        shutil.copy2(samples_path, backup_path)
    write_csv(samples_path, rows, list(rows[0]))

    controller_path = result_dir / "samples.csv"
    controller_backup = result_dir / "samples-online.csv"
    controller_rows = read_csv(controller_path)
    if not controller_backup.exists():
        shutil.copy2(controller_path, controller_backup)
    for row in controller_rows:
        token = int(row["token"])
        if token not in timings:
            continue
        started, finished = timings[token]
        row["received_timestamp_ns"] = str(started)
        row["completed_timestamp_ns"] = str(finished)
        row["latency_ms"] = f"{(finished - started) / 1_000_000:.6f}"
    write_csv(controller_path, controller_rows, list(controller_rows[0]))
    overall = write_summaries(
        result_dir,
        inventory,
        rows,
        process_count=int(metadata["expected_sessions"]),
    )
    changed = sum(abs(old - new) > 0.001 for old, new in zip(old_values, new_values))
    max_delta = max(abs(old - new) for old, new in zip(old_values, new_values))
    print(f"reconciled {len(rows)} DBTs; changed >1us: {changed}; max delta: {max_delta:.3f} ms")
    if overall:
        print(overall[0])


if __name__ == "__main__":
    main()
