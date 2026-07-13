#!/usr/bin/env python3
"""Small, dependency-free controller for DDB's MI REPL.

The controller assigns every top-level command an external MI token.  DDB
preserves that token in the final response, which lets us measure from the
"received cmd" debug event to the corresponding response.  This is the same
pairing used by the paper's original analysis notebook, but the records are
written directly as CSV rather than recovered manually from a large log.
"""

from __future__ import annotations

import csv
import datetime as dt
import json
import os
import re
import subprocess
import threading
import time
import urllib.error
import urllib.request
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Pattern


ANSI_RE = re.compile(r"\x1b\[[0-9;]*[A-Za-z]")
ISO_TIMESTAMP_RE = re.compile(r"^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d\.\d+Z)")
STOPPED_SESSION_RE = re.compile(
    r'\*stopped,(?=[^\n]*session-id="(?P<session>\d+)")[^\n]*'
)


def clean_line(line: str) -> str:
    return ANSI_RE.sub("", line).rstrip("\n")


def source_timestamp_ns(line: str) -> int | None:
    """Return the timestamp emitted by tracing, if this is a tracing line."""
    match = ISO_TIMESTAMP_RE.match(line)
    if not match:
        return None
    parsed = dt.datetime.fromisoformat(match.group(1).replace("Z", "+00:00"))
    return int(parsed.timestamp()) * 1_000_000_000 + parsed.microsecond * 1_000


def command_name(command: str) -> str:
    prefix = command.strip().split(maxsplit=1)[0].lstrip("-")
    return {
        "bt-remote": "dbt",
        "exec-continue": "continue",
        "exec-interrupt": "exec-interrupt",
    }.get(prefix, prefix)


@dataclass
class ObservedLine:
    seq: int
    wall_ns: int
    mono_ns: int
    source_ns: int | None
    text: str


@dataclass
class CommandResult:
    sample: int
    phase: str
    command: str
    command_name: str
    token: int
    submitted_ns: int
    received_ns: int
    completed_ns: int
    received_timestamp_ns: int
    completed_timestamp_ns: int
    submit_to_complete_ms: float
    latency_ms: float
    response_count: int
    status: str
    rpc_boundaries: int = -1
    service_frames: int = -1


@dataclass(frozen=True)
class PendingCommand:
    command: str
    sample: int
    phase: str
    token: int
    cursor: int
    submitted_ns: int


RESULT_FIELDS = list(CommandResult.__dataclass_fields__)


