"""Regression checks for the executable CI coverage policy."""

import importlib.util
from pathlib import Path
import unittest

policy_path = Path(__file__).resolve().parents[1] / "check-ci-workflow.py"
spec = importlib.util.spec_from_file_location("ci_policy", policy_path)
policy = importlib.util.module_from_spec(spec)
spec.loader.exec_module(policy)


class WorkflowPolicyTest(unittest.TestCase):
    def setUp(self):
        self.workflow = policy.WORKFLOW.read_text(encoding="utf-8")
        self.docs = policy.DOCS_WORKFLOW.read_text(encoding="utf-8")
        self.fuzz = policy.FUZZ_WORKFLOW.read_text(encoding="utf-8")

    def validate(self, workflow):
        return policy.validate_workflows(workflow, self.docs, self.fuzz)

    def test_checked_in_workflows_satisfy_policy(self):
        self.assertEqual(self.validate(self.workflow), [])

    def test_fixture_display_name_is_not_its_identity(self):
        renamed = self.workflow.replace(
            "Build debuggable real fixtures for TUI PTY tests", "Build native fixtures"
        )
        self.assertEqual(self.validate(renamed), [])

    def test_unrelated_timeout_does_not_satisfy_a_package_install_timeout(self):
        changed = self.workflow.replace(
            "      - name: Install debugger test dependencies\n        timeout-minutes: 5\n",
            "      - name: Install debugger test dependencies\n",
        )
        self.assertIn(
            "all package installs need five-minute step timeouts", self.validate(changed)
        )

    def test_removing_either_fixture_build_is_rejected(self):
        for fixture in ("real_loop", "real_dbt"):
            with self.subTest(fixture=fixture):
                changed = self.workflow.replace(
                    f"--manifest-path ddb/core/tests/fixtures/{fixture}/Cargo.toml", ""
                )
                self.assertTrue(any(
                    "--manifest-path" in error for error in self.validate(changed)
                ))

    def test_missing_identity_or_debug_information_is_rejected(self):
        for required in (
            "id: build-debug-fixtures",
            "id: grpc-source",
            "id: build-greeters",
            "id: greeter-runtime",
            "CARGO_PROFILE_DEV_DEBUG: '2'",
            "CARGO_PROFILE_DEV_OPT_LEVEL: '0'",
        ):
            with self.subTest(required=required):
                self.assertTrue(self.validate(self.workflow.replace(required, "")))

    def test_greeter_tests_require_provisioning_and_serial_execution(self):
        for required in (
            "DDB_GREETER_WORKSPACE=", "mosquitto", "libpaho-mqtt-dev", "--test-threads=1"
        ):
            with self.subTest(required=required):
                self.assertTrue(self.validate(self.workflow.replace(required, "")))

    def test_greeter_display_names_are_not_gate_identifiers(self):
        for name in (
            "Initialize pinned instrumented gRPC",
            "Build instrumented auto-attach greeters",
            "Install greeter launch runtime",
        ):
            with self.subTest(name=name):
                self.assertEqual(self.validate(self.workflow.replace(name, "Greeter setup")), [])


if __name__ == "__main__":
    unittest.main()
