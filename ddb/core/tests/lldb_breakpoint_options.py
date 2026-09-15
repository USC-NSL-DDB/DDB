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

    def test_explicit_function_does_not_become_a_source_location(self):
        self.bridge._break_insert(["--function", "file.c:12"])
        self.target.BreakpointCreateByName.assert_called_once_with("file.c:12")
        self.target.BreakpointCreateByLocation.assert_not_called()


if __name__ == "__main__":
    unittest.main()
