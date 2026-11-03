"""Exercise real private-file ownership and process-tree finalization."""

from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

from supervision import SupervisionFailure, _owned_children, run_owned


class NativeSupervision(unittest.TestCase):
    def test_selected_profile_reaches_real_child(self):
        receipt, stdout = self.invoke("import resource; print(resource.getrlimit(resource.RLIMIT_AS)[0])")
        self.assertEqual(int(stdout.strip()), 8 * 1024**3)
        self.assertEqual(receipt["addressLimitBytes"], 8 * 1024**3)
        self.assertEqual(receipt["documentDeadlineSeconds"], 300)

    def invoke(self, code, **limits):
        with tempfile.TemporaryDirectory() as output:
            directory = Path(output)
            receipt = run_owned([sys.executable, "-c", code], b"{}", directory, **limits)
            stdout = (directory / "stdout.json").read_text()
            self.assertTrue(receipt["scratchRemoved"])
            self.assertFalse(Path(receipt["scratchPath"]).exists())
            self.assertTrue(receipt["processGroupGone"])
            self.assertEqual(receipt["remainingChildren"], [])
            self.assertTrue((directory / "stderr.out").exists())
            return receipt, stdout

    def test_parent_cleans_arbitrary_library_scratch_after_success(self):
        receipt, stdout = self.invoke(
            "import os,pathlib; pathlib.Path(os.environ['TMPDIR'],'opaque.scratch').write_text('owned'); print('retained')"
        )
        self.assertEqual(receipt["exit"], 0)
        self.assertIn("opaque.scratch", receipt["temporaryEntriesBeforeCleanup"])
        self.assertEqual(stdout.strip(), "retained")

    def test_failure_still_cleans_private_scope(self):
        receipt, _ = self.invoke(
            "import os,pathlib; pathlib.Path(os.environ['TMPDIR'],'opaque.scratch').touch(); raise SystemExit(7)"
        )
        self.assertEqual(receipt["exit"], 7)
        self.assertTrue(receipt["scratchRemoved"])

    def test_successful_root_cannot_leave_an_orphan(self):
        receipt, stdout = self.invoke(
            "import subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()']); print(child.pid,flush=True)"
        )
        self.assertEqual(receipt["exit"], 0)
        self.assertIn(int(stdout.strip()), [child["pid"] for child in receipt["descendantsReaped"]])

    def test_adopted_descendant_cannot_escape_by_starting_a_session(self):
        receipt, stdout = self.invoke(
            "import subprocess,sys; child=subprocess.Popen([sys.executable,'-c','import signal; signal.pause()'],start_new_session=True); print(child.pid,flush=True)"
        )
        self.assertEqual(receipt["exit"], 0)
        self.assertIn(int(stdout.strip()), [child["pid"] for child in receipt["descendantsReaped"]])

    def test_forced_timeout_reaps_root_and_descendant_before_cleanup(self):
        receipt, stdout = self.invoke(
            "import os,pathlib,signal,subprocess,sys; "
            "signal.signal(signal.SIGTERM,signal.SIG_IGN); "
            "pathlib.Path(os.environ['TMPDIR'],'opaque.scratch').touch(); "
            "child=subprocess.Popen([sys.executable,'-c','import signal; signal.signal(signal.SIGTERM,signal.SIG_IGN); signal.pause()']); "
            "print(child.pid,flush=True); signal.pause()",
            timeout=1, grace=1,
        )
        self.assertTrue(receipt["timeout"])
        self.assertEqual(receipt["exit"], -9)
        self.assertIn(int(stdout.strip()), [child["pid"] for child in receipt["descendantsReaped"]])

    def test_parent_interrupt_still_reaps_and_closes_scope(self):
        with tempfile.TemporaryDirectory() as output:
            directory = Path(output)
            code = (
                "import os,signal,sys; sys.stdin.buffer.read(); "
                "print(os.getpid(),flush=True); os.kill(os.getppid(),signal.SIGINT); signal.pause()"
            )
            with self.assertRaises(KeyboardInterrupt):
                run_owned([sys.executable, "-c", code], b"{}", directory)
            self.assertEqual(_owned_children(), [])
            self.assertEqual(list(directory.glob("scratch-*")), [])

    def test_checked_cleanup_error_cannot_return_success(self):
        with tempfile.TemporaryDirectory() as output:
            real_cleanup = tempfile.TemporaryDirectory.cleanup

            def cleanup_error(scope):
                real_cleanup(scope)
                raise PermissionError("injected checked-cleanup error")

            with patch.object(tempfile.TemporaryDirectory, "cleanup", cleanup_error):
                with self.assertRaises(SupervisionFailure):
                    run_owned([sys.executable, "-c", "pass"], b"{}", Path(output))
            self.assertEqual(_owned_children(), [])


if __name__ == "__main__":
    unittest.main()
