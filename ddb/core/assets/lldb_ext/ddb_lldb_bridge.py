"""DDB's structured LLDB bridge.

This module runs inside LLDB's embedded Python interpreter.  Rust sends one
JSON request per line; the bridge emits prefixed JSON records so LLDB prompts,
diagnostics, and inferior output can coexist on stdout without becoming part
of the machine protocol.
"""

from __future__ import print_function

import fnmatch
import json
import os
import queue
import re
import shlex
import signal
import sys
import threading
import time
import traceback

import lldb  # type: ignore


RECORD_PREFIX = "@DDB@"
GROUP_ID = "i1"
INVALID_ADDRESS = getattr(lldb, "LLDB_INVALID_ADDRESS", (1 << 64) - 1)
STACK_PREWARM_FRAME_LIMIT = 64
FRAMEWORK_PRESETS = {
    "ddb-runtime": {
        "functions": ["DDB::*"],
        "files": [],
    },
    "serviceweaver": {
        "functions": ["*runHandler", "github.com/ServiceWeaver/*"],
        "files": [],
    },
    "go-runtime": {
        "functions": ["runtime.*", "runtime/internal/*"],
        "files": [],
    },
    "cpp-stdlib": {
        "functions": ["std::*", "__gnu_cxx::*", "__cxx*"],
        "files": ["/usr/include/*", "/usr/lib/*"],
    },
    "networking": {
        "functions": ["net.*", "net::*"],
        "files": [],
    },
    "grpc": {
        "functions": ["grpc::*", "grpc_*", "google::protobuf::*"],
        "files": [
            "*/grpc/*",
            "*/grpcpp/*",
            "*/google/protobuf/*",
            "/usr/include/grpc*/*",
            "/usr/local/include/grpc*/*",
        ],
    },
    "protobuf-gen": {
        "functions": [],
        "files": ["*.grpc.pb.cc"],
    },
}


def _text(value):
    if value is None:
        return ""
    return str(value)


class Emitter(object):
    def __init__(self):
        self._lock = threading.Lock()

    def emit(self, record_type, message, payload=None, token=None, stream=None):
        record = {"type": record_type, "message": message}
        if payload is not None:
            record["payload"] = payload
        if token is not None:
            record["token"] = token
        if stream is not None:
            record["stream"] = stream
        encoded = json.dumps(record, separators=(",", ":"), sort_keys=True)
        with self._lock:
            sys.stdout.write(RECORD_PREFIX + encoded + "\n")
            sys.stdout.flush()

    def result(self, token, message="done", payload=None):
        self.emit("result", message, payload=payload, token=token)

    def event(self, message, payload, token=None):
        self.emit("event", message, payload=payload, token=token)

    def stream(self, message, stream="console"):
        self.emit("stream", message, stream=stream)


