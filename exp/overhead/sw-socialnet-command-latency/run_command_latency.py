#!/usr/bin/env python3
"""Measure repeated DBT latency for every thread during one global pause."""

from __future__ import annotations

import argparse
import csv
import re
import shlex
import statistics
import subprocess
import time
from collections import Counter, defaultdict, deque
from pathlib import Path

from analyze_latency import percentile, print_table
from ddb_session import DdbSession, default_output_dir, write_metadata
from kernel_checks import verify_tracers


HERE = Path(__file__).resolve().parent
REPOSITORY_ROOT = HERE.parents[2]


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--ddb",
        type=Path,
        default=REPOSITORY_ROOT / "ddb" / "target" / "release" / "ddb",
    )
    parser.add_argument(
        "--config",
        type=Path,
        default=HERE / "ddb" / "serviceweaver_config.yaml",
    )
    parser.add_argument("--kubeconfig", type=Path, default=Path("~/.kube/config"))
    parser.add_argument("--namespace", default="default")
    parser.add_argument("--selector", required=True)
    parser.add_argument("--debugger-prefix", default="ssh-debugger-")
    parser.add_argument("--ddb-revision", default="unknown")
    parser.add_argument("--socialnet-revision", default="unknown")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--expected-sessions", type=int, default=14)
    parser.add_argument("--api-port", type=int, default=5000)
    parser.add_argument(
        "--repetitions",
        type=int,
        default=30,
        help="Measured concurrent all-thread batches.",
    )
    parser.add_argument(
        "--warmup-passes",
        type=int,
        default=1,
        help="Unmeasured DBT passes over all selected threads before sampling.",
    )
    parser.add_argument(
        "--thread-limit",
        type=int,
        default=0,
        help="Use N threads selected round-robin across sessions (0 means all).",
    )
    parser.add_argument(
        "--thread-id",
        action="append",
        type=int,
        help="Measure only this global thread ID; repeat to select several.",
    )
    parser.add_argument("--run-seconds", type=float, default=1.0)
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument(
        "--batch-timeout",
        type=float,
        default=300.0,
        help="Maximum time for one concurrently submitted command batch.",
    )
    parser.add_argument(
        "--console-level",
        choices=["debug"],
        default="debug",
        help="Must remain debug so timestamped received/output records are available.",
    )
    parser.add_argument(
        "--workload",
        help="Optional workload command run while the processes are briefly resumed.",
    )
    parser.add_argument("--skip-tracer-check", action="store_true")
    parser.add_argument("--echo-ddb", action="store_true")
    return parser


def split_mi_dict_list(text: str, field: str) -> list[str]:
    """Split a top-level MI list of dictionaries while respecting nested frames."""
    marker = f"{field}=["
    start = text.find(marker)
    if start < 0:
        return []
    objects: list[str] = []
    depth = 0
    object_start = -1
    in_string = False
    escaped = False
    for index in range(start + len(marker), len(text)):
        char = text[index]
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "{":
            if depth == 0:
                object_start = index
            depth += 1
        elif char == "}":
            depth -= 1
            if depth == 0 and object_start >= 0:
                objects.append(text[object_start : index + 1])
                object_start = -1
        elif char == "]" and depth == 0:
            break
    return objects


def thread_process_map(session: DdbSession) -> dict[int, tuple[int, str]]:
    mapping: dict[int, tuple[int, str]] = {}
    for item in session.lines:
        if "=thread-created," not in item.text:
            continue
        thread = re.search(r'(?:=thread-created,|,)id="(\d+)"', item.text)
        process = re.search(r'(?:=thread-created,|,)session-id="(\d+)"', item.text)
        alias = re.search(r'(?:=thread-created,|,)session-alias="([^"]*)"', item.text)
        if thread and process and alias:
            mapping[int(thread.group(1))] = (int(process.group(1)), alias.group(1))
    return mapping


def parse_threads(response_lines: list[object], session: DdbSession) -> list[dict]:
    """Extract DDB global threads and their owning process from thread-info."""
    text = "\n".join(getattr(line, "text") for line in response_lines)
    ownership = thread_process_map(session)
    threads: list[dict] = []
    for body in split_mi_dict_list(text, "threads"):
        match = re.search(r'(?:^\{|,)id="(\d+)"', body)
        if not match:
            continue
        thread_id = int(match.group(1))
        session_id, alias = ownership.get(thread_id, (-1, "UNKNOWN"))
        threads.append(
            {
                "thread_id": thread_id,
                "session_id": session_id,
                "session_alias": alias,
            }
        )
    unique = {row["thread_id"]: row for row in threads}
    return [unique[thread_id] for thread_id in sorted(unique)]


