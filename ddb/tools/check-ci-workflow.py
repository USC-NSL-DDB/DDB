#!/usr/bin/env python3
"""Enforce the reliability and coverage invariants of the core CI workflow."""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = REPOSITORY_ROOT / ".github/workflows/rust-check.yml"
DOCS_WORKFLOW = REPOSITORY_ROOT / ".github/workflows/api-docs-pages.yml"

EXPECTED_JOBS = {
    "api-contracts",
    "rust-quality",
    "rust-tests",
    "api-integration",
    "tui-integration",
    "required",
}

REQUIRED_GATES = {
    "Check formatting",
    "Check API schema and generated contracts",
    "Check and test the TypeScript SDK",
    "Check, test, and package the Python SDK",
    "Validate generated API specifications",
    "Validate examples and captured runtime payloads against specifications",
    "Check Protobuf schema compatibility",
    "Check OpenAPI compatibility",
    "Check AsyncAPI compatibility",
    "Check every target and feature",
    "Lint DDB targets",
    "Lint public API and extension crates",
    "Run workspace tests",
    "Verify optimized thread state transitions",
    "Run public API conformance against DDB Mock",
    "Run API deployment security and graceful-shutdown tests",
    "Test public extension authoring surface",
    "Run TypeScript and Python SDKs against DDB Mock",
    "Reproduce public API release artifacts",
    "Run DDB all-feature tests",
    "Run API lifecycle soak",
    "Check, test, lint, and build ddb-tui",
    "Build DDB for TUI PTY tests",
    "Build debuggable real fixture for TUI PTY tests",
    "Run ddb-tui Mock, GDB, and LLDB PTY workflows",
    "Run managed DDB process lifecycle tests",
    "Build and smoke-test paired DDB/ddb-tui artifact",
    "Enforce ddb tui dispatcher p95 overhead",
    "Retain paired release candidate",
}


def job_blocks(text: str) -> dict[str, str]:
    jobs = text.split("\njobs:\n", maxsplit=1)[1]
    matches = list(re.finditer(r"(?m)^  ([a-z][a-z0-9-]*):\n", jobs))
    return {
        match.group(1): jobs[match.start() : matches[index + 1].start()]
        if index + 1 < len(matches)
        else jobs[match.start() :]
        for index, match in enumerate(matches)
    }


def main() -> int:
    text = WORKFLOW.read_text(encoding="utf-8")
    docs_text = DOCS_WORKFLOW.read_text(encoding="utf-8")
    errors: list[str] = []

    if "'ddb/docs-site/**'" in text:
        errors.append("portal-only changes must not trigger the full core workflow")
    if "cancel-in-progress: true" not in text:
        errors.append("core workflow must cancel superseded runs on the same ref")
    if "cancel-in-progress: true" not in docs_text:
        errors.append("documentation workflow must cancel superseded runs on the same ref")
    installer = "bash ddb/tools/install-ci-packages.sh"
    if installer not in text:
        errors.append("system packages must use the bounded CI installer")
    if "sudo apt-get" in text:
        errors.append("apt commands must be centralized in install-ci-packages.sh")

    blocks = job_blocks(text)
    if set(blocks) != EXPECTED_JOBS:
        errors.append(
            "parallel job set drifted: expected "
            + repr(sorted(EXPECTED_JOBS))
            + ", got "
            + repr(sorted(blocks))
        )
    for name, block in blocks.items():
        timeout = re.search(r"(?m)^    timeout-minutes: ([0-9]+)$", block)
        if not timeout:
            errors.append(f"job {name} must define timeout-minutes")
        elif int(timeout.group(1)) > 60:
            errors.append(f"job {name} timeout exceeds 60 minutes")

    install_steps = text.count(installer)
    bounded_steps = text.count("        timeout-minutes: 5")
    if install_steps != bounded_steps:
        errors.append(
            f"all package installs need five-minute step timeouts: "
            f"{install_steps} installers, {bounded_steps} timeouts"
        )

    step_names = set(re.findall(r"(?m)^      - name: (.+)$", text))
    missing_gates = sorted(REQUIRED_GATES - step_names)
    if missing_gates:
        errors.append("workflow lost required gates: " + ", ".join(missing_gates))

    if errors:
        for error in errors:
            print(f"CI policy violation: {error}", file=sys.stderr)
        return 1

    print("CI workflow policy is valid")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