class FrameFilters(object):
    def __init__(self):
        self.enabled = False
        self.mode = "blacklist"
        self.files = []
        self.functions = []
        self.active_presets = set()

    def configure(self, arguments):
        if not arguments or arguments[0] == "--status":
            return self._response("success", "Filter status")
        action = arguments[0]
        if action == "--enable":
            self.enabled = True
            return self._response("success", "Filter enabled")
        if action == "--disable":
            self.enabled = False
            return self._response("success", "Filter disabled")
        if action == "--add-file":
            pattern = self._required_argument(arguments, action)
            self.files.append((pattern, self._match_type(arguments)))
            return self._response("success", "Added file rule: {}".format(pattern))
        if action == "--add-function":
            pattern = self._required_argument(arguments, action)
            self.functions.append((pattern, self._match_type(arguments)))
            return self._response("success", "Added function rule: {}".format(pattern))
        if action == "--remove-file":
            pattern = self._required_argument(arguments, action)
            self.files = [rule for rule in self.files if rule[0] != pattern]
            return self._response("success", "Removed file rule: {}".format(pattern))
        if action == "--remove-function":
            pattern = self._required_argument(arguments, action)
            self.functions = [rule for rule in self.functions if rule[0] != pattern]
            return self._response("success", "Removed function rule: {}".format(pattern))
        if action == "--preset-enable":
            name = self._required_argument(arguments, action)
            self._enable_preset(name)
            return self._response("success", "Enabled preset: {}".format(name))
        if action == "--preset-disable":
            name = self._required_argument(arguments, action)
            self._disable_preset(name)
            return self._response("success", "Disabled preset: {}".format(name))
        if action == "--list-presets":
            response = self._response("success", "Available presets")
            response["presets"] = sorted(FRAMEWORK_PRESETS)
            return response
        if action == "--clear":
            self.files = []
            self.functions = []
            self.active_presets = set()
            return self._response("success", "Cleared all rules")
        if action == "--mode":
            mode = self._required_argument(arguments, action)
            if mode not in ("blacklist", "whitelist"):
                raise ValueError("filter mode must be blacklist or whitelist")
            self.mode = mode
            return self._response("success", "Mode set to: {}".format(mode))
        raise ValueError("unknown frame-filter action: {}".format(action))

    @staticmethod
    def _required_argument(arguments, option):
        if len(arguments) < 2:
            raise ValueError("{} requires an argument".format(option))
        return arguments[1]

    @staticmethod
    def _match_type(arguments):
        match_type = "glob"
        if "--match-type" in arguments:
            type_index = arguments.index("--match-type")
            if type_index + 1 < len(arguments):
                match_type = arguments[type_index + 1]
        if match_type not in ("exact", "glob", "regex"):
            raise ValueError("unknown frame-filter match type: {}".format(match_type))
        return match_type

    def _enable_preset(self, name):
        preset = FRAMEWORK_PRESETS.get(name)
        if preset is None:
            raise ValueError("unknown frame-filter preset: {}".format(name))
        if name in self.active_presets:
            return
        self.functions.extend((pattern, "glob") for pattern in preset["functions"])
        self.files.extend((pattern, "glob") for pattern in preset["files"])
        self.active_presets.add(name)

    def _disable_preset(self, name):
        preset = FRAMEWORK_PRESETS.get(name)
        if preset is None or name not in self.active_presets:
            raise ValueError("frame-filter preset is not active: {}".format(name))
        function_patterns = set(preset["functions"])
        file_patterns = set(preset["files"])
        self.functions = [
            rule
            for rule in self.functions
            if not (rule[1] == "glob" and rule[0] in function_patterns)
        ]
        self.files = [
            rule
            for rule in self.files
            if not (rule[1] == "glob" and rule[0] in file_patterns)
        ]
        self.active_presets.remove(name)

    def _response(self, message, info):
        return {
            "message": message,
            "info": info,
            "config": {
                "enabled": self.enabled,
                "mode": self.mode,
                "function_rules": [
                    {"pattern": pattern, "match_type": match_type}
                    for pattern, match_type in self.functions
                ],
                "file_rules": [
                    {"pattern": pattern, "match_type": match_type}
                    for pattern, match_type in self.files
                ],
                "active_presets": sorted(self.active_presets),
            },
        }

    def include(self, frame):
        if not self.enabled:
            return True
        function_name = frame.GetFunctionName() or ""
        file_name = _file_path(frame.GetLineEntry().GetFileSpec())
        matched = (
            self._matches(function_name, self.functions)
            or self._matches(file_name, self.files)
        )
        return not matched if self.mode == "blacklist" else matched

    @staticmethod
    def _matches(value, patterns):
        for pattern, match_type in patterns:
            if match_type == "regex" and re.search(pattern, value):
                return True
            if match_type == "glob" and fnmatch.fnmatch(value, pattern):
                return True
            if match_type == "exact" and value == pattern:
                return True
        return False


def _file_path(file_spec):
    if not file_spec or not file_spec.IsValid():
        return ""
    directory = file_spec.GetDirectory() or ""
    filename = file_spec.GetFilename() or ""
    return os.path.join(directory, filename) if directory else filename


def _normalized_source_path(path):
    # Rust emits one DWARF compile unit per codegen unit using
    # `<source>/@/<unit-hash>` file specs. The suffix identifies the codegen
    # unit, not a real source path.
    return path.split("/@/", 1)[0]


def _error_text(error):
    if error and error.Fail():
        return error.GetCString() or str(error)
    return ""


def _value_text(value):
    if not value or not value.IsValid():
        return ""
    return value.GetValue() or value.GetSummary() or value.GetObjectDescription() or ""


def _value_u64(value, default=0):
    if not value or not value.IsValid():
        return default
    result = value.GetValueAsUnsigned(INVALID_ADDRESS)
    if result != INVALID_ADDRESS:
        return int(result)
    rendered_parts = _value_text(value).split(None, 1)
    if not rendered_parts:
        return default
    rendered = rendered_parts[0]
    try:
        return int(rendered, 0)
    except (TypeError, ValueError):
        return default


def _child(value, name):
    if not value or not value.IsValid():
        return None
    child = value.GetChildMemberWithName(name)
    return child if child and child.IsValid() else None


def _frame_payload(frame, target, level=None, include_arguments=True):
    line_entry = frame.GetLineEntry()
    file_path = _file_path(line_entry.GetFileSpec())
    address = frame.GetPCAddress().GetLoadAddress(target)
    arguments = []
    if include_arguments:
        variables = frame.GetVariables(True, False, False, True)
        for index in range(variables.GetSize()):
            value = variables.GetValueAtIndex(index)
            arguments.append(
                {
                    "name": _text(value.GetName()),
                    "value": _value_text(value),
                }
            )
    payload = {
        "level": _text(frame.GetFrameID() if level is None else level),
        "addr": "0x{:x}".format(address if address != INVALID_ADDRESS else 0),
        "func": _text(frame.GetFunctionName() or frame.GetDisplayFunctionName() or "??"),
        "args": arguments,
        "arch": _text(target.GetTriple()),
    }
    if file_path:
        payload["file"] = file_path
        payload["fullname"] = file_path
    if line_entry and line_entry.IsValid():
        payload["line"] = _text(line_entry.GetLine())
    return payload


