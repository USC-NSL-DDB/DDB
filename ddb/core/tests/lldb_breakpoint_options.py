"""Exercise LLDB bridge option handling without requiring an LLDB installation."""
import importlib.util
from pathlib import Path
import sys
import types
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True

bridge_path = Path(__file__).parents[1] / "assets/lldb_ext/ddb_lldb_bridge.py"
spec = importlib.util.spec_from_file_location("ddb_lldb_bridge", bridge_path)
bridge_module = importlib.util.module_from_spec(spec)
with patch.dict(sys.modules, {"lldb": types.ModuleType("lldb")}):
    spec.loader.exec_module(bridge_module)


class SourceDirectoryPatternTest(unittest.TestCase):
    def setUp(self):
        self.paths = [
            "/repo/src[x]/requested.c",
            "/repo/src[x]/nested/child.c",
            "/repo/src[x]-other/other.c",
            "/other/repo/src[x]/unrelated.c",
            r'/repo/src+(){}^$?*|\"/quoted.c',
            "/root_file.c",
        ]
        units = []
        for path in self.paths:
            unit = Mock()
            unit.GetFileSpec.return_value = path
            units.append(unit)
        module = Mock()
        module.compile_unit_iter.return_value = units
        target = Mock()
        target.module_iter.return_value = [module]
        self.bridge = object.__new__(bridge_module.Bridge)
        self.bridge._target = Mock(return_value=target)
        self.file_path = patch.object(bridge_module, "_file_path", side_effect=lambda path: path)
        self.file_path.start()
        self.addCleanup(self.file_path.stop)

    def sources(self, arguments):
        result, payload = self.bridge._source_files(arguments)
        self.assertEqual(result, "done")
        return [source["fullname"] for source in payload["files"]]

    def test_literal_directory_expression_matches_exact_parent(self):
        self.assertEqual(
            self.sources(["--dirname", r"^/repo/src\[x\]$"]),
            self.paths[:1],
        )

    def test_directory_expression_matches_directory_not_basename(self):
        self.assertEqual(self.sources(["--dirname", "nested$"]), [self.paths[1]])
        self.assertEqual(self.sources(["--dirname", "requested"]), [])

    def test_unfiltered_and_exact_root_queries(self):
        self.assertEqual(self.sources([]), sorted(self.paths))
        self.assertEqual(self.sources(["--dirname", "^/$"]), [self.paths[-1]])

    def test_portable_literals_survive_mi_command_tokenization(self):
        pattern = r'^/repo/src[+][(][)][{][}]\^\$[?]\*[|]\\' + '"$'
        command = "-file-list-exec-source-files --dirname " + bridge_module.json.dumps(pattern)
        self.bridge._select_thread = Mock()
        result, payload = self.bridge.execute(command)
        self.assertEqual(result, "done")
        self.assertEqual([source["fullname"] for source in payload["files"]], [self.paths[-2]])

    def test_missing_or_invalid_expression_is_not_an_empty_listing(self):
        with self.assertRaisesRegex(ValueError, "requires a regular expression"):
            self.sources(["--dirname"])
        with self.assertRaises(bridge_module.re.error):
            self.sources(["--dirname", "["])
        self.bridge._target.assert_not_called()

    def test_source_probe_accepts_only_an_exact_debugger_path(self):
        self.bridge._select_thread = Mock()
        for path, expected in [(self.paths[0], [self.paths[0]]), ("/not-compiled/requested.c", [])]:
            result, payload = self.bridge.execute("-ddb-probe-source " + bridge_module.json.dumps(path))
            self.assertEqual(result, "done")
            self.assertEqual([source["fullname"] for source in payload["files"]], expected)
            self.assertEqual(payload["source_status"], "present" if expected else "unknown")

    def test_source_probe_rejects_missing_and_relative_paths(self):
        self.bridge._select_thread = Mock()
        for command in ["-ddb-probe-source", "-ddb-probe-source requested.c"]:
            with self.assertRaises(ValueError):
                self.bridge.execute(command)