class DdbSession:
    def __init__(
        self,
        ddb: Path,
        config: Path,
        output_dir: Path,
        api_port: int = 5000,
        expected_sessions: int = 14,
        startup_timeout: float = 180.0,
        command_timeout: float = 60.0,
        console_level: str = "debug",
        echo: bool = False,
    ) -> None:
        self.ddb = ddb.expanduser().resolve()
        self.config = config.expanduser().resolve()
        self.output_dir = output_dir.expanduser().resolve()
        self.api_port = api_port
        self.expected_sessions = expected_sessions
        self.startup_timeout = startup_timeout
        self.command_timeout = command_timeout
        self.console_level = console_level
        self.echo = echo

        self.process: subprocess.Popen[str] | None = None
        self.lines: list[ObservedLine] = []
        self.results: list[CommandResult] = []
        self._cv = threading.Condition()
        self._reader: threading.Thread | None = None
        self._raw = None
        self._next_token = 1_000_000

    @property
    def api_base(self) -> str:
        return f"http://127.0.0.1:{self.api_port}"

    def start(self) -> None:
        if not self.ddb.is_file():
            raise FileNotFoundError(f"DDB binary not found: {self.ddb}")
        if not self.config.is_file():
            raise FileNotFoundError(f"DDB config not found: {self.config}")

        self.output_dir.mkdir(parents=True, exist_ok=True)
        self._raw = (self.output_dir / "ddb-console.log").open("w", buffering=1)
        argv = [
            str(self.ddb),
            str(self.config),
            "--console-log",
            "--console-level",
            self.console_level,
            "--file-level",
            "debug",
        ]
        self.process = subprocess.Popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        self._reader = threading.Thread(target=self._read_output, daemon=True)
        self._reader.start()
        self._wait_until_ready()

    def _read_output(self) -> None:
        assert self.process is not None and self.process.stdout is not None
        for raw in self.process.stdout:
            now_wall = time.time_ns()
            now_mono = time.monotonic_ns()
            text = clean_line(raw)
            if self._raw is not None:
                self._raw.write(raw)
            if self.echo:
                print(text, flush=True)
            with self._cv:
                item = ObservedLine(
                    len(self.lines), now_wall, now_mono, source_timestamp_ns(text), text
                )
                self.lines.append(item)
                self._cv.notify_all()
        with self._cv:
            self._cv.notify_all()

    def api_json(self, path: str):
        with urllib.request.urlopen(self.api_base + path, timeout=2) as response:
            return json.load(response)

    def _wait_until_ready(self) -> None:
        deadline = time.monotonic() + self.startup_timeout
        last_count = 0
        while time.monotonic() < deadline:
            self._check_alive()
            try:
                if self.api_json("/status").get("status") == "up":
                    sessions = self.api_json("/sessions")
                    last_count = len(sessions)
                    if last_count >= self.expected_sessions:
                        return
            except (OSError, urllib.error.URLError, json.JSONDecodeError):
                pass
            time.sleep(0.5)
        raise TimeoutError(
            f"DDB reached {last_count}/{self.expected_sessions} sessions in "
            f"{self.startup_timeout:.0f}s; see {self.output_dir / 'ddb-console.log'}"
        )

    def _check_alive(self) -> None:
        if self.process is None:
            raise RuntimeError("DDB has not been started")
        rc = self.process.poll()
        if rc is not None:
            raise RuntimeError(
                f"DDB exited with status {rc}; see {self.output_dir / 'ddb-console.log'}"
            )

    def mark(self) -> int:
        with self._cv:
            return len(self.lines)

    def wait_for_pattern(
        self,
        pattern: str | Pattern[str],
        after: int = 0,
        timeout: float | None = None,
    ) -> tuple[re.Match[str], ObservedLine]:
        regex = re.compile(pattern) if isinstance(pattern, str) else pattern
        deadline = time.monotonic() + (timeout or self.command_timeout)
        cursor = after
        with self._cv:
            while True:
                for item in self.lines[cursor:]:
                    match = regex.search(item.text)
                    if match:
                        return match, item
                cursor = len(self.lines)
                self._check_alive()
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"timed out waiting for pattern: {regex.pattern}")
                self._cv.wait(min(remaining, 0.25))

    def wait_for_stopped_sessions(
        self,
        expected: set[int],
        *,
        after: int,
        initially_stopped: set[int] | None = None,
        timeout: float | None = None,
    ) -> tuple[set[int], ObservedLine | None]:
        """Wait until every expected DDB session has reported ``*stopped``.

        GDB's ``stopped-threads=all`` is scoped to one inferior, so a single
        breakpoint notification cannot establish that the other DDB sessions
        are stopped.  Callers can seed the breakpoint session through
        ``initially_stopped`` and then wait for one asynchronous stop record
        from every remaining session after a broadcast interrupt.
        """
        stopped = set(initially_stopped or ())
        unexpected = stopped - expected
        if unexpected:
            raise ValueError(f"initially stopped sessions are not attached: {unexpected}")

        deadline = time.monotonic() + (timeout or self.command_timeout)
        cursor = after
        final_stop: ObservedLine | None = None
        with self._cv:
            while True:
                for item in self.lines[cursor:]:
                    for match in STOPPED_SESSION_RE.finditer(item.text):
                        sid = int(match.group("session"))
                        if sid in expected and sid not in stopped:
                            stopped.add(sid)
                            final_stop = item
                cursor = len(self.lines)
                if expected <= stopped:
                    return stopped, final_stop

                self._check_alive()
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    missing = sorted(expected - stopped)
                    raise TimeoutError(
                        f"timed out waiting for all sessions to stop; missing={missing}"
                    )
                self._cv.wait(min(remaining, 0.25))

    def stopped_session_ids(self, *, after: int, before: int | None = None) -> set[int]:
        """Return session IDs with stop notifications in a captured line range."""
        with self._cv:
            lines = list(self.lines[after:before])
        return {
            int(match.group("session"))
            for item in lines
            for match in STOPPED_SESSION_RE.finditer(item.text)
        }

    def submit_many(
        self, requests: list[tuple[str, int, str]]
    ) -> list[PendingCommand]:
        """Write a batch of independently tokened commands before waiting.

        DDB's command handler has a native worker pool. Writing the complete
        batch in one flush lets that pool execute commands concurrently while
        the external MI tokens retain unambiguous response pairing.
        """
        if not requests:
            return []
        assert self.process is not None and self.process.stdin is not None
        for command, _, _ in requests:
            if not command.startswith("-"):
                raise ValueError(f"DDB command must start with '-': {command}")

        cursor = self.mark()
        submitted = time.monotonic_ns()
        pending: list[PendingCommand] = []
        lines: list[str] = []
        for command, sample, phase in requests:
            token = self._next_token
            self._next_token += 1
            pending.append(
                PendingCommand(
                    command=command,
                    sample=sample,
                    phase=phase,
                    token=token,
                    cursor=cursor,
                    submitted_ns=submitted,
                )
            )
            lines.append(f"{token}{command}\n")
        self.process.stdin.write("".join(lines))
        self.process.stdin.flush()
        return pending

    def wait_many(
        self,
        pending: list[PendingCommand],
        *,
        timeout: float | None = None,
        require_logged_response: bool = False,
    ) -> list[tuple[CommandResult, list[ObservedLine]]]:
        """Wait for a batch while scanning the shared output stream once."""
        if not pending:
            return []
        by_token = {item.token: item for item in pending}
        if len(by_token) != len(pending):
            raise ValueError("pending command tokens must be unique")

        direct = re.compile(r"^(?P<token>\d+)\^(?:done|running|connected|error|exit)\b")
        logged_output = re.compile(
            r"\boutput:\s*(?P<token>\d+)\^(?:done|running|connected|error|exit)\b"
        )
        received = re.compile(r"received cmd:\s*(?P<token>\d+)-[^\s]+\b")
        first_received: dict[int, ObservedLine] = {}
        responses: dict[int, list[ObservedLine]] = {
            token: [] for token in by_token
        }
        logged_responses: dict[int, list[ObservedLine]] = {
            token: [] for token in by_token
        }
        deadline = time.monotonic() + (timeout or self.command_timeout)
        scan = min(item.cursor for item in pending)

        def ready(token: int) -> bool:
            if not responses[token]:
                return False
            return not require_logged_response or (
                token in first_received and bool(logged_responses[token])
            )

        with self._cv:
            while True:
                for item in self.lines[scan:]:
                    match = received.search(item.text)
                    if match:
                        token = int(match.group("token"))
                        if token in by_token and token not in first_received:
                            first_received[token] = item

                    match = direct.search(item.text.strip())
                    if match:
                        token = int(match.group("token"))
                        if token in by_token:
                            responses[token].append(item)

                    match = logged_output.search(item.text)
                    if match:
                        token = int(match.group("token"))
                        if token in by_token:
                            logged_responses[token].append(item)
                scan = len(self.lines)

                if all(ready(token) for token in by_token):
                    break
                self._check_alive()
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    missing = [token for token in by_token if not ready(token)]
                    raise TimeoutError(
                        f"batch did not complete; missing {len(missing)}/{len(pending)} "
                        f"tokens (first: {missing[:10]}); see "
                        f"{self.output_dir / 'ddb-console.log'}"
                    )
                self._cv.wait(min(remaining, 0.25))

        completed_results: list[tuple[CommandResult, list[ObservedLine]]] = []
        for item in pending:
            command_responses = responses[item.token]
            command_logged = logged_responses[item.token]
            completed = command_responses[-1].mono_ns
            received_line = first_received.get(item.token)
            received_ns = received_line.mono_ns if received_line else item.submitted_ns
            received_source_ns = (
                received_line.source_ns
                if received_line is not None and received_line.source_ns is not None
                else 0
            )
            completed_source_ns = next(
                (
                    line.source_ns
                    for line in reversed(command_logged)
                    if line.source_ns is not None
                ),
                0,
            )
            latency_ms = (
                (completed_source_ns - received_source_ns) / 1_000_000
                if received_source_ns and completed_source_ns
                else (completed - received_ns) / 1_000_000
            )
            response_text = "\n".join(line.text for line in command_responses)
            boundaries = response_text.count('boundary_frame="1"')
            result = CommandResult(
                sample=item.sample,
                phase=item.phase,
                command=item.command,
                command_name=command_name(item.command),
                token=item.token,
                submitted_ns=item.submitted_ns,
                received_ns=received_ns,
                completed_ns=completed,
                received_timestamp_ns=received_source_ns,
                completed_timestamp_ns=completed_source_ns,
                submit_to_complete_ms=(completed - item.submitted_ns) / 1_000_000,
                latency_ms=latency_ms,
                response_count=len(command_responses),
                status="ok",
                rpc_boundaries=boundaries if command_name(item.command) == "dbt" else -1,
                service_frames=boundaries + 1 if command_name(item.command) == "dbt" else -1,
            )
            self.results.append(result)
            completed_results.append((result, command_responses))
        return completed_results

    def send(
        self,
        command: str,
        *,
        sample: int,
        phase: str,
        timeout: float | None = None,
        quiet_seconds: float = 0.15,
        require_logged_response: bool = False,
    ) -> tuple[CommandResult, list[ObservedLine]]:
        if not command.startswith("-"):
            raise ValueError(f"DDB command must start with '-': {command}")
        assert self.process is not None and self.process.stdin is not None

        token = self._next_token
        self._next_token += 1
        cursor = self.mark()
        submitted = time.monotonic_ns()
        self.process.stdin.write(f"{token}{command}\n")
        self.process.stdin.flush()

        direct = re.compile(rf"^{token}\^(?:done|running|connected|error|exit)\b")
        logged_output = re.compile(
            rf"\boutput:\s*{token}\^(?:done|running|connected|error|exit)\b"
        )
        received = re.compile(rf"received cmd:\s*{token}{re.escape(command.split(maxsplit=1)[0])}\b")
        deadline = time.monotonic() + (timeout or self.command_timeout)
        first_received: ObservedLine | None = None
        responses: list[ObservedLine] = []
        logged_responses: list[ObservedLine] = []
        scan = cursor
        last_response_at: float | None = None

        with self._cv:
            while True:
                for item in self.lines[scan:]:
                    if first_received is None and received.search(item.text):
                        first_received = item
                    if direct.search(item.text.strip()):
                        responses.append(item)
                        last_response_at = time.monotonic()
                    if logged_output.search(item.text):
                        logged_responses.append(item)
                scan = len(self.lines)

                if (
                    responses
                    and require_logged_response
                    and first_received is not None
                    and logged_responses
                ):
                    break
                if responses and not require_logged_response and last_response_at is not None:
                    quiet_left = quiet_seconds - (time.monotonic() - last_response_at)
                    if quiet_left <= 0:
                        break
                else:
                    quiet_left = 0.25

                self._check_alive()
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(
                        f"command did not complete: {token}{command}; see "
                        f"{self.output_dir / 'ddb-console.log'}"
                    )
                self._cv.wait(min(remaining, max(0.01, quiet_left)))

        completed = responses[-1].mono_ns
        received_ns = first_received.mono_ns if first_received else submitted
        received_source_ns = (
            first_received.source_ns if first_received and first_received.source_ns else 0
        )
        completed_source_ns = next(
            (
                item.source_ns
                for item in reversed(logged_responses)
                if item.source_ns is not None
            ),
            0,
        )
        paper_latency_ms = (
            (completed_source_ns - received_source_ns) / 1_000_000
            if received_source_ns and completed_source_ns
            else (completed - received_ns) / 1_000_000
        )
        response_text = "\n".join(line.text for line in responses)
        boundaries = response_text.count('boundary_frame="1"')
        result = CommandResult(
            sample=sample,
            phase=phase,
            command=command,
            command_name=command_name(command),
            token=token,
            submitted_ns=submitted,
            received_ns=received_ns,
            completed_ns=completed,
            received_timestamp_ns=received_source_ns,
            completed_timestamp_ns=completed_source_ns,
            submit_to_complete_ms=(completed - submitted) / 1_000_000,
            latency_ms=paper_latency_ms,
            response_count=len(responses),
            status="ok",
            rpc_boundaries=boundaries if command_name(command) == "dbt" else -1,
            service_frames=boundaries + 1 if command_name(command) == "dbt" else -1,
        )
        self.results.append(result)
        return result, responses

    def write_results(self, filename: str = "samples.csv") -> Path:
        path = self.output_dir / filename
        with path.open("w", newline="") as stream:
            writer = csv.DictWriter(stream, fieldnames=RESULT_FIELDS)
            writer.writeheader()
            for result in self.results:
                writer.writerow(asdict(result))
        return path

    def stop(self) -> None:
        process = self.process
        if process is None:
            return
        if process.poll() is None and process.stdin is not None:
            try:
                process.stdin.write("exit\n")
                process.stdin.flush()
                process.wait(timeout=15)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        if self._reader is not None:
            self._reader.join(timeout=2)
        if self._raw is not None:
            self._raw.close()
            self._raw = None
        self.process = None

    def __enter__(self) -> "DdbSession":
        self.start()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:
        self.stop()


def default_output_dir(kind: str) -> Path:
    stamp = time.strftime("%Y%m%d_%H%M%S", time.gmtime())
    return Path("results") / f"{kind}_{stamp}"


def write_metadata(path: Path, values: dict) -> None:
    values = dict(values)
    values.setdefault("created_utc", time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()))
    values.setdefault("hostname", os.uname().nodename)
    path.mkdir(parents=True, exist_ok=True)
    (path / "metadata.json").write_text(json.dumps(values, indent=2, sort_keys=True) + "\n")
