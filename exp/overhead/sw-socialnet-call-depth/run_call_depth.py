#!/usr/bin/env python3
"""Measure one DBT per breakpoint hit for a chosen call depth."""

from __future__ import annotations

import argparse
import csv
import os
import re
import subprocess
import time
from pathlib import Path

from analyze_latency import load, print_table, summarize
from ddb_session import DdbSession, default_output_dir, write_metadata


BREAK_HIT = re.compile(
    r'\*stopped,(?=[^\n]*reason="breakpoint-hit")'
    r'(?=[^\n]*thread-id="(?P<thread>\d+)")'
    r'(?=[^\n]*session-id="(?P<session>\d+)")[^\n]*'
)
LOCAL_DDB_CONFIG = Path(__file__).resolve().parent / "ddb" / "serviceweaver_config.yaml"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Set a breakpoint, trigger one request, and time DBT at the hit."
    )
    parser.add_argument("--breakpoint", required=True, help="GDB source location, e.g. storage.go:122")
    parser.add_argument(
        "--trigger",
        required=True,
        help="Command that sends one request (it may block until DDB resumes the cluster).",
    )
    parser.add_argument(
        "--expected-call-depth",
        "--expected-depth",
        dest="expected_call_depth",
        type=int,
        help="Expected call depth, including the originating process.",
    )
    parser.add_argument("--group-id", type=int, help="DDB group ID; auto-selected when only one exists")
    parser.add_argument("--samples", type=int, default=10)
    parser.add_argument("--warmup", type=int, default=3)
    configured_ddb = os.environ.get("DDB_BIN")
    parser.add_argument(
        "--ddb",
        type=Path,
        default=Path(configured_ddb) if configured_ddb else None,
        required=configured_ddb is None,
        help="Rust DDB binary (or set DDB_BIN)",
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=LOCAL_DDB_CONFIG,
    )
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--expected-sessions", type=int, default=14)
    parser.add_argument("--api-port", type=int, default=5000)
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--console-level", choices=["debug", "info"], default="debug")
    parser.add_argument("--echo-ddb", action="store_true")
    return parser


def choose_group(session: DdbSession, requested: int | None) -> int:
    groups = session.api_json("/groups")
    if requested is not None:
        if requested not in {group["id"] for group in groups}:
            raise RuntimeError(f"group {requested} is not present; groups={groups}")
        return requested
    if len(groups) != 1:
        raise RuntimeError(f"found {len(groups)} groups; select one with --group-id: {groups}")
    return int(groups[0]["id"])