class StorageAddressTest(unittest.TestCase):
    def value(self, address):
        value = Mock()
        value.IsValid.return_value = True
        value.GetError.return_value.Fail.return_value = False
        value.GetLoadAddress.return_value = address
        return value

    def test_storage_address_is_not_displayed_pointer_value(self):
        value = self.value(0x1234)
        value.GetValue.return_value = "0x9876"
        self.assertEqual(bridge_module._storage_address_metadata(value), {"address": "0x1234"})
        value.GetValue.assert_not_called()

    def test_invalid_and_unavailable_values_have_no_address(self):
        value = self.value(bridge_module.INVALID_ADDRESS)
        self.assertEqual(bridge_module._storage_address_metadata(value), {})
        value.GetLoadAddress.reset_mock()
        value.GetError.return_value.Fail.return_value = True
        self.assertEqual(bridge_module._storage_address_metadata(value), {})
        value.GetLoadAddress.assert_not_called()

    def test_scope_lookahead_does_not_resolve_storage(self):
        bridge = object.__new__(bridge_module.Bridge)
        first, lookahead = self.value(0x1000), self.value(0x2000)
        first.GetName.return_value = "first"
        lookahead.GetName.return_value = "second"
        first.GetExpressionPath.return_value = False
        bridge._frame_and_arguments = Mock(return_value=(Mock(), ["globals", "0", "2"]))
        bridge._scope_symbols = Mock(return_value=[
            (("first", "file", 1), first), (("second", "file", 2), lookahead),
        ])
        with patch.object(bridge_module.lldb, "SBStream", Mock(), create=True):
            _, payload = bridge._list_scope_variables([])
        self.assertEqual(payload["variables"][0]["address"], "0x1000")
        self.assertNotIn("address", payload["variables"][1])
        lookahead.GetLoadAddress.assert_not_called()

    def test_no_values_stack_does_not_resolve_storage(self):
        bridge = object.__new__(bridge_module.Bridge)
        value = self.value(0x1000)
        bridge._frame_and_arguments = Mock(return_value=(Mock(), ["--no-values"]))
        bridge._lexical_variables = Mock(return_value=[value])
        _, payload = bridge._stack_variables([])
        self.assertNotIn("address", payload["variables"][0])
        value.GetLoadAddress.assert_not_called()