class ProcessMonitor(object):
    def __init__(self, debugger, emitter):
        self.debugger = debugger
        self.emitter = emitter
        self.listener = lldb.SBListener("ddb.process-monitor")
        self.running = True
        self.thread = None
        self.stack_prewarm_enabled = False
        self._group_started = False
        self._group_added = False
        self._threads = set()
        self._process_uid = None
        self._last_state = None
        self._last_stop_id = None
        self._stack_prewarmed = False
        self.pause_started_ns = None
        self._lock = threading.Lock()

    def start(self):
        pass

    def stop(self):
        self.running = False

    def reset(self):
        with self._lock:
            self._group_started = False
            self._group_added = False
            self._threads = set()
            self._process_uid = None
            self._last_state = None
            self._last_stop_id = None
            self._stack_prewarmed = False
            self.pause_started_ns = None

    def snapshot(self, process=None):
        process = process or self.debugger.GetSelectedTarget().GetProcess()
        if process and process.IsValid():
            self._publish_state(process, process.GetState())

    def poll(self):
        while self.running:
            event = lldb.SBEvent()
            if not self.listener.GetNextEvent(event):
                break
            if not lldb.SBProcess.EventIsProcessEvent(event):
                continue
            process = lldb.SBProcess.GetProcessFromEvent(event)
            if not process or not process.IsValid():
                continue
            state = lldb.SBProcess.GetStateFromEvent(event)
            try:
                self._publish_state(process, state)
            except Exception:
                self.emitter.stream(traceback.format_exc(), "log")

    def _ensure_process(self, process):
        uid = process.GetUniqueID()
        if self._process_uid != uid:
            self.listener = lldb.SBListener("ddb.process-monitor")
            self.listener.StartListeningForEvents(
                process.GetBroadcaster(),
                lldb.SBProcess.eBroadcastBitStateChanged,
            )
            self._group_started = False
            self._group_added = False
            self._threads = set()
            self._process_uid = uid
            self._last_state = None
            self._last_stop_id = None
            self._stack_prewarmed = False
        if not self._group_added:
            self.emitter.event("thread-group-added", {"id": GROUP_ID})
            self._group_added = True
        pid = process.GetProcessID()
        if pid and not self._group_started:
            self.emitter.event(
                "thread-group-started",
                {"id": GROUP_ID, "pid": _text(pid)},
            )
            self._group_started = True

    def _sync_threads(self, process):
        current = set()
        for index in range(process.GetNumThreads()):
            thread = process.GetThreadAtIndex(index)
            thread_id = int(thread.GetIndexID())
            current.add(thread_id)
            if thread_id not in self._threads:
                self.emitter.event(
                    "thread-created",
                    {"id": _text(thread_id), "group-id": GROUP_ID},
                )
        for thread_id in sorted(self._threads - current):
            self.emitter.event(
                "thread-exited",
                {"id": _text(thread_id), "group-id": GROUP_ID},
            )
        self._threads = current

    def _publish_state(self, process, state):
        with self._lock:
            self._ensure_process(process)
            self._sync_threads(process)
            stop_id = process.GetStopID() if state == lldb.eStateStopped else None
            if state == self._last_state and (
                state != lldb.eStateStopped or stop_id == self._last_stop_id
            ):
                return
            self._last_state = state
            self._last_stop_id = stop_id
            if state in (lldb.eStateRunning, lldb.eStateStepping):
                self.emitter.event("running", {"thread-id": "all"})
            elif state in (lldb.eStateStopped, lldb.eStateCrashed, lldb.eStateSuspended):
                self.pause_started_ns = time.monotonic_ns()
                thread = process.GetSelectedThread()
                if not thread or not thread.IsValid():
                    thread = process.GetThreadAtIndex(0)
                self.emitter.event("stopped", self._stop_payload(process, thread, state))
                self._prewarm_stack_once(thread)
            elif state in (lldb.eStateExited, lldb.eStateDetached):
                reason = "exited-normally" if process.GetExitStatus() == 0 else "exited"
                self.emitter.event(
                    "stopped",
                    {"reason": reason, "stopped-threads": "all"},
                )
                for thread_id in sorted(self._threads):
                    self.emitter.event(
                        "thread-exited",
                        {"id": _text(thread_id), "group-id": GROUP_ID},
                    )
                self._threads = set()
                self.emitter.event("thread-group-exited", {"id": GROUP_ID})

    def _prewarm_stack_once(self, thread):
        if (
            not self.stack_prewarm_enabled
            or self._stack_prewarmed
            or not thread
            or not thread.IsValid()
        ):
            return
        self._stack_prewarmed = True
        try:
            # LLDB lazily constructs its unwind and symbol caches on the first
            # stack query. Emit the stop first, then use the bridge's idle window
            # to pay that one-time cost instead of charging the first interactive
            # backtrace. Subsequent stops reuse LLDB's caches. Bound speculative
            # symbolization so an unusually deep or damaged stack cannot stall
            # session readiness indefinitely.
            target = thread.GetProcess().GetTarget()
            for index in range(STACK_PREWARM_FRAME_LIMIT):
                frame = thread.GetFrameAtIndex(index)
                if not frame or not frame.IsValid():
                    break
                _frame_payload(frame, target, index, include_arguments=False)
        except Exception:
            # Prewarming is an optimization and must never make a valid stop
            # unusable. The real stack command will report any persistent error.
            self.emitter.stream(
                "LLDB stack prewarm failed:\n{}".format(traceback.format_exc()), "log"
            )

    @staticmethod
    def _stop_payload(process, thread, state):
        payload = {"reason": "stopped", "stopped-threads": "all"}
        if thread and thread.IsValid():
            payload["thread-id"] = _text(thread.GetIndexID())
            reason = thread.GetStopReason()
            if reason == lldb.eStopReasonBreakpoint:
                payload["reason"] = "breakpoint-hit"
                if thread.GetStopReasonDataCount() > 0:
                    payload["bkptno"] = _text(thread.GetStopReasonDataAtIndex(0))
            elif reason in (
                lldb.eStopReasonPlanComplete,
                lldb.eStopReasonTrace,
            ):
                payload["reason"] = "end-stepping-range"
            elif reason == lldb.eStopReasonSignal:
                payload["reason"] = "signal-received"
                if thread.GetStopReasonDataCount() > 0:
                    number = int(thread.GetStopReasonDataAtIndex(0))
                    unix_signals = process.GetUnixSignals()
                    payload["signal-name"] = _text(
                        unix_signals.GetSignalAsCString(number)
                    )
            target = process.GetTarget()
            frame = thread.GetFrameAtIndex(0)
            if frame and frame.IsValid():
                payload["frame"] = _frame_payload(frame, target, 0)
        if state == lldb.eStateCrashed:
            payload["reason"] = "signal-received"
        return payload


