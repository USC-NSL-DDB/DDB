#!/usr/bin/env python3
"""Check kernel tracer state for every ServiceWeaver process."""

from __future__ import annotations

import argparse
import json
import subprocess
from pathlib import Path


PROBE = r'''for s in /proc/[0-9]*/status; do
  if grep -q '^Name:[[:space:]]*server.out$' "$s"; then
    pid=${s#/proc/}; pid=${pid%/status}
    state=$(awk '/^State:/{print $2}' "$s")
    tracer=$(awk '/^TracerPid:/{print $2}' "$s")
    printf '%s %s %s\n' "$pid" "$state" "$tracer"
    exit 0
  fi
done
exit 1'''


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kubeconfig", type=Path, required=True)
    parser.add_argument("--namespace", default="default")
    parser.add_argument("--selector", required=True)
    parser.add_argument("--expected", type=int, required=True)
    parser.add_argument("--expect", choices=["detached", "stopped", "attached"], required=True)
    parser.add_argument("--quiet", action="store_true")
    args = parser.parse_args()

    base = [
        "kubectl",
        "--kubeconfig",
        str(args.kubeconfig.expanduser().resolve()),
        "-n",
        args.namespace,
    ]
    pods = json.loads(
        subprocess.check_output(base + ["get", "pods", "-l", args.selector, "-o", "json"], text=True)
    )["items"]
    if len(pods) != args.expected:
        raise SystemExit(f"expected {args.expected} pods, found {len(pods)}")

    rows = []
    for pod in pods:
        name = pod["metadata"]["name"]
        owned = [
            item["name"]
            for item in pod["spec"].get("ephemeralContainers", [])
            if item.get("name", "").startswith("ssh-debugger-")
        ]
        if len(owned) != 1:
            raise SystemExit(
                f"{name}: expected exactly one recipe-owned ssh-debugger-* sidecar, "
                f"found {len(owned)}"
            )
        output = subprocess.check_output(
            base + ["exec", name, "-c", owned[0], "--", "sh", "-lc", PROBE],
            text=True,
        ).strip()
        pid, state, tracer = output.split()
        rows.append((name, int(pid), state, int(tracer)))

    if args.expect == "detached":
        bad = [row for row in rows if row[3] != 0]
    elif args.expect == "stopped":
        bad = [row for row in rows if row[3] == 0 or row[2] not in {"t", "T"}]
    else:
        bad = [row for row in rows if row[3] == 0]
    if bad:
        detail = ", ".join(f"{name}(state={state},tracer={tracer})" for name, _, state, tracer in bad)
        raise SystemExit(f"kernel process-state check failed: {detail}")
    if not args.quiet:
        print(f"Kernel process state: {len(rows)}/{args.expected} {args.expect}")


if __name__ == "__main__":
    main()