class ProcessMonitorTest(unittest.TestCase):
    def setUp(self):
        self.api = patch.multiple(
            bridge_module.lldb,
            SBListener=Mock(),
            SBEvent=Mock(),
            SBProcess=Mock(),
            eStateStopped=5,
            eStateRunning=6,
            eStateStepping=7,
            eStateCrashed=8,
            eStateSuspended=11,
            create=True,
        )
        self.api.start()
        self.addCleanup(self.api.stop)
        self.emitter = Mock()
        self.monitor = bridge_module.ProcessMonitor(Mock(), self.emitter)
        self.monitor._ensure_process = Mock()
        self.monitor._sync_threads = Mock()
        self.monitor._stop_payload = Mock(return_value={"reason": "entry"})
        self.process = Mock()
        self.process.GetState.return_value = 5
        self.process.GetStopID.return_value = 1
        self.thread = Mock()
        self.thread.IsValid.return_value = False
        self.process.GetSelectedThread.return_value = self.thread
        self.process.GetThreadAtIndex.return_value = self.thread

    def test_launch_snapshot_waits_for_an_inspectable_stop(self):
        self.monitor.snapshot(self.process)
        self.emitter.event.assert_not_called()
        self.monitor._sync_threads.assert_not_called()
        self.assertIsNone(self.monitor.pause_started_ns)

        self.thread.IsValid.return_value = True
        self.monitor.snapshot(self.process)
        self.emitter.event.assert_called_once_with("stopped", {"reason": "entry"})
        self.assertIsNotNone(self.monitor.pause_started_ns)
        self.monitor.snapshot(self.process)
        self.emitter.event.assert_called_once()

    def test_stale_stop_does_not_replace_current_running_state(self):
        self.thread.IsValid.return_value = True
        self.process.GetState.return_value = 6
        self.monitor._publish_state(self.process, 5)
        self.emitter.event.assert_not_called()
        self.assertIsNone(self.monitor.pause_started_ns)

    def test_temporary_breakpoint_deletion_follows_the_identified_stop(self):
        self.thread.IsValid.return_value = True
        self.monitor.temporary_breakpoints = {7}
        self.monitor._stop_payload.return_value = {
            "reason": "breakpoint-hit", "bkptno": "7", "thread-id": "1"
        }
        target = self.process.GetTarget.return_value
        def emitted(kind, payload):
            if kind == "stopped":
                target.BreakpointDelete.assert_not_called()
                self.assertEqual(payload["bkptno"], "7")
        self.emitter.event.side_effect = emitted
        self.monitor.snapshot(self.process)
        self.assertEqual(self.emitter.event.call_args_list[-1],
            unittest.mock.call("breakpoint-deleted", {"id": "7"}))
        target.BreakpointDelete.assert_called_once_with(7)
        self.assertEqual(self.monitor.temporary_breakpoints, set())

    def test_automatically_restarted_event_is_not_a_user_stop(self):
        api = bridge_module.lldb.SBProcess
        api.EventIsProcessEvent.return_value = True
        api.GetProcessFromEvent.return_value = self.process
        api.GetStateFromEvent.return_value = 5
        api.GetRestartedFromEvent.return_value = True
        self.monitor.listener.GetNextEvent.side_effect = [True, False]
        self.monitor._group_started = True
        self.monitor.debugger.GetSelectedTarget.return_value.GetProcess.return_value = self.process
        self.monitor._publish_state = Mock()
        self.monitor.poll()
        self.monitor._publish_state.assert_not_called()

    def test_running_launch_publishes_threads_that_become_visible_without_an_event(self):
        # LLDB can report a running process before its native thread list is
        # available. A program waiting for its parent need not stop again.
        self.process.GetState.return_value = 6
        self.process.GetNumThreads.side_effect = [0, 1]
        self.thread.GetIndexID.return_value = 1
        self.monitor._sync_threads = (
            bridge_module.ProcessMonitor._sync_threads.__get__(self.monitor)
        )
        self.monitor._group_started = True
        self.monitor.debugger.GetSelectedTarget.return_value.GetProcess.return_value = self.process
        self.monitor.listener.GetNextEvent.return_value = False
        self.monitor.snapshot(self.process)
        self.assertFalse(any(
            call.args[0] == "thread-created"
            for call in self.emitter.event.call_args_list
        ))
        self.monitor.poll()
        self.assertIn(
            unittest.mock.call(
                "thread-created", {"id": "1", "group-id": bridge_module.GROUP_ID}
            ),
            self.emitter.event.call_args_list,
        )
        self.monitor.poll()
        self.assertEqual(sum(
            call.args[0] == "thread-created"
            for call in self.emitter.event.call_args_list
        ), 1)