def interleave_threads_by_session(inventory: list[dict]) -> list[dict]:
    """Round-robin threads so adjacent DBTs target different GDB sessions."""
    grouped: dict[int, deque[dict]] = defaultdict(deque)
    for thread in inventory:
        grouped[int(thread["session_id"])].append(thread)
    ordered: list[dict] = []
    while grouped:
        for session_id in sorted(grouped):
            ordered.append(grouped[session_id].popleft())
            if not grouped[session_id]:
                del grouped[session_id]
    return ordered


def metric_row(values: list[float]) -> dict[str, str | int]:
    return {
        "count": len(values),
        "mean_ms": f"{statistics.fmean(values):.3f}",
        "median_ms": f"{statistics.median(values):.3f}",
        "stddev_ms": f"{statistics.pstdev(values):.3f}",
        "p95_ms": f"{percentile(values, 95):.3f}",
        "p99_ms": f"{percentile(values, 99):.3f}",
        "min_ms": f"{min(values):.3f}",
        "max_ms": f"{max(values):.3f}",
    }


def write_csv(path: Path, rows: list[dict], fields: list[str] | None = None) -> None:
    if fields is None:
        fields = list(rows[0]) if rows else []
    with path.open("w", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=fields)
        writer.writeheader()
        writer.writerows(rows)


def write_summaries(
    output_dir: Path,
    inventory: list[dict],
    rows: list[dict],
    *,
    process_count: int,
) -> list[dict]:
    measured = [row for row in rows if row["phase"] == "measure" and row["status"] == "ok"]
    by_thread: dict[int, list[dict]] = defaultdict(list)
    by_depth: dict[int, list[dict]] = defaultdict(list)
    for row in measured:
        by_thread[int(row["thread_id"])].append(row)
        by_depth[int(row["rpc_boundaries"])].append(row)

    process_by_thread = {
        int(row["thread_id"]): (int(row["session_id"]), str(row["session_alias"]))
        for row in inventory
    }
    thread_summary = []
    for thread_id, samples in sorted(by_thread.items()):
        depths = [int(row["rpc_boundaries"]) for row in samples]
        mode_depth = Counter(depths).most_common(1)[0][0]
        session_id, session_alias = process_by_thread.get(thread_id, (-1, "UNKNOWN"))
        thread_summary.append(
            {
                "thread_id": thread_id,
                "session_id": session_id,
                "session_alias": session_alias,
                "rpc_boundaries_mode": mode_depth,
                "rpc_boundaries_min": min(depths),
                "rpc_boundaries_max": max(depths),
                **metric_row([float(row["latency_ms"]) for row in samples]),
            }
        )
    write_csv(
        output_dir / "dbt-thread-summary.csv",
        thread_summary,
        [
            "thread_id",
            "session_id",
            "session_alias",
            "rpc_boundaries_mode",
            "rpc_boundaries_min",
            "rpc_boundaries_max",
            "count",
            "mean_ms",
            "median_ms",
            "stddev_ms",
            "p95_ms",
            "p99_ms",
            "min_ms",
            "max_ms",
        ],
    )

    depth_summary = []
    for depth, samples in sorted(by_depth.items()):
        depth_summary.append(
            {
                "rpc_boundaries": depth,
                "thread_count": len({int(row["thread_id"]) for row in samples}),
                **metric_row([float(row["latency_ms"]) for row in samples]),
            }
        )
    write_csv(
        output_dir / "dbt-depth-summary.csv",
        depth_summary,
        [
            "rpc_boundaries",
            "thread_count",
            "count",
            "mean_ms",
            "median_ms",
            "stddev_ms",
            "p95_ms",
            "p99_ms",
            "min_ms",
            "max_ms",
        ],
    )

    overall = []
    if measured:
        overall.append(
            {
                "command": "dbt",
                "process_count": process_count,
                "thread_count": len(by_thread),
                "repetitions": max(int(row["pass"]) for row in measured),
                **metric_row([float(row["latency_ms"]) for row in measured]),
            }
        )
    write_csv(
        output_dir / "summary.csv",
        overall,
        [
            "command",
            "process_count",
            "thread_count",
            "repetitions",
            "count",
            "mean_ms",
            "median_ms",
            "stddev_ms",
            "p95_ms",
            "p99_ms",
            "min_ms",
            "max_ms",
        ],
    )
    return overall


