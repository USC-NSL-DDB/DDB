"""Verify the configured policy when an attached GDB loses its command input."""
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

RUNTIME = Path(__file__).resolve().parents[1] / "assets/gdb_ext/runtime-gdb.py"


class GdbExitPolicyTest(unittest.TestCase):
    def test_transport_eof_obeys_kill_and_detach(self):
        with tempfile.TemporaryDirectory(prefix="ddb-gdb-exit-") as directory:
            source = Path(directory) / "main.c"
            binary = Path(directory) / "main"
            source.write_text("#include <sys/prctl.h>\n#include <unistd.h>\nint main(){prctl(PR_SET_PTRACER, PR_SET_PTRACER_ANY);while(1)sleep(1);}\n")
            subprocess.run(["cc", "-g", str(source), "-o", str(binary)], check=True)
            for policy in ("kill", "detach"):
                for running in (False, True):
                    with self.subTest(policy=policy, running=running):
                        inferior = subprocess.Popen([str(binary)])
                        debugger = None
                        try:
                            time.sleep(0.05)
                            debugger = subprocess.Popen(["gdb", "-q", "--interpreter=mi3"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
                            commands = ["-gdb-set confirm off", "-gdb-set mi-async on", f'-interpreter-exec console "source {RUNTIME}"', f'-interpreter-exec console "set ddb-on-exit {policy}"', f"-target-attach {inferior.pid}"]
                            if running:
                                commands.append("-exec-continue")
                            debugger.stdin.write("\n".join(commands) + "\n")
                            debugger.stdin.flush()
                            # A tokened reply confirms every setup command has finished.
                            debugger.stdin.write("991-list-thread-groups\n")
                            debugger.stdin.flush()
                            output = ""
                            while "991^done" not in output:
                                line = debugger.stdout.readline()
                                self.assertTrue(line, output)
                                output += line
                            debugger.stdin.close()
                            debugger.stdin = None
                            output += debugger.communicate(timeout=5)[0]
                            self.assertNotIn("Undefined set command", output)
                            self.assertEqual(debugger.returncode, 0, output)
                            if policy == "kill":
                                self.assertEqual(inferior.wait(timeout=3), -9, output)
                            else:
                                self.assertIsNone(inferior.poll(), output)
                        finally:
                            if debugger and debugger.poll() is None:
                                debugger.kill()
                                debugger.wait()
                            if inferior.poll() is None:
                                inferior.kill()
                            inferior.wait()


if __name__ == "__main__":
    unittest.main()