class BreakpointOptionsTest(unittest.TestCase):
    def setUp(self):
        self.breakpoint = Mock()
        self.breakpoint.IsValid.return_value = True
        self.breakpoint.GetNumLocations.return_value = 1
        self.breakpoint.GetID.return_value = 7
        self.breakpoint.GetHitCount.return_value = 0
        self.breakpoint.IsOneShot.return_value = False
        self.breakpoint.IsEnabled.return_value = True
        self.target = Mock()
        self.target.BreakpointCreateByName.return_value = self.breakpoint
        self.target.BreakpointCreateByLocation.return_value = self.breakpoint
        self.bridge = object.__new__(bridge_module.Bridge)
        self.bridge._target = lambda: self.target
        self.bridge.monitor = types.SimpleNamespace(temporary_breakpoints=set())

    def test_temporary_breakpoint_keeps_its_native_identity_until_projection(self):
        _, payload = self.bridge._break_insert(["-t", "--function", "tick"])
        self.breakpoint.SetOneShot.assert_not_called()
        self.assertEqual(self.bridge.monitor.temporary_breakpoints, {7})
        self.assertEqual(payload["bkpt"]["disp"], "del")

    def test_explicit_function_keeps_the_name_and_condition(self):
        name = "worker::tick(int, int)"
        self.breakpoint.GetCondition.return_value = "counter > 0"
        result, payload = self.bridge._break_insert(
            ["-c", "counter > 0", "--function", name]
        )
        self.assertEqual(result, "done")
        self.target.BreakpointCreateByName.assert_called_once_with(name)
        self.target.BreakpointCreateByLocation.assert_not_called()
        self.breakpoint.SetCondition.assert_called_once_with("counter > 0")
        self.assertEqual(payload["bkpt"]["original-location"], name)

    def test_ignore_count_is_installed_before_the_breakpoint_is_returned(self):
        self.breakpoint.GetIgnoreCount.return_value = 3
        self.bridge._break_insert(["-i", "3", "--function", "tick"])
        self.breakpoint.SetIgnoreCount.assert_called_once_with(3)

    def test_ignore_count_failure_rolls_back_the_breakpoint(self):
        self.breakpoint.GetIgnoreCount.return_value = 0
        with self.assertRaisesRegex(RuntimeError, "ignore count"):
            self.bridge._break_insert(["-i", "3", "--function", "tick"])
        self.target.BreakpointDelete.assert_called_once_with(7)

    def test_explicit_function_does_not_become_a_source_location(self):
        self.bridge._break_insert(["--function", "file.c:12"])
        self.target.BreakpointCreateByName.assert_called_once_with("file.c:12")
        self.target.BreakpointCreateByLocation.assert_not_called()


class NativeConsoleOptionsTest(unittest.TestCase):
    def test_explicit_frame_is_passed_to_the_command_interpreter(self):
        bridge = object.__new__(bridge_module.Bridge)
        bridge.debugger = Mock()
        bridge.emitter = Mock()
        frame = Mock()
        thread = Mock()
        thread.GetFrameAtIndex.return_value = frame
        bridge._thread = lambda: thread
        result = Mock()
        result.GetOutput.return_value = "value = 42\n"
        context = Mock()
        lldb = types.SimpleNamespace(
            SBCommandReturnObject=lambda: result,
            SBExecutionContext=Mock(return_value=context),
        )
        with patch.object(bridge_module, "lldb", lldb):
            status, payload = bridge._interpreter_exec(
                ["--frame", "2", "console", "frame variable value"]
            )
        thread.GetFrameAtIndex.assert_called_once_with(2)
        lldb.SBExecutionContext.assert_called_once_with(frame)
        bridge.debugger.GetCommandInterpreter().HandleCommand.assert_called_once_with(
            "frame variable value", context, result
        )
        self.assertEqual((status, payload), ("done", {"output": "value = 42\n"}))


class VariableAssignmentTest(unittest.TestCase):
    def test_assigns_stored_child_and_reports_backend_rejection(self):
        bridge = object.__new__(bridge_module.Bridge)
        child = Mock()
        child.IsValid.return_value = True
        child.SetValueFromCString.return_value = True
        child.GetValue.return_value = "17"
        child.GetSummary.return_value = None
        bridge.variable_objects = {"root.0": child}
        error = Mock()
        error.GetCString.return_value = "value is read-only"
        with patch.object(bridge_module.lldb, "SBError", return_value=error, create=True):
            result, payload = bridge._var_assign(["root.0", "17"])
            self.assertEqual((result, payload), ("done", {"value": "17"}))
            child.SetValueFromCString.assert_called_once_with("17", error)
            child.SetValueFromCString.return_value = False
            with self.assertRaisesRegex(RuntimeError, "read-only"):
                bridge._var_assign(["root.0", "18"])


if __name__ == "__main__":
    unittest.main()