def main() -> None:
    args = build_parser().parse_args()
    if (
        args.repetitions < 1
        or args.warmup_passes < 0
        or args.thread_limit < 0
        or args.batch_timeout <= 0
    ):
        raise SystemExit(
            "repetitions and batch-timeout must be positive; "
            "warmup/thread-limit cannot be negative"
        )

    output_dir = (args.output_dir or default_output_dir("all-thread-dbt")).expanduser().resolve()
    output_dir.mkdir(parents=True, exist_ok=True)
    write_metadata(
        output_dir,
        {
            "kind": "all-thread-same-pause-dbt-latency",
            "paper_method": "DDB received-command timestamp to final token-matched MI response",
            "pause_policy": (
                "all attached sessions explicitly stopped and kernel tracers verified once; "
                "no continue between any warmup or measured DBT"
            ),
            "schedule": (
                "each batch submits one DBT per selected thread concurrently and waits; "
                "warmup and measured batches repeat inside one global pause"
            ),
            "expected_sessions": args.expected_sessions,
            "warmup_passes": args.warmup_passes,
            "repetitions": args.repetitions,
            "thread_limit": args.thread_limit,
            "requested_thread_ids": args.thread_id or [],
            "namespace": args.namespace,
            "selector": args.selector,
            "ddb_revision": args.ddb_revision,
            "socialnet_revision": args.socialnet_revision,
        },
    )

    workload = None
    workload_log = None
    inventory: list[dict] = []
    rows: list[dict] = []
    unexpected_stop_sessions: set[int] = set()
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
        attached = {int(item["sid"]) for item in session.api_json("/sessions")}
        if len(attached) != args.expected_sessions:
            raise RuntimeError(
                f"expected exactly {args.expected_sessions} sessions, got {sorted(attached)}"
            )

        if args.workload:
            workload_log = (output_dir / "workload.log").open("w")
            workload = subprocess.Popen(
                shlex.split(args.workload), stdout=workload_log, stderr=subprocess.STDOUT
            )

        session.send("-exec-continue", sample=-1, phase="setup", timeout=args.timeout)
        time.sleep(args.run_seconds)
        pause_marker = session.mark()
        pause, _ = session.send(
            "-exec-interrupt", sample=-1, phase="setup-cluster-pause", timeout=args.timeout
        )
        # A session that was already stopped may not emit a second *stopped
        # record for a broadcast interrupt.  Kernel state is the authoritative
        # cluster-wide proof; the event count is retained as a diagnostic.
        time.sleep(0.25)
        stopped_events = session.stopped_session_ids(after=pause_marker)
        if not args.skip_tracer_check:
            verify_tracers(
                args.kubeconfig.expanduser().resolve(),
                output_dir,
                args.expected_sessions,
                namespace=args.namespace,
                selector=args.selector,
                debugger_prefix=args.debugger_prefix,
                require_stopped=True,
            )

        _, thread_response = session.send(
            "-thread-info", sample=-1, phase="inventory", timeout=args.timeout
        )
        inventory = parse_threads(thread_response, session)
        if not inventory:
            raise RuntimeError("DDB returned no parseable global threads")
        unknown_owners = [row["thread_id"] for row in inventory if row["session_id"] < 0]
        if unknown_owners:
            raise RuntimeError(
                f"could not map {len(unknown_owners)} global threads to DDB sessions"
            )
        if args.thread_id:
            requested = set(args.thread_id)
            available = {row["thread_id"] for row in inventory}
            missing = requested - available
            if missing:
                raise RuntimeError(f"requested global threads do not exist: {sorted(missing)}")
            inventory = [row for row in inventory if row["thread_id"] in requested]
        inventory = interleave_threads_by_session(inventory)
        if args.thread_limit:
            inventory = inventory[: args.thread_limit]
        write_csv(
            output_dir / "thread-inventory.csv",
            inventory,
            ["thread_id", "session_id", "session_alias"],
        )

        stop_proof = (
            f"{args.expected_sessions} kernel-stopped processes"
            if not args.skip_tracer_check
            else "kernel stop check skipped"
        )
        print(
            f"[global pause] {stop_proof} "
            f"({len(stopped_events)} emitted new stop events), "
            f"{len(inventory)} threads selected, interrupt={pause.latency_ms:.3f} ms",
            flush=True,
        )
        def execute_batch(phase: str, pass_count: int) -> None:
            if pass_count == 0:
                return
            for pass_number in range(1, pass_count + 1):
                requests = [
                    (
                        f"-bt-remote --thread {thread['thread_id']}",
                        pass_number,
                        phase,
                    )
                    for thread in inventory
                ]
                marker = session.mark()
                started = time.monotonic()
                pending = session.submit_many(requests)
                if phase == "measure":
                    print(
                        f"[concurrent batch {pass_number}/{pass_count}] "
                        f"queued={len(pending)} (one DBT per thread)",
                        flush=True,
                    )
                completed = session.wait_many(
                    pending,
                    timeout=args.batch_timeout,
                    require_logged_response=True,
                )
                elapsed = time.monotonic() - started
                end = session.mark()
                batch_stops = session.stopped_session_ids(after=marker, before=end)
                unexpected_stop_sessions.update(batch_stops)

                pass_latencies: list[float] = []
                for ordinal, (thread, (result, response)) in enumerate(
                    zip(inventory, completed), start=1
                ):
                    response_text = "\n".join(item.text for item in response)
                    status = (
                        "error"
                        if re.search(rf"^{result.token}\^error\b", response_text, re.M)
                        else "ok"
                    )
                    row = {
                        "phase": phase,
                        "pass": pass_number,
                        "thread_ordinal": ordinal,
                        "thread_id": thread["thread_id"],
                        "session_id": thread["session_id"],
                        "session_alias": thread["session_alias"],
                        "latency_ms": f"{result.latency_ms:.6f}",
                        "submit_to_complete_ms": f"{result.submit_to_complete_ms:.6f}",
                        "rpc_boundaries": result.rpc_boundaries,
                        "service_frames": result.service_frames,
                        "batch_new_stop_events": len(batch_stops),
                        "status": status,
                        "token": result.token,
                    }
                    rows.append(row)
                    if status == "ok":
                        pass_latencies.append(result.latency_ms)

                if phase == "measure" and pass_latencies:
                    rate = len(completed) / elapsed if elapsed else 0.0
                    print(
                        f"[concurrent batch {pass_number}/{pass_count} complete] "
                        f"elapsed={elapsed:.3f} s throughput={rate:.1f} DBT/s "
                        f"n={len(pass_latencies)} "
                        f"mean={statistics.fmean(pass_latencies):.3f} ms "
                        f"median={statistics.median(pass_latencies):.3f} ms",
                        flush=True,
                    )

        execute_batch("warmup", args.warmup_passes)
        execute_batch("measure", args.repetitions)
    finally:
        try:
            if session.process is not None and session.process.poll() is None:
                session.send("-exec-continue", sample=-1, phase="cleanup", timeout=args.timeout)
        except Exception as exc:
            print(f"cleanup continue failed: {exc}", flush=True)
        session.write_results()
        session.stop()
        if workload is not None and workload.poll() is None:
            workload.terminate()
            try:
                workload.wait(timeout=5)
            except subprocess.TimeoutExpired:
                workload.kill()
        if workload_log is not None:
            workload_log.close()
        write_csv(
            output_dir / "dbt-thread-samples.csv",
            rows,
            [
                "phase",
                "pass",
                "thread_ordinal",
                "thread_id",
                "session_id",
                "session_alias",
                "latency_ms",
                "submit_to_complete_ms",
                "rpc_boundaries",
                "service_frames",
                "batch_new_stop_events",
                "status",
                "token",
            ],
        )

    overall = write_summaries(
        output_dir, inventory, rows, process_count=args.expected_sessions
    )
    print("\nAggregated measured DBT latency:")
    print_table(overall)
    print(f"\nResults: {output_dir}")

    failures = [row for row in rows if row["status"] != "ok"]
    expected_count = len(inventory) * args.repetitions
    measured_count = sum(row["phase"] == "measure" for row in rows)
    if failures or unexpected_stop_sessions or measured_count != expected_count:
        raise SystemExit(
            f"validation failed: errors={len(failures)}, "
            f"unexpected-stop-sessions={sorted(unexpected_stop_sessions)}, "
            f"measured={measured_count}, expected={expected_count}"
        )


if __name__ == "__main__":
    main()
