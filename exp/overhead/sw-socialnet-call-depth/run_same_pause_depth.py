#!/usr/bin/env python3
"""Measure repeated DBT commands while all DDB sessions remain paused."""

from __future__ import annotations

import argparse
import csv
import re
import statistics
import subprocess
import time
from pathlib import Path

from ddb_session import DdbSession, write_metadata
from kernel_checks import verify_tracers
from run_call_depth import BREAK_HIT, choose_group


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--breakpoint", required=True)
    parser.add_argument("--trigger", required=True)
    parser.add_argument(
        "--expected-boundaries",
        dest="expected_boundaries",
        type=int,
        required=True,
        help="Expected RPC boundary count; call depth is this value plus one.",
    )
    parser.add_argument("--ddb", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("--kubeconfig", type=Path, required=True)
    parser.add_argument("--namespace", required=True)
    parser.add_argument("--selector", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--expected-sessions", type=int, default=14)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--repetitions", type=int, default=30)
    parser.add_argument("--group-id", type=int)
    parser.add_argument("--api-port", type=int, default=5000)
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--echo-ddb", action="store_true")
    return parser.parse_args()


def start_trigger(command: str, log_path: Path) -> tuple[subprocess.Popen[str], object]:
    stream = log_path.open("w")
    process = subprocess.Popen(
        command, shell=True, stdout=stream, stderr=subprocess.STDOUT, text=True
    )
    return process, stream


def finish_trigger(process: subprocess.Popen[str], stream: object) -> None:
    try:
        process.wait(timeout=15)
    except subprocess.TimeoutExpired:
        process.terminate()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    stream.close()


def reach_cluster_pause(
    session: DdbSession,
    attached: set[int],
    trigger_command: str,
    trigger_log: Path,
    *,
    sample: int,
    phase: str,
    timeout: float,
) -> tuple[int, subprocess.Popen[str], object, float]:
    marker = session.mark()
    session.send("-exec-continue", sample=sample, phase=phase, timeout=timeout)
    trigger, stream = start_trigger(trigger_command, trigger_log)
    try:
        hit, _ = session.wait_for_pattern(BREAK_HIT, after=marker, timeout=timeout)
        thread = int(hit.group("thread"))
        breakpoint_session = int(hit.group("session"))
        if breakpoint_session not in attached:
            raise RuntimeError(f"breakpoint came from unknown session {breakpoint_session}")

        pause_marker = session.mark()
        pause, _ = session.send(
            "-exec-interrupt",
            sample=sample,
            phase=f"{phase}-cluster-pause",
            timeout=timeout,
        )
        stopped, final_stop = session.wait_for_stopped_sessions(
            attached,
            after=pause_marker,
            initially_stopped={breakpoint_session},
            timeout=timeout,
        )
        if stopped != attached:
            raise RuntimeError(f"not all sessions stopped: {sorted(stopped)}")
        pause_ms = (
            max(0.0, (final_stop.mono_ns - pause.submitted_ns) / 1_000_000)
            if final_stop is not None
            else 0.0
        )
        return thread, trigger, stream, pause_ms
    except Exception:
        finish_trigger(trigger, stream)
        raise


def main() -> None:
    args = parse_args()
    if args.expected_boundaries < 0:
        raise SystemExit("--expected-boundaries must be nonnegative")
    call_depth = args.expected_boundaries + 1
    output_dir = args.output_dir.expanduser().resolve()
    write_metadata(
        output_dir,
        {
            "kind": "same-pause-repeated-dbt",
            "breakpoint": args.breakpoint,
            "call_depth": call_depth,
            "expected_rpc_boundaries": args.expected_boundaries,
            "expected_sessions": args.expected_sessions,
            "warmup_fresh_cycles": args.warmup,
            "same_pause_repetitions": args.repetitions,
            "pause_policy": (
                f"all {args.expected_sessions} stopped once before "
                f"{args.repetitions} consecutive DBTs; no continue between DBTs"
            ),
            "kubeconfig": str(args.kubeconfig.expanduser().resolve()),
        },
    )

    repeated: list[dict[str, int | float]] = []
    session = DdbSession(
        args.ddb,
        args.config,
        output_dir,
        api_port=args.api_port,
        expected_sessions=args.expected_sessions,
        command_timeout=args.timeout,
        console_level="debug",
        echo=args.echo_ddb,
    )
    try:
        session.start()
        attached = {int(item["sid"]) for item in session.api_json("/sessions")}
        if len(attached) != args.expected_sessions:
            raise RuntimeError(
                f"expected {args.expected_sessions} sessions, got {sorted(attached)}"
            )
        group_id = choose_group(session, args.group_id)
        session.send(
            f'-break-insert --group {group_id} "{args.breakpoint}"',
            sample=-1,
            phase="setup",
            timeout=args.timeout,
        )

        for cycle in range(args.warmup):
            thread, trigger, stream, _ = reach_cluster_pause(
                session,
                attached,
                args.trigger,
                output_dir / f"trigger_warmup_{cycle}.log",
                sample=cycle,
                phase="warmup",
                timeout=args.timeout,
            )
            try:
                session.send(
                    f"-bt-remote --thread {thread}",
                    sample=cycle,
                    phase="warmup",
                    timeout=args.timeout,
                )
                print(
                    f"[prepare {cycle + 1}/{args.warmup}] command path primed",
                    flush=True,
                )
            finally:
                session.send("-exec-continue", sample=cycle, phase="warmup")
                finish_trigger(trigger, stream)
            time.sleep(0.2)

        thread, trigger, stream, _ = reach_cluster_pause(
            session,
            attached,
            args.trigger,
            output_dir / "trigger_same-pause_0.log",
            sample=0,
            phase="same-pause",
            timeout=args.timeout,
        )
        try:
            verify_tracers(
                args.kubeconfig.expanduser().resolve(),
                output_dir,
                args.expected_sessions,
                namespace=args.namespace,
                selector=args.selector,
                require_stopped=True,
            )
            print(
                f"[same pause] all {args.expected_sessions} kernel tracers verified",
                flush=True,
            )
            for repeat in range(1, args.repetitions + 1):
                marker = session.mark()
                result, _ = session.send(
                    f"-bt-remote --thread {thread}",
                    sample=repeat,
                    phase="same-pause",
                    timeout=args.timeout,
                )
                end = session.mark()
                new_stops = session.stopped_session_ids(after=marker, before=end)
                row = {
                    "repeat": repeat,
                    "latency_ms": result.latency_ms,
                    "call_depth": result.service_frames,
                    "rpc_boundaries": result.rpc_boundaries,
                    "new_stop_events": len(new_stops),
                }
                repeated.append(row)
                if repeat > 1:
                    print(
                        f"[warm {repeat - 1:02d}/{args.repetitions - 1}] "
                        f"dbt={result.latency_ms:.3f} ms "
                        f"depth={result.service_frames} "
                        f"boundaries={result.rpc_boundaries} "
                        f"new-stops={len(new_stops)}",
                        flush=True,
                    )
        finally:
            session.send("-exec-continue", sample=0, phase="same-pause")
            finish_trigger(trigger, stream)
    finally:
        session.write_results()
        session.stop()

    with (output_dir / "same-pause-dbt.csv").open("w", newline="") as stream:
        writer = csv.DictWriter(
            stream,
            fieldnames=[
                "repeat",
                "latency_ms",
                "call_depth",
                "rpc_boundaries",
                "new_stop_events",
            ],
        )
        writer.writeheader()
        writer.writerows(repeated)

    bad_depth = [
        row
        for row in repeated
        if row["call_depth"] != call_depth
        or row["rpc_boundaries"] != args.expected_boundaries
    ]
    new_stops = [row for row in repeated if row["new_stop_events"] != 0]
    if bad_depth or new_stops:
        raise SystemExit(
            f"validation failed: depth mismatches={len(bad_depth)}, "
            f"commands with new stops={len(new_stops)}"
        )

    steady = [float(row["latency_ms"]) for row in repeated[1:]]
    print("\nWarm same-pause DBTs:")
    print(
        f"n={len(steady)} mean={statistics.mean(steady):.3f} ms "
        f"median={statistics.median(steady):.3f} ms "
        f"min={min(steady):.3f} ms max={max(steady):.3f} ms"
    )
    print(f"\nResults: {output_dir}")


if __name__ == "__main__":
    main()
