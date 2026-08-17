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
FAKETIME_INITIAL_VALUE = "-00000000000000000000.000000000"
FAKETIME_NO_CACHE_NAME = "FAKETIME_NO_CACHE"
FAKETIME_NO_CACHE_VALUE = "1"
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
    def __init__(self, channel_id):
        if not re.match(r"^[0-9a-f]{32}$", channel_id):
            raise ValueError("invalid DDB LLDB protocol channel id")
        self._lock = threading.Lock()
        self._record_prefix = "{}{}@".format(RECORD_PREFIX, channel_id)

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
            sys.stdout.write(self._record_prefix + encoded + "\n")
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
    def __init__(self, debugger, channel_id):
        self.debugger = debugger
        self.emitter = Emitter(channel_id)
        self.monitor = ProcessMonitor(debugger, self.emitter)
        self.filters = FrameFilters()
        self.launch_arguments = []
        self.running = True
        self.requests = queue.Queue()
        self.reader = None
        self.variable_objects = {}
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
        self.emitter.emit("ready", "ready")
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
                self.emitter.result(token, "error", {"msg": _text(error)})

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
            "-stack-list-variables": self._stack_variables,
            "-data-list-register-names": self._register_names,
            "-data-list-register-values": self._register_values,
            "-var-create": self._var_create,
            "-var-list-children": self._var_list_children,
            "-var-delete": self._var_delete,
            "-data-evaluate-expression": self._evaluate_expression,
            "-data-read-memory-bytes": self._read_memory_bytes,
            "-file-list-exec-source-files": self._source_files,
            "-file-list-lines": self._source_lines,
            "-break-insert": self._break_insert,
            "-break-list": self._break_list,
            "-break-delete": self._break_delete,
            "-break-enable": self._break_enable,
            "-break-disable": self._break_disable,
            "-break-condition": self._break_condition,
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

    def _frame_and_arguments(self, arguments):
        arguments = list(arguments)
        if "--thread" in arguments:
            index = arguments.index("--thread")
            if index + 1 >= len(arguments):
                raise ValueError("--thread requires an index")
            int(arguments[index + 1])
            del arguments[index : index + 2]
        frame_index = 0
        if "--frame" in arguments:
            index = arguments.index("--frame")
            if index + 1 >= len(arguments):
                raise ValueError("--frame requires an index")
            frame_index = int(arguments[index + 1])
            del arguments[index : index + 2]
        frame = self._thread().GetFrameAtIndex(frame_index)
        if not frame or not frame.IsValid():
            raise ValueError("unknown stack frame {}".format(frame_index))
        return frame, arguments

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
        environment = dict(os.environ)
        if "FAKETIME" in environment:
            environment["FAKETIME"] = FAKETIME_INITIAL_VALUE
            environment[FAKETIME_NO_CACHE_NAME] = FAKETIME_NO_CACHE_VALUE
        launch_info = lldb.SBLaunchInfo(self.launch_arguments)
        launch_info.SetEnvironmentEntries(
            ["{}={}".format(name, value) for name, value in environment.items()],
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
        environment = self._find_environment_variable("FAKETIME")
        if environment is None:
            return None

        no_cache = self._find_environment_variable(FAKETIME_NO_CACHE_NAME)
        expected_no_cache = "{}={}".format(
            FAKETIME_NO_CACHE_NAME,
            FAKETIME_NO_CACHE_VALUE,
        )
        if no_cache is None or no_cache[1] != expected_no_cache:
            observed_no_cache = no_cache[1] if no_cache is not None else ""
            raise RuntimeError(
                "cannot synchronize FAKETIME: the inferior must be started with "
                "FAKETIME_NO_CACHE=1 (observed {!r})".format(observed_no_cache)
            )

        pause_started_ns = self.monitor.pause_started_ns
        if pause_started_ns is None:
            raise RuntimeError(
                "cannot synchronize FAKETIME: no debugger pause is active"
            )
        address, old_entry = environment
        now_ns = time.monotonic_ns()
        if now_ns < pause_started_ns:
            raise RuntimeError(
                "cannot synchronize FAKETIME: pause clock moved backwards"
            )
        candidate_accumulated_seconds = round(
            self.accumulated_pause_seconds
            + (now_ns - pause_started_ns) / 1e9,
            9,
        )
        new_entry = "FAKETIME=-{:.9f}".format(candidate_accumulated_seconds)
        if len(new_entry) > len(old_entry):
            raise RuntimeError(
                "cannot synchronize FAKETIME: existing environment buffer is too small"
            )
        error = lldb.SBError()
        process = self._process()
        encoded = (new_entry + "\0").encode("utf-8")
        written = process.WriteMemory(address, encoded, error)
        if error.Fail() or written != len(encoded):
            raise RuntimeError(
                "failed to synchronize FAKETIME: {}".format(_error_text(error))
            )
        verification_error = lldb.SBError()
        observed = process.ReadMemory(address, len(encoded), verification_error)
        if verification_error.Fail() or observed != encoded:
            raise RuntimeError(
                "failed to synchronize FAKETIME: read-after-write verification failed: "
                "{}".format(_error_text(verification_error))
            )
        self.accumulated_pause_seconds = candidate_accumulated_seconds
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
        arguments = list(arguments)
        if "--thread" in arguments:
            index = arguments.index("--thread")
            if index + 1 >= len(arguments):
                raise ValueError("--thread requires an index")
            self._select_thread(arguments[index + 1])
            del arguments[index : index + 2]
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

    def _stack_variables(self, arguments):
        frame, arguments = self._frame_and_arguments(arguments)

        include_values = "--no-values" not in arguments
        variables = []
        values = frame.GetVariables(True, True, True, True)
        for index in range(values.GetSize()):
            value = values.GetValueAtIndex(index)
            if not value or not value.IsValid():
                continue
            record = {
                "name": _text(value.GetName()),
                "type": _text(value.GetTypeName()),
                "numchild": _text(value.GetNumChildren()),
            }
            if include_values:
                record["value"] = _value_text(value)
            variables.append(record)
        return "done", {"variables": variables}

    def _registers(self, frame=None):
        if frame is None:
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

    def _register_values(self, arguments):
        frame, arguments = self._frame_and_arguments(arguments)
        registers = self._registers(frame)
        format_code = "N"
        format_index = None
        for index, argument in enumerate(arguments):
            if argument in ("N", "x", "d", "t"):
                format_code = argument
                format_index = index
                break
        requested = []
        if format_index is not None:
            for argument in arguments[format_index + 1 :]:
                try:
                    requested.append(int(argument))
                except ValueError:
                    raise ValueError("invalid register number {}".format(argument))
        if not requested:
            requested = list(range(len(registers)))

        def rendered(register):
            if format_code == "N":
                return _value_text(register)
            value = _value_u64(register)
            if format_code == "x":
                return "0x{:x}".format(value)
            if format_code == "d":
                return _text(value)
            if format_code == "t":
                return "0b{:b}".format(value)
            raise ValueError("unsupported register format {}".format(format_code))

        return "done", {
            "register-values": [
                {
                    "number": _text(index),
                    "value": rendered(registers[index]),
                }
                for index in requested
                if 0 <= index < len(registers)
            ]
        }

    def _var_create(self, arguments):
        frame, arguments = self._frame_and_arguments(arguments)
        if len(arguments) < 3 or arguments[1] not in ("*", "@"):
            raise ValueError("-var-create requires a name, frame marker, and expression")
        name = arguments[0]
        if name in self.variable_objects:
            raise ValueError("duplicate variable object {}".format(name))
        expression = " ".join(arguments[2:])
        value = frame.FindVariable(expression)
        if not value or not value.IsValid():
            value = frame.EvaluateExpression(expression)
        if not value or not value.IsValid() or value.GetError().Fail():
            error = value.GetError() if value and value.IsValid() else None
            raise RuntimeError(_error_text(error) or "LLDB variable creation failed")
        self.variable_objects[name] = value
        return "done", {
            "name": name,
            "numchild": _text(value.GetNumChildren()),
            "value": _value_text(value),
            "type": _text(value.GetTypeName()),
            "has_more": "0",
        }

    def _var_list_children(self, arguments):
        include_values = "--no-values" not in arguments
        positional = [
            argument
            for argument in arguments
            if argument not in ("--no-values", "--all-values", "--simple-values")
        ]
        if not positional:
            raise ValueError("-var-list-children requires a variable-object name")
        name = positional[0]
        value = self.variable_objects.get(name)
        if value is None or not value.IsValid():
            raise ValueError("unknown variable object {}".format(name))
        count = value.GetNumChildren()
        start = int(positional[1]) if len(positional) > 1 else 0
        end = int(positional[2]) if len(positional) > 2 else count
        if start < 0 or end < 0:
            start = 0
            end = count
        start = min(start, count)
        end = min(max(start, end), count)
        children = []
        for index in range(start, end):
            child = value.GetChildAtIndex(index)
            if not child or not child.IsValid():
                continue
            child_name = "{}.{}".format(name, index)
            self.variable_objects[child_name] = child
            record = {
                "name": child_name,
                "exp": _text(child.GetName() or "[{}]".format(index)),
                "numchild": _text(child.GetNumChildren()),
                "type": _text(child.GetTypeName()),
            }
            if include_values:
                record["value"] = _value_text(child)
            children.append(record)
        return "done", {
            "numchild": _text(count),
            "children": children,
            "has_more": "1" if end < count else "0",
        }

    def _var_delete(self, arguments):
        if len(arguments) != 1:
            raise ValueError("-var-delete requires one variable-object name")
        name = arguments[0]
        matches = [
            key
            for key in self.variable_objects
            if key == name or key.startswith(name + ".")
        ]
        if not matches:
            raise ValueError("unknown variable object {}".format(name))
        for key in matches:
            del self.variable_objects[key]
        return "done", {"ndeleted": _text(len(matches))}

    def _evaluate_expression(self, arguments):
        frame, arguments = self._frame_and_arguments(arguments)
        if not arguments:
            raise ValueError("-data-evaluate-expression requires an expression")
        expression = " ".join(arguments)
        value = frame.EvaluateExpression(expression)
        if not value or not value.IsValid() or value.GetError().Fail():
            error = value.GetError() if value and value.IsValid() else None
            raise RuntimeError(_error_text(error) or "LLDB expression evaluation failed")
        return "done", {"value": _value_text(value)}

    def _read_memory_bytes(self, arguments):
        positional = []
        offset = 0
        index = 0
        while index < len(arguments):
            argument = arguments[index]
            if argument == "-o":
                if index + 1 >= len(arguments):
                    raise ValueError("-data-read-memory-bytes -o requires an offset")
                offset = int(arguments[index + 1], 0)
                index += 2
                continue
            if argument == "--":
                positional.extend(arguments[index + 1 :])
                break
            positional.append(argument)
            index += 1
        if len(positional) < 2:
            raise ValueError("-data-read-memory-bytes requires an address and count")
        address_expression = positional[0]
        try:
            address = int(address_expression, 0)
        except ValueError:
            value = self._thread().GetFrameAtIndex(0).EvaluateExpression(
                address_expression
            )
            if not value or not value.IsValid() or value.GetError().Fail():
                error = value.GetError() if value and value.IsValid() else None
                raise RuntimeError(
                    _error_text(error) or "LLDB address expression evaluation failed"
                )
            address = value.GetValueAsUnsigned()
        count = int(positional[1], 0)
        if count <= 0:
            raise ValueError("memory byte count must be positive")
        begin = address + offset
        error = lldb.SBError()
        data = self._process().ReadMemory(begin, count, error)
        if error.Fail():
            raise RuntimeError(_error_text(error) or "LLDB memory read failed")
        if not isinstance(data, bytes):
            data = bytes(data)
        return "done", {
            "memory": [
                {
                    "begin": hex(begin),
                    "offset": _text(offset),
                    "end": hex(begin + len(data)),
                    "contents": data.hex(),
                }
            ]
        }

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
        condition = None
        enabled = True
        temporary = False
        positional = []
        index = 0
        while index < len(arguments):
            argument = arguments[index]
            if argument in ("-d", "--disabled"):
                enabled = False
                index += 1
                continue
            if argument in ("-t", "--temporary"):
                temporary = True
                index += 1
                continue
            if argument in ("-h", "--hardware"):
                raise ValueError("hardware breakpoints are unsupported by this LLDB bridge")
            if argument in ("-c", "--condition"):
                if index + 1 >= len(arguments):
                    raise ValueError("-break-insert -c requires a condition")
                condition = arguments[index + 1]
                index += 2
                continue
            if argument == "--":
                positional.extend(arguments[index + 1 :])
                break
            if argument == "-f":
                # GDB's pending-breakpoint flag needs no LLDB equivalent.
                index += 1
                continue
            if argument.startswith("-"):
                raise ValueError(
                    "unsupported -break-insert option: {}".format(argument)
                )
            positional.append(argument)
            index += 1
        if len(positional) != 1:
            raise ValueError("-break-insert requires exactly one location")
        location = positional[0]
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
        try:
            if not enabled:
                breakpoint.SetEnabled(False)
                if breakpoint.IsEnabled():
                    raise RuntimeError("LLDB did not retain the disabled breakpoint state")
            if condition is not None:
                breakpoint.SetCondition(condition)
                if breakpoint.GetCondition() != condition:
                    raise RuntimeError("LLDB did not retain the breakpoint condition")
            if temporary:
                breakpoint.SetOneShot(True)
                if not breakpoint.IsOneShot():
                    raise RuntimeError("LLDB did not retain the temporary breakpoint flag")
        except Exception:
            target.BreakpointDelete(breakpoint.GetID())
            raise
        return "done", {
            "bkpt": {
                "number": _text(breakpoint.GetID()),
                "type": "breakpoint",
                "disp": "del" if breakpoint.IsOneShot() else "keep",
                "enabled": "y" if breakpoint.IsEnabled() else "n",
                "times": _text(breakpoint.GetHitCount()),
                "original-location": location,
                **({"cond": condition} if condition is not None else {}),
            }
        }

    def _break_list(self, _arguments):
        target = self._target()
        body = []
        for index in range(target.GetNumBreakpoints()):
            breakpoint = target.GetBreakpointAtIndex(index)
            if not breakpoint or not breakpoint.IsValid():
                continue
            details = {
                "number": _text(breakpoint.GetID()),
                "type": "breakpoint",
                "disp": "del" if breakpoint.IsOneShot() else "keep",
                "enabled": "y" if breakpoint.IsEnabled() else "n",
                "times": _text(breakpoint.GetHitCount()),
            }
            condition = breakpoint.GetCondition() or ""
            if condition:
                details["cond"] = condition
            if breakpoint.GetNumLocations() > 0:
                location = breakpoint.GetLocationAtIndex(0)
                address = location.GetAddress()
                load_address = address.GetLoadAddress(target)
                if load_address != INVALID_ADDRESS:
                    details["addr"] = "0x{:x}".format(load_address)
                line_entry = address.GetLineEntry()
                if line_entry and line_entry.IsValid():
                    path = _normalized_source_path(
                        _file_path(line_entry.GetFileSpec())
                    )
                    line = int(line_entry.GetLine())
                    details["file"] = os.path.basename(path)
                    details["fullname"] = path
                    details["line"] = _text(line)
                    details["original-location"] = "{}:{}".format(path, line)
            body.append({"bkpt": details})
        return "done", {
            "BreakpointTable": {
                "nr_rows": _text(len(body)),
                "nr_cols": "6",
                "hdr": [],
                "body": body,
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

    def _breakpoint(self, breakpoint_id):
        try:
            parsed_id = int(breakpoint_id)
        except (TypeError, ValueError):
            raise ValueError("invalid LLDB breakpoint id {}".format(breakpoint_id))
        breakpoint = self._target().FindBreakpointByID(parsed_id)
        if not breakpoint or not breakpoint.IsValid():
            raise ValueError("unknown LLDB breakpoint {}".format(breakpoint_id))
        return breakpoint

    def _set_breakpoints_enabled(self, arguments, enabled):
        if not arguments:
            raise ValueError("breakpoint enable/disable requires an id")
        for breakpoint_id in arguments:
            breakpoint = self._breakpoint(breakpoint_id)
            breakpoint.SetEnabled(enabled)
            if breakpoint.IsEnabled() != enabled:
                raise RuntimeError(
                    "LLDB did not {} breakpoint {}".format(
                        "enable" if enabled else "disable", breakpoint_id
                    )
                )
        return "done", {}

    def _break_enable(self, arguments):
        return self._set_breakpoints_enabled(arguments, True)

    def _break_disable(self, arguments):
        return self._set_breakpoints_enabled(arguments, False)

    def _break_condition(self, arguments):
        if not arguments:
            raise ValueError("-break-condition requires an id")
        breakpoint = self._breakpoint(arguments[0])
        condition = " ".join(arguments[1:])
        breakpoint.SetCondition(condition)
        retained = breakpoint.GetCondition() or ""
        if retained != condition:
            raise RuntimeError("LLDB did not retain the breakpoint condition")
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


def run(debugger, channel_id):
    """Take ownership of LLDB stdin and serve DDB JSON requests."""
    bridge = Bridge(debugger, channel_id)
    try:
        bridge.serve()
    finally:
        bridge.close()
