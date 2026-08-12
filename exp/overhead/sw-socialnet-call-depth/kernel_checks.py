#!/usr/bin/env python3
"""Kernel-level tracer verification shared by DDB latency experiments."""

from __future__ import annotations

import csv
import json
import subprocess
from pathlib import Path


def verify_tracers(
    kubeconfig: Path,
    output_dir: Path,
    expected: int,
    *,
    namespace: str,
    selector: str,
    require_stopped: bool = False,
) -> list[dict[str, str | int]]:
    base = ["kubectl", "--kubeconfig", str(kubeconfig), "-n", namespace]
    pod_data = json.loads(
        subprocess.check_output(
            base
            + [
                "get",
                "pods",
                "-l",
                selector,
                "-o",
                "json",
            ],
            text=True,
        )
    )
    rows: list[dict[str, str | int]] = []
    probe = r'''for s in /proc/[0-9]*/status; do
  if grep -q '^Name:[[:space:]]*server.out$' "$s"; then
    pid=${s#/proc/}; pid=${pid%/status}
    state=$(awk '/^State:/{print $2}' "$s")
    tracer=$(awk '/^TracerPid:/{print $2}' "$s")
    printf '%s %s %s\n' "$pid" "$state" "$tracer"
    exit 0
  fi
done
exit 1'''
    for pod in pod_data["items"]:
        name = pod["metadata"]["name"]
        owned = [
            item["name"]
            for item in pod["spec"].get("ephemeralContainers", [])
            if item.get("name", "").startswith("ssh-debugger-")
        ]
        if len(owned) != 1:
            raise RuntimeError(
                f"{name} has {len(owned)} recipe-owned debugger sidecars; expected 1"
            )
        container = owned[0]
        result = subprocess.check_output(
            base + ["exec", name, "-c", container, "--", "sh", "-lc", probe],
            text=True,
        ).strip()
        pid, state, tracer = result.split()
        rows.append(
            {
                "pod": name,
                "container": container,
                "pid": int(pid),
                "state": state,
                "tracer_pid": int(tracer),
            }
        )
    if len(rows) != expected:
        raise RuntimeError(f"expected {expected} traced pods, found {len(rows)}")
    untraced = [row["pod"] for row in rows if row["tracer_pid"] == 0]
    if untraced:
        raise RuntimeError(f"untraced processes: {untraced}")
    if require_stopped:
        running = [row["pod"] for row in rows if row["state"] not in {"t", "T"}]
        if running:
            raise RuntimeError(f"traced but not stopped processes: {running}")
    with (output_dir / "tracer-pids.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=rows[0].keys())
        writer.writeheader()
        writer.writerows(rows)
    return rows