class Bridge(object):
    def __init__(self, debugger):
        self.debugger = debugger
        self.emitter = Emitter()
        self.monitor = ProcessMonitor(debugger, self.emitter)
        self.filters = FrameFilters()
        self.launch_arguments = []
        self.running = True
        self.requests = queue.Queue()
        self.reader = None
        self.accumulated_pause_seconds = 0.0
        self.debugger.SetAsync(True)
        self.monitor.start()

    def close(self):
        self.monitor.stop()

    def _read_requests(self):
        while True:
            line = sys.stdin.readline()
            if not line:
                self.requests.put(None)
                return
            self.requests.put(line)
            try:
                request = json.loads(line)
            except (TypeError, ValueError):
                continue
            if request.get("command") == "-gdb-exit":
                return

    def serve(self):
        self.emitter.stream("DDB LLDB bridge ready", "log")
        self.reader = threading.Thread(
            target=self._read_requests,
            name="ddb-lldb-stdin",
        )
        self.reader.daemon = True
        self.reader.start()
        while self.running:
            self.monitor.poll()
            try:
                line = self.requests.get(timeout=0.05)
            except queue.Empty:
                continue
            if line is None:
                break
            line = line.strip()
            if not line:
                continue
            token = None
            try:
                request = json.loads(line)
                token = int(request["id"])
                command = request["command"]
                thread_id = request.get("thread_id")
                message, payload = self.execute(command, thread_id)
                self.monitor.poll()
                self.emitter.result(token, message, payload)
                if not self.running:
                    # DDB owns this dedicated LLDB process. Native shutdown and
                    # result flushing are complete; SystemExit would only unwind
                    # this nested script command and leave LLDB running.
                    os._exit(0)
            except Exception as error:
                self.emitter.result(
                    token,
                    "error",
                    {
                        "msg": _text(error),
                        "traceback": traceback.format_exc(),
                    },
                )

    def execute(self, command, thread_id=None):
        arguments = shlex.split(command)
        if not arguments:
            raise ValueError("empty debugger command")
        operation = arguments[0]
        self._select_thread(thread_id)

        handlers = {
            "-file-exec-and-symbols": self._create_target,
            "-exec-arguments": self._set_arguments,
            "-target-attach": self._attach,
            "-exec-run": self._run,
            "-exec-continue": self._continue,
            "-record-time-and-continue": self._record_time_and_continue,
            "-exec-interrupt": self._interrupt,
            "-exec-interrupt-if-running": self._interrupt_if_running,
            "-exec-next": self._next,
            "-record-time-and-next": self._record_time_and_next,
            "-exec-step": self._step,
            "-record-time-and-step": self._record_time_and_step,
            "-exec-finish": self._finish,
            "-record-time-and-finish": self._record_time_and_finish,
            "-exec-jump": self._jump,
            "-thread-select": self._thread_select,
            "-thread-info": self._thread_info,
            "-list-thread-groups": self._thread_groups,
            "-stack-list-frames": self._stack_frames,
            "-data-list-register-names": self._register_names,
            "-data-list-register-values": self._register_values,
            "-data-evaluate-expression": self._evaluate_expression,
            "-file-list-exec-source-files": self._source_files,
            "-file-list-lines": self._source_lines,
            "-break-insert": self._break_insert,
            "-break-delete": self._break_delete,
            "-switch-context-custom": self._switch_context,
            "-get-remote-bt": self._get_remote_backtrace,
            "-serviceweaver-bt-remote": self._get_remote_backtrace,
            "-list-signals": self._list_signals,
            "-interpreter-exec": self._interpreter_exec,
            "-ddb-filter-config": self._filter_config,
            "-ddb-set-stack-prewarm": self._set_stack_prewarm,
            "-enable-frame-filters": self._no_op,
            "-gdb-set": self._no_op,
            "-target-detach": self._detach,
            "-target-kill": self._kill,
            "-ddb-shutdown": self._shutdown,
            "-gdb-exit": self._exit_bridge,
        }
        handler = handlers.get(operation)
        if handler is None:
            raise ValueError("unsupported LLDB backend command: {}".format(operation))
        return handler(arguments[1:])

    def _target(self):
        target = self.debugger.GetSelectedTarget()
        if not target or not target.IsValid():
            raise RuntimeError("no LLDB target is selected")
        return target

    def _process(self):
        process = self._target().GetProcess()
        if not process or not process.IsValid():
            raise RuntimeError("no LLDB process is active")
        return process

    def _thread(self):
        process = self._process()
        thread = process.GetSelectedThread()
        if not thread or not thread.IsValid():
            thread = process.GetThreadAtIndex(0)
        if not thread or not thread.IsValid():
            raise RuntimeError("no LLDB thread is selected")
        return thread

    def _select_thread(self, thread_id):
        if thread_id is None:
            return
        process = self._process()
        thread = process.GetThreadByIndexID(int(thread_id))
        if not thread or not thread.IsValid():
            raise ValueError("unknown LLDB thread {}".format(thread_id))
        process.SetSelectedThread(thread)

    def _create_target(self, arguments):
        if len(arguments) != 1:
            raise ValueError("-file-exec-and-symbols requires one executable")
        target = self.debugger.CreateTarget(arguments[0])
        if not target or not target.IsValid():
            raise RuntimeError("failed to create LLDB target for {}".format(arguments[0]))
        self.debugger.SetSelectedTarget(target)
        self.monitor.reset()
        return "done", {}

    def _set_arguments(self, arguments):
        self.launch_arguments = list(arguments)
        return "done", {}

    def _attach(self, arguments):
        if len(arguments) != 1:
            raise ValueError("-target-attach requires one pid")
        target = self.debugger.GetSelectedTarget()
        if not target or not target.IsValid():
            target = self.debugger.CreateTarget("")
            self.debugger.SetSelectedTarget(target)
        error = lldb.SBError()
        process = target.AttachToProcessWithID(
            self.debugger.GetListener(), int(arguments[0]), error
        )
        if error.Fail() or not process or not process.IsValid():
            raise RuntimeError(_error_text(error) or "LLDB attach failed")
        self.monitor.snapshot(process)
        return "connected", {}

    def _run(self, arguments):
        target = self._target()
        launch_info = lldb.SBLaunchInfo(self.launch_arguments)
        launch_info.SetEnvironmentEntries(
            ["{}={}".format(name, value) for name, value in os.environ.items()],
            True,
        )
        if "--start" in arguments:
            launch_info.SetLaunchFlags(
                launch_info.GetLaunchFlags() | lldb.eLaunchFlagStopAtEntry
            )
        error = lldb.SBError()
        process = target.Launch(launch_info, error)
        if error.Fail() or not process or not process.IsValid():
            raise RuntimeError(_error_text(error) or "LLDB launch failed")
        self.monitor.snapshot(process)
        return "running", {}

    def _continue(self, _arguments):
        error = self._process().Continue()
        if error.Fail():
            raise RuntimeError(_error_text(error))
        return "running", {}

    def _find_environment_variable(self, name):
        process = self._process()
        target = self._target()
        pointer_size = target.GetAddressByteSize()
        symbols = target.FindSymbols("environ", lldb.eSymbolTypeData)
        for symbol_index in range(symbols.GetSize()):
            symbol = symbols.GetContextAtIndex(symbol_index).GetSymbol()
            if not symbol or not symbol.IsValid():
                continue
            storage_address = symbol.GetStartAddress().GetLoadAddress(target)
            if storage_address == INVALID_ADDRESS:
                continue
            error = lldb.SBError()
            environ_address = process.ReadPointerFromMemory(storage_address, error)
            if error.Fail() or not environ_address:
                continue
            for index in range(4096):
                error = lldb.SBError()
                entry_address = process.ReadPointerFromMemory(
                    environ_address + index * pointer_size,
                    error,
                )
                if error.Fail() or not entry_address:
                    break
                entry = process.ReadCStringFromMemory(entry_address, 4096, error)
                if error.Fail():
                    break
                if entry.startswith(name + "="):
                    return entry_address, entry
        return None

    def _sync_faketime(self):
        pause_started_ns = self.monitor.pause_started_ns
        if pause_started_ns is None:
            return None
        environment = self._find_environment_variable("FAKETIME")
        if environment is None:
            return None
        address, old_entry = environment
        now_ns = time.monotonic_ns()
        if now_ns < pause_started_ns:
            return None
        self.accumulated_pause_seconds += (now_ns - pause_started_ns) / 1e9
        new_entry = "FAKETIME=-{:.9f}".format(self.accumulated_pause_seconds)
        if len(new_entry) > len(old_entry):
            self.emitter.stream(
                "cannot synchronize FAKETIME: existing environment buffer is too small",
                "log",
            )
            return None
        error = lldb.SBError()
        encoded = (new_entry + "\0").encode("utf-8")
        written = self._process().WriteMemory(address, encoded, error)
        if error.Fail() or written != len(encoded):
            self.emitter.stream(
                "failed to synchronize FAKETIME: {}".format(_error_text(error)),
                "log",
            )
            return None
        self.monitor.pause_started_ns = None
        return new_entry

    def _record_time_and_continue(self, arguments):
        faketime = self._sync_faketime()
        message, payload = self._continue(arguments)
        if faketime is not None:
            payload["faketime"] = faketime
        return message, payload

    def _record_time_and_next(self, arguments):
        faketime = self._sync_faketime()
        message, payload = self._next(arguments)
        if faketime is not None:
            payload["faketime"] = faketime
        return message, payload

    def _record_time_and_step(self, arguments):
        faketime = self._sync_faketime()
        message, payload = self._step(arguments)
        if faketime is not None:
            payload["faketime"] = faketime
        return message, payload

    def _record_time_and_finish(self, arguments):
        faketime = self._sync_faketime()
        message, payload = self._finish(arguments)
        if faketime is not None:
            payload["faketime"] = faketime
        return message, payload

    def _interrupt(self, _arguments):
        error = self._process().Stop()
        if error.Fail():
            raise RuntimeError(_error_text(error))
        return "done", {"message": "Interrupted"}

    def _interrupt_if_running(self, arguments):
        process = self._process()
        if process.GetState() in (lldb.eStateRunning, lldb.eStateStepping):
            return self._interrupt(arguments)
        return "done", {"message": "Process not running, no interrupt sent"}

    def _next(self, _arguments):
        self._thread().StepOver()
        return "running", {}

    def _step(self, _arguments):
        self._thread().StepInto()
        return "running", {}

    def _finish(self, _arguments):
        self._thread().StepOut()
        return "running", {}

    def _jump(self, arguments):
        if not arguments:
            raise ValueError("-exec-jump requires a location")
        result = lldb.SBCommandReturnObject()
        self.debugger.GetCommandInterpreter().HandleCommand(
            "thread jump --line {}".format(arguments[-1]), result
        )
        if not result.Succeeded():
            raise RuntimeError(result.GetError())
        return "done", {}

    def _thread_select(self, arguments):
        if len(arguments) != 1:
            raise ValueError("-thread-select requires one thread id")
        process = self._process()
        thread = process.GetThreadByIndexID(int(arguments[0]))
        if not thread or not thread.IsValid():
            raise ValueError("unknown LLDB thread {}".format(arguments[0]))
        process.SetSelectedThread(thread)
        return "done", {"new-thread-id": _text(thread.GetIndexID())}

    def _thread_info(self, arguments):
        process = self._process()
        requested = None
        for argument in arguments:
            if not argument.startswith("-"):
                try:
                    requested = int(argument)
                except ValueError:
                    pass
        threads = []
        for index in range(process.GetNumThreads()):
            thread = process.GetThreadAtIndex(index)
            if requested is not None and thread.GetIndexID() != requested:
                continue
            frame = thread.GetFrameAtIndex(0)
            record = {
                "id": _text(thread.GetIndexID()),
                "target-id": "Thread {}".format(thread.GetThreadID()),
                "name": _text(thread.GetName()),
                "state": _text(lldb.SBDebugger.StateAsCString(process.GetState())),
            }
            if frame and frame.IsValid():
                record["frame"] = _frame_payload(frame, process.GetTarget(), 0)
            threads.append(record)
        selected = process.GetSelectedThread()
        return "done", {
            "threads": threads,
            "current-thread-id": _text(
                selected.GetIndexID() if selected and selected.IsValid() else ""
            ),
        }

    def _thread_groups(self, _arguments):
        process = self._process()
        executable = _file_path(process.GetTarget().GetExecutable())
        return "done", {
            "groups": [
                {
                    "id": GROUP_ID,
                    "type": "process",
                    "pid": _text(process.GetProcessID()),
                    "executable": executable,
                }
            ]
        }

    def _stack_frames(self, arguments):
        thread = self._thread()
        low = 0
        high = thread.GetNumFrames() - 1
        numbers = []
        for argument in arguments:
            try:
                numbers.append(int(argument))
            except ValueError:
                pass
        if numbers:
            low = max(0, numbers[0])
        if len(numbers) > 1:
            high = min(high, numbers[1])
        frames = []
        output_level = 0
        for index in range(low, high + 1):
            frame = thread.GetFrameAtIndex(index)
            if frame and frame.IsValid() and self.filters.include(frame):
                frames.append(
                    _frame_payload(
                        frame,
                        thread.GetProcess().GetTarget(),
                        output_level,
                        include_arguments=False,
                    )
                )
                output_level += 1
        return "done", {"stack": frames}

    def _registers(self):
        frame = self._thread().GetFrameAtIndex(0)
        register_sets = frame.GetRegisters()
        registers = []
        for set_index in range(register_sets.GetSize()):
            register_set = register_sets.GetValueAtIndex(set_index)
            for register_index in range(register_set.GetNumChildren()):
                register = register_set.GetChildAtIndex(register_index)
                if register and register.IsValid():
                    registers.append(register)
        return registers

    def _register_names(self, _arguments):
        return "done", {
            "register-names": [
                _text(register.GetName()) for register in self._registers()
            ]
        }

    def _register_values(self, _arguments):
        return "done", {
            "register-values": [
                {
                    "number": _text(index),
                    "value": _value_text(register),
                }
                for index, register in enumerate(self._registers())
            ]
        }

    def _evaluate_expression(self, arguments):
        if not arguments:
            raise ValueError("-data-evaluate-expression requires an expression")
        expression = " ".join(arguments)
        value = self._thread().GetFrameAtIndex(0).EvaluateExpression(expression)
        if not value or not value.IsValid() or value.GetError().Fail():
            error = value.GetError() if value and value.IsValid() else None
            raise RuntimeError(_error_text(error) or "LLDB expression evaluation failed")
        return "done", {"value": _value_text(value)}

    def _source_files(self, arguments):
        dirname = None
        if "--dirname" in arguments:
            index = arguments.index("--dirname")
            if index + 1 < len(arguments):
                dirname = os.path.realpath(arguments[index + 1])
        paths = set()
        target = self._target()
        for module in target.module_iter():
            for unit in module.compile_unit_iter():
                path = _file_path(unit.GetFileSpec())
                if not path:
                    continue
                full_path = os.path.realpath(_normalized_source_path(path))
                if dirname and not (
                    full_path == dirname or full_path.startswith(dirname + os.sep)
                ):
                    continue
                paths.add(full_path)
        return "done", {
            "files": [
                {"file": os.path.basename(path), "fullname": path}
                for path in sorted(paths)
            ]
        }

    def _source_lines(self, arguments):
        if not arguments:
            raise ValueError("-file-list-lines requires a source file")
        requested = arguments[-1]
        requested_realpath = os.path.realpath(requested)
        target = self._target()
        lines = set()
        for module in target.module_iter():
            for unit in module.compile_unit_iter():
                unit_path = _file_path(unit.GetFileSpec())
                if not unit_path:
                    continue
                unit_path = _normalized_source_path(unit_path)
                if (
                    os.path.realpath(unit_path) != requested_realpath
                    and os.path.basename(unit_path) != os.path.basename(requested)
                ):
                    continue
                for index in range(unit.GetNumLineEntries()):
                    entry = unit.GetLineEntryAtIndex(index)
                    if not entry or not entry.IsValid():
                        continue
                    entry_path = _normalized_source_path(
                        _file_path(entry.GetFileSpec())
                    )
                    if (
                        os.path.realpath(entry_path) != requested_realpath
                        and os.path.basename(entry_path)
                        != os.path.basename(requested)
                    ):
                        continue
                    address = entry.GetStartAddress().GetLoadAddress(target)
                    if address == INVALID_ADDRESS:
                        continue
                    lines.add((int(address), int(entry.GetLine())))
        return "done", {
            "lines": [
                {"pc": "0x{:x}".format(address), "line": _text(line)}
                for address, line in sorted(lines)
            ]
        }

    def _break_insert(self, arguments):
        if not arguments:
            raise ValueError("-break-insert requires a location")
        location = arguments[-1]
        target = self._target()
        breakpoint = None
        if ":" in location:
            filename, line = location.rsplit(":", 1)
            try:
                breakpoint = target.BreakpointCreateByLocation(filename, int(line))
            except ValueError:
                breakpoint = None
        if breakpoint is None or not breakpoint.IsValid():
            breakpoint = target.BreakpointCreateByName(location)
        if not breakpoint or not breakpoint.IsValid() or breakpoint.GetNumLocations() == 0:
            if breakpoint and breakpoint.IsValid():
                target.BreakpointDelete(breakpoint.GetID())
            raise RuntimeError("LLDB could not resolve breakpoint {}".format(location))
        return "done", {
            "bkpt": {
                "number": _text(breakpoint.GetID()),
                "type": "breakpoint",
                "disp": "keep",
                "enabled": "y" if breakpoint.IsEnabled() else "n",
                "times": _text(breakpoint.GetHitCount()),
                "original-location": location,
            }
        }

    def _break_delete(self, arguments):
        if not arguments:
            raise ValueError("-break-delete requires an id")
        target = self._target()
        for breakpoint_id in arguments:
            if not target.BreakpointDelete(int(breakpoint_id)):
                raise ValueError("unknown LLDB breakpoint {}".format(breakpoint_id))
        return "done", {}

    def _switch_context(self, arguments):
        thread = self._thread()
        frame = thread.GetFrameAtIndex(0)
        architecture = self._target().GetTriple().lower()
        register_names = (
            {"pc": "pc", "sp": "sp", "fp": "x29", "lr": "lr"}
            if ("aarch64" in architecture or "arm64" in architecture)
            else {"pc": "rip", "sp": "rsp", "fp": "rbp"}
        )
        old_context = {}
        for assignment in arguments:
            if "=" not in assignment:
                continue
            alias, rendered = assignment.split("=", 1)
            register_name = register_names.get(alias)
            if register_name is None:
                continue
            register = frame.FindRegister(register_name)
            if not register or not register.IsValid():
                raise RuntimeError("LLDB register {} is unavailable".format(register_name))
            old_context[alias] = _text(_value_u64(register))
            if not register.SetValueFromCString(rendered):
                raise RuntimeError("failed to set LLDB register {}".format(register_name))
        return "done", {"message": "success", "old_ctx": old_context}

    def _get_remote_backtrace(self, _arguments):
        thread = self._thread()
        metadata = None
        for index in range(thread.GetNumFrames()):
            frame = thread.GetFrameAtIndex(index)
            function_name = frame.GetFunctionName() or frame.GetDisplayFunctionName() or ""
            if "Backtrace::extraction" not in function_name:
                continue
            candidate = frame.FindVariable("meta")
            if not candidate or not candidate.IsValid():
                candidate = frame.FindVariable("meta_arg")
            if candidate and candidate.IsValid():
                metadata = candidate
                break
        if metadata is None:
            return "done", {"message": "failed"}

        caller_meta = _child(metadata, "meta")
        caller_context = _child(metadata, "ctx")
        if caller_meta is None or caller_context is None:
            return "done", {"message": "failed"}
        context = {}
        for index in range(caller_context.GetNumChildren()):
            register = caller_context.GetChildAtIndex(index)
            name = register.GetName()
            if name:
                context[name] = _text(_value_u64(register))
        os_tid = thread.GetThreadID()
        return "done", {
            "message": "success",
            "metadata": {
                "caller_ctx": context,
                "caller_meta": {
                    "pid": _text(_value_u64(_child(caller_meta, "pid"), -1)),
                    "tid": _text(_value_u64(_child(caller_meta, "tid"), -1)),
                    "ip": _text(
                        _value_u64(_child(caller_meta, "caller_comm_ip"), -1)
                    ),
                    "proclet_id": _text(
                        _value_u64(_child(caller_meta, "proclet_id"), 0)
                    ),
                },
                "local_meta": {"tid": _text(os_tid)},
            },
        }

    @staticmethod
    def _list_signals(_arguments):
        records = []
        for item in signal.Signals:
            records.append(
                {
                    "name": item.name,
                    "stop": "true",
                    "print": "true",
                    "pass": "true",
                    "desc": "",
                }
            )
        return "done", {"signals": records}

    def _interpreter_exec(self, arguments):
        if len(arguments) < 2 or arguments[0] != "console":
            raise ValueError("-interpreter-exec only supports the console interpreter")
        command = " ".join(arguments[1:])
        if command.startswith("signal "):
            command = "process signal " + command[len("signal ") :]
        result = lldb.SBCommandReturnObject()
        self.debugger.GetCommandInterpreter().HandleCommand(command, result)
        if not result.Succeeded():
            raise RuntimeError(result.GetError())
        output = result.GetOutput() or ""
        if output:
            self.emitter.stream(output, "console")
        return "done", {"output": output}

    def _filter_config(self, arguments):
        return "done", self.filters.configure(arguments)

    def _set_stack_prewarm(self, arguments):
        if arguments == ["true"]:
            enabled = True
        elif arguments == ["false"]:
            enabled = False
        else:
            raise ValueError("-ddb-set-stack-prewarm requires true or false")
        self.monitor.stack_prewarm_enabled = enabled
        return "done", {"enabled": _text(enabled).lower()}

    @staticmethod
    def _no_op(_arguments):
        return "done", {}

    def _detach(self, _arguments):
        error = self._process().Detach()
        if error.Fail():
            raise RuntimeError(_error_text(error))
        return "done", {}

    def _kill(self, _arguments):
        error = self._process().Kill()
        if error.Fail():
            raise RuntimeError(_error_text(error))
        return "done", {}

    def _shutdown(self, arguments):
        if arguments == ["detach"]:
            self.monitor.stop()
            self._detach([])
        elif arguments == ["kill"]:
            self.monitor.stop()
            self._kill([])
        else:
            raise ValueError("-ddb-shutdown requires 'detach' or 'kill'")
        self.running = False
        return "exit", {}

    def _exit_bridge(self, _arguments):
        self.running = False
        return "exit", {}


def run(debugger):
    """Take ownership of LLDB stdin and serve DDB JSON requests."""
    bridge = Bridge(debugger)
    try:
        bridge.serve()
    finally:
        bridge.close()
