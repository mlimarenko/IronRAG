import os
import pathlib
import subprocess
import sys
import tempfile
import unittest

GUARD = pathlib.Path(__file__).resolve().parent / "check_schema_rollout_fence.py"
REPO_MARKER = (
    pathlib.Path(__file__).resolve().parents[2] / "apps/api/ops/schema-rollout.pending"
)


def run_guard(
    *, marker: pathlib.Path | None = None, cwd: str | None = None
) -> subprocess.CompletedProcess[str]:
    args = [sys.executable, str(GUARD)]
    if marker is not None:
        args += ["--marker", str(marker)]
    return subprocess.run(
        args, cwd=cwd, capture_output=True, text=True, timeout=10, check=False
    )


class SchemaRolloutFenceTests(unittest.TestCase):
    def test_absent_marker_permits(self):
        with tempfile.TemporaryDirectory() as directory:
            proc = run_guard(marker=pathlib.Path(directory) / "schema-rollout.pending")
        self.assertEqual(proc.returncode, 0, proc.stderr)

    def test_present_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            marker.write_text("IRG-92\n" + "a" * 96 + "\n")
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_empty_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            marker.write_text("")
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_marker_contents_are_not_echoed(self):
        # The guard must never print the marker body: a present-but-unreadable or
        # arbitrary payload could otherwise leak untrusted input into the log.
        untrusted_line = "untrusted-marker-body-0123456789abcdef"
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            marker.write_text(f"IRG-92\n{untrusted_line}\n")
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)
        self.assertNotIn(untrusted_line, proc.stderr)
        self.assertNotIn(untrusted_line, proc.stdout)

    def test_directory_as_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            marker.mkdir()
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_symlink_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            target = pathlib.Path(directory) / "target.txt"
            target.write_text("x")
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            marker.symlink_to(target)
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_dangling_symlink_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            marker.symlink_to(pathlib.Path(directory) / "missing")
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_fifo_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            marker = pathlib.Path(directory) / "schema-rollout.pending"
            os.mkfifo(marker)
            proc = run_guard(marker=marker)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_unreadable_marker_refuses(self):
        with tempfile.TemporaryDirectory() as directory:
            parent = pathlib.Path(directory) / "locked"
            parent.mkdir()
            marker = parent / "schema-rollout.pending"
            marker.write_text("x")
            parent.chmod(0o000)
            try:
                proc = run_guard(marker=marker)
            finally:
                parent.chmod(0o700)
        self.assertEqual(proc.returncode, 1, proc.stderr)

    def test_default_marker_observes_repo_marker_not_cwd(self):
        # The default marker is bound to __file__'s repo path, never CWD: a run
        # from an unrelated directory must still observe the real repo marker.
        expected = 1 if os.path.lexists(REPO_MARKER) else 0
        with tempfile.TemporaryDirectory() as directory:
            proc = run_guard(cwd=directory)
        self.assertEqual(proc.returncode, expected, proc.stderr)


if __name__ == "__main__":
    unittest.main()
