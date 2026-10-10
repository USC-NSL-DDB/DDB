"""Test the production GDB storage resolver without starting a debuggee."""
import ast
from pathlib import Path
import re
import types
import unittest
from unittest.mock import Mock


runtime = Path(__file__).parents[1] / "assets/gdb_ext/runtime-gdb.py"
names = {"storage_path", "storage_value", "storage_metadata",
         "VariableCreateMICommand", "VariableChildrenMICommand"}
tree = ast.parse(runtime.read_text())
definitions = ast.Module(body=[node for node in tree.body
                              if getattr(node, "name", None) in names], type_ignores=[])


class StoragePathTest(unittest.TestCase):
    def setUp(self):
        self.gdb = types.SimpleNamespace(error=RuntimeError, MICommand=object,
                                        execute_mi=Mock())
        self.namespace = {"re": re, "gdb": self.gdb}
        exec(compile(definitions, str(runtime), "exec"), self.namespace)
        self.parse = self.namespace["storage_path"]

    def test_native_paths_preserve_precedence_and_scope(self):
        self.assertEqual(self.parse("((request).headers)[1]"),
                         ("index", ("field", ("symbol", "request"), "headers"), 1))
        self.assertEqual(self.parse("(*ptr).member"),
                         ("field", ("deref", ("symbol", "ptr")), "member"))
        self.assertEqual(self.parse("*ptr.member"),
                         ("deref", ("field", ("symbol", "ptr"), "member")))
        self.assertEqual(self.parse("ptr->member"),
                         ("field", ("deref", ("symbol", "ptr")), "member"))
        self.assertEqual(self.parse("$ddb_scope_symbol(1, 2).field"),
                         ("field", ("scope", 1, 2), "field"))
        self.assertEqual(self.parse("::space::value"), ("symbol", "::space::value"))

    def test_unsupported_expressions_never_read_native_values(self):
        read = Mock()
        self.namespace["storage_value"] = read
        for expression in ["call()", "value++", "value = 3", "array[call()]",
                           "(Thing*)ptr", "value + 1", "obj[index]", "a; b",
                           "x" * 4097, "(" * 80 + "x" + ")" * 80]:
            with self.subTest(expression=expression[:30]):
                self.assertEqual(self.namespace["storage_metadata"](expression), {})
        read.assert_not_called()

    def test_variable_creation_does_not_evaluate_watch_twice(self):
        self.gdb.execute_mi.return_value = {"name": "object", "value": "7"}
        self.namespace["storage_value"] = Mock()
        command = object.__new__(self.namespace["VariableCreateMICommand"])
        result = command.invoke(["object", "*", "call()"])
        self.assertNotIn("address", result)
        self.gdb.execute_mi.assert_called_once_with("-var-create", "object", "*", "call()")
        self.namespace["storage_value"].assert_not_called()

    def test_no_values_children_do_not_query_paths(self):
        self.gdb.execute_mi.return_value = {"children": [{"name": "object.0"}]}
        command = object.__new__(self.namespace["VariableChildrenMICommand"])
        command.invoke(["--no-values", "object", "0", "1"])
        self.gdb.execute_mi.assert_called_once_with(
            "-var-list-children", "--no-values", "object", "0", "1")


if __name__ == "__main__":
    unittest.main()
