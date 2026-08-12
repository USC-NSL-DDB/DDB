#!/usr/bin/env python3
"""Inject the recipe-owned SSH debugger using kubectl; no Python packages needed."""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
from uuid import uuid4


DEFAULT_IMAGE = (
    "docker.io/h21565897/distributeddebugger@"
    "sha256:0409e92698a87ab5370091b3a32953c2fea39b55cc096e57dc2607f4f2aa5ebc"
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--kubeconfig", required=True)
    parser.add_argument("--namespace", default="default")
    parser.add_argument("--label-key", default="serviceweaver/app")
    parser.add_argument("--label-value", "--label", dest="label_value", required=True)
    parser.add_argument("--expected", type=int, required=True)
    parser.add_argument("--image", default=DEFAULT_IMAGE)
    parser.add_argument("--target-container", default="serviceweaver")
    parser.add_argument("--debugger-prefix", default="ssh-debugger-")
    args = parser.parse_args()

    base = [
        "kubectl",
        "--kubeconfig",
        args.kubeconfig,
        "-n",
        args.namespace,
    ]
    selector = f"{args.label_key}={args.label_value}"
    pods = json.loads(
        subprocess.check_output(
            base + ["get", "pods", "-l", selector, "-o", "json"],
            text=True,
        )
    ).get("items", [])
    active = [
        pod for pod in pods
        if not pod.get("metadata", {}).get("deletionTimestamp")
    ]
    if len(active) != args.expected:
        print(
            f"expected {args.expected} active pods matching {selector}, "
            f"found {len(active)}",
            file=sys.stderr,
        )
        return 1

    injected = skipped = 0
    for pod in active:
        name = pod["metadata"]["name"]
        existing = [
            item.get("name", "")
            for item in pod.get("spec", {}).get("ephemeralContainers", [])
            if item.get("name", "").startswith(args.debugger_prefix)
        ]
        if existing:
            print(f"  = {name} ({existing[0]} already injected)")
            skipped += 1
            continue

        debugger_name = f"{args.debugger_prefix}{uuid4().hex[:12]}"
        subprocess.run(
            base
            + [
                "debug",
                f"pod/{name}",
                f"--image={args.image}",
                f"--target={args.target_container}",
                f"--container={debugger_name}",
                "--profile=sysadmin",
                "--image-pull-policy=IfNotPresent",
                "--stdin",
                "--tty",
                "--attach=false",
                "--quiet",
            ],
            check=True,
        )
        print(f"  + {name} ({debugger_name})")
        injected += 1

    print(f"Injected: {injected} | already present: {skipped}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