def main() -> None:
    args = build_parser().parse_args()
    if args.expected_call_depth is not None and args.expected_call_depth < 1:
        raise SystemExit("--expected-call-depth must be positive")
    expected_boundaries = (
        args.expected_call_depth - 1
        if args.expected_call_depth is not None
        else None
    )
    output_dir = (args.output_dir or default_output_dir("call-depth")).expanduser().resolve()
    write_metadata(
        output_dir,
        {
            "kind": "call-depth",
            "paper_method": (
                "breakpoint hit, broadcast interrupt, verify every attached session stopped, "
                "then time one -bt-remote command"
            ),
            "breakpoint": args.breakpoint,
            "trigger": args.trigger,
            "expected_call_depth": args.expected_call_depth,
            "expected_rpc_boundaries": expected_boundaries,
            "expected_sessions": args.expected_sessions,
            "pause_policy": "all attached DDB sessions stopped before DBT submission",
            "samples": args.samples,
            "warmup": args.warmup,
        },
    )

    depth_rows: list[dict[str, int | float | str]] = []
    depth_mismatches = 0
    session = DdbSession(
        args.ddb,
        args.config,
        output_dir,
        api_port=args.api_port,
        expected_sessions=args.expected_sessions,
        command_timeout=args.timeout,
        console_level=args.console_level,
        echo=args.echo_ddb,
    )
    try:
        session.start()
        attached_session_ids = {
            int(item["sid"]) for item in session.api_json("/sessions")
        }
        if len(attached_session_ids) != args.expected_sessions:
            raise RuntimeError(
                f"expected exactly {args.expected_sessions} attached sessions, "
                f"found {len(attached_session_ids)}: {sorted(attached_session_ids)}"
            )
        group_id = choose_group(session, args.group_id)
        bp_result, _ = session.send(
            f'-break-insert --group {group_id} "{args.breakpoint}"',
            sample=-1,
            phase="setup",
        )
        print(f"breakpoint installed for group {group_id}: {bp_result.latency_ms:.3f} ms")

        total = args.warmup + args.samples
        for cycle in range(total):
            phase = "warmup" if cycle < args.warmup else "measure"
            sample = cycle - args.warmup if phase == "measure" else cycle
            marker = session.mark()
            session.send("-exec-continue", sample=sample, phase=phase)

            trigger_log = (output_dir / f"trigger_{phase}_{sample}.log").open("w")
            trigger = subprocess.Popen(
                args.trigger,
                shell=True,
                stdout=trigger_log,
                stderr=subprocess.STDOUT,
            )
            try:
                hit, _ = session.wait_for_pattern(BREAK_HIT, after=marker, timeout=args.timeout)
                thread = int(hit.group("thread"))
                breakpoint_session = int(hit.group("session"))
                if breakpoint_session not in attached_session_ids:
                    raise RuntimeError(
                        f"breakpoint came from unknown session {breakpoint_session}; "
                        f"attached={sorted(attached_session_ids)}"
                    )

                # A GDB breakpoint stops every thread in its own inferior, not
                # every DDB session. Broadcast the pause and wait for the actual
                # asynchronous stop event from every other attached session.
                pause_marker = session.mark()
                pause, _ = session.send(
                    "-exec-interrupt",
                    sample=sample,
                    phase=f"{phase}-cluster-pause",
                    timeout=args.timeout,
                )
                stopped_sessions, final_stop = session.wait_for_stopped_sessions(
                    attached_session_ids,
                    after=pause_marker,
                    initially_stopped={breakpoint_session},
                    timeout=args.timeout,
                )
                pause_to_all_stopped_ms = (
                    max(0.0, (final_stop.mono_ns - pause.submitted_ns) / 1_000_000)
                    if final_stop is not None
                    else 0.0
                )

                # This marker is the timing precondition: all 14 sessions have
                # stopped before the external DBT command can be submitted.
                dbt_marker = session.mark()
                dbt, _ = session.send(
                    f"-bt-remote --thread {thread}",
                    sample=sample,
                    phase=phase,
                    timeout=args.timeout,
                )
                dbt_end = session.mark()
                dbt_stop_sessions = session.stopped_session_ids(
                    after=dbt_marker, before=dbt_end
                )
                if dbt_stop_sessions:
                    raise RuntimeError(
                        "DBT caused new stop events even though the cluster-wide pause "
                        f"was verified first: {sorted(dbt_stop_sessions)}"
                    )
                matches_expected = (
                    args.expected_call_depth is None
                    or dbt.service_frames == args.expected_call_depth
                )
                if not matches_expected:
                    depth_mismatches += 1
                depth_rows.append(
                    {
                        "sample": sample,
                        "phase": phase,
                        "thread": thread,
                        "breakpoint_session": breakpoint_session,
                        "stopped_sessions": len(stopped_sessions),
                        "pause_command_latency_ms": pause.latency_ms,
                        "pause_to_all_stopped_ms": pause_to_all_stopped_ms,
                        "latency_ms": dbt.latency_ms,
                        "call_depth": dbt.service_frames,
                        "rpc_boundaries": dbt.rpc_boundaries,
                        "service_frames": dbt.service_frames,
                        "dbt_stop_events": len(dbt_stop_sessions),
                        "matches_expected": str(matches_expected).lower(),
                    }
                )
                print(
                    f"[{phase} {cycle + 1}/{total}] all {len(stopped_sessions)} sessions "
                    f"stopped in {pause_to_all_stopped_ms:.3f} ms; "
                    f"dbt={dbt.latency_ms:.3f} ms, depth={dbt.service_frames}, "
                    f"boundaries={dbt.rpc_boundaries}, "
                    f"thread={thread}"
                )
                if not matches_expected:
                    print(
                        f"  WARNING: expected call depth {args.expected_call_depth} "
                        f"({expected_boundaries} RPC boundaries); "
                        "check the breakpoint/request pair and ServiceWeaver extension"
                    )
            finally:
                # Restore every parent context touched by DBT and release the request.
                session.send("-exec-continue", sample=sample, phase=phase)
                try:
                    trigger.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    trigger.terminate()
                    trigger.wait(timeout=5)
                trigger_log.close()
            time.sleep(0.2)
    finally:
        session.write_results()
        session.stop()

    depth_csv = output_dir / "depth-samples.csv"
    with depth_csv.open("w", newline="") as stream:
        fields = [
            "sample",
            "phase",
            "thread",
            "breakpoint_session",
            "stopped_sessions",
            "pause_command_latency_ms",
            "pause_to_all_stopped_ms",
            "latency_ms",
            "call_depth",
            "rpc_boundaries",
            "service_frames",
            "dbt_stop_events",
            "matches_expected",
        ]
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(depth_rows)

    rows = summarize(load([output_dir / "samples.csv"], include_warmup=False))
    rows = [row for row in rows if row["command"] == "dbt"]
    print("\nMeasured DBT latency:")
    print_table(rows)
    print(f"\nResults: {output_dir}")
    if depth_mismatches:
        raise SystemExit(
            f"{depth_mismatches} sample(s) did not match --expected-call-depth; "
            f"inspect {depth_csv}"
        )


if __name__ == "__main__":
    main()
