#!/usr/bin/env python3
"""Recover paper-compatible command samples from an existing DDB debug log."""

from __future__ import annotations

import argparse
import csv
import re
from pathlib import Path

from ddb_session import clean_line, command_name, source_timestamp_ns


RECEIVED = re.compile(r"received cmd:\s*(?P<token>\d+)(?P<command>-[^\s]+(?:\s+.*)?)$")
OUTPUT = re.compile(
    r"(?:\boutput:\s*(?P<token>\d+)\^(?:done|running|error|exit|connected)\b"
    r"|\boutput from token:\s*(?P<legacy_token>\d+)\b)"
)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("log", type=Path)
    parser.add_argument("--output", type=Path, default=Path("samples-from-log.csv"))
    args = parser.parse_args()

    starts: dict[int, tuple[int, str]] = {}
    finishes: dict[int, int] = {}
    with args.log.open(errors="replace") as stream:
        for raw in stream:
            line = clean_line(raw)
            timestamp = source_timestamp_ns(line)
            if timestamp is None:
                continue
            received = RECEIVED.search(line)
            if received:
                token = int(received.group("token"))
                starts.setdefault(token, (timestamp, received.group("command").strip()))
            output = OUTPUT.search(line)
            if output:
                # Continue may emit one response per session; the original
                # notebook used the last token-matched output.
                token = output.group("token") or output.group("legacy_token")
                finishes[int(token)] = timestamp

    args.output.parent.mkdir(parents=True, exist_ok=True)
    fields = [
        "sample",
        "phase",
        "command",
        "command_name",
        "token",
        "received_timestamp_ns",
        "completed_timestamp_ns",
        "latency_ms",
        "status",
    ]
    matched = 0
    with args.output.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        for sample, token in enumerate(sorted(starts)):
            if token not in finishes:
                continue
            started, command = starts[token]
            completed = finishes[token]
            writer.writerow(
                {
                    "sample": sample,
                    "phase": "measure",
                    "command": command,
                    "command_name": command_name(command),
                    "token": token,
                    "received_timestamp_ns": started,
                    "completed_timestamp_ns": completed,
                    "latency_ms": (completed - started) / 1_000_000,
                    "status": "ok",
                }
            )
            matched += 1
    print(f"matched {matched}/{len(starts)} received commands -> {args.output}")


if __name__ == "__main__":
    main()
