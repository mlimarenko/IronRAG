"""No self-test may write into a repository it merely inherited.

Git hands every hook a `GIT_DIR` pointing at the real repository. A self-test
that builds a throwaway repository therefore runs with a loaded gun: `git init`
re-initialises the inherited directory instead of the temporary one.

What makes it destructive is where the hook ran from. In a linked worktree the
inherited path is `<main>/.git/worktrees/<name>`, which does not end in `.git`,
so git concludes it is being asked to create a bare repository and stamps
`core.bare = true` into the config -- and that config is SHARED with the main
checkout and every other worktree. All of them then fail with "fatal: this
operation must be run in a work tree", long after the test run is over. The
same call from an ordinary checkout is harmless, which is precisely why this
survived: it only fires for the people running several worktrees at once.

That is not hypothetical: it has happened twice, in two different scripts, and
each time it surfaced as an unrelated-looking git failure rather than as a
failing test. So this case checks the property itself rather than trusting each
author to remember `git_sandbox` -- it points a decoy `GIT_DIR` at a repository
of its own and demands that the whole self-test suite leave it untouched.

The child run's exit status is deliberately NOT asserted: with a decoy `GIT_DIR`
in the environment, cases that legitimately inspect the real repository are
expected to misbehave. What must hold is only that nothing wrote to the decoy.
The collected-test count IS asserted, because a child that failed to start
would otherwise leave the decoy pristine and turn this gate green for the one
reason that means nothing.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
import re
import subprocess
import sys
from tempfile import TemporaryDirectory
import unittest


OPS_DIRECTORY = Path(__file__).parent
# Set for the inner run so the suite does not invoke this case recursively.
CHILD_MARKER = "IRONRAG_OPS_SELFTEST_CHILD"
RAN_TESTS = re.compile(r"^Ran (\d+) tests?", re.MULTILINE)


def _load(module_name: str):
    path = OPS_DIRECTORY / f"{module_name}.py"
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {module_name} module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GIT_SANDBOX = _load("git_sandbox")


@unittest.skipIf(
    os.environ.get(CHILD_MARKER) == "1",
    "inner run: the outer case is the gate",
)
class AmbientGitStateTests(unittest.TestCase):
    def test_self_tests_leave_an_inherited_repository_untouched(self) -> None:
        with TemporaryDirectory(prefix="ironrag-ops-decoy-") as directory:
            decoy = Path(directory)
            checkout = decoy / "checkout"
            checkout.mkdir()
            GIT_SANDBOX.init_repository(checkout)
            GIT_SANDBOX.run_git(
                ["-c", "user.email=decoy@invalid", "-c", "user.name=decoy",
                 "commit", "--quiet", "--allow-empty", "--message", "decoy"],
                cwd=checkout,
            )
            # The decoy must carry a LINKED WORKTREE, because that is the only
            # shape that is destructive: its git dir does not end in `.git`, so
            # an inherited `git init` declares the shared config bare. A plain
            # checkout survives the same call untouched and would make this
            # case permanently green.
            GIT_SANDBOX.run_git(
                ["worktree", "add", "--quiet", str(decoy / "linked"), "-b", "decoy-branch"],
                cwd=checkout,
            )
            config_path = checkout / ".git" / "config"
            before = config_path.read_text(encoding="utf-8")

            environment = dict(os.environ)
            environment[CHILD_MARKER] = "1"
            for name in GIT_SANDBOX.INHERITED_GIT_VARIABLES:
                environment.pop(name, None)
            environment["GIT_DIR"] = str(checkout / ".git" / "worktrees" / "linked")

            completed = subprocess.run(
                [
                    sys.executable,
                    "-m",
                    "unittest",
                    "discover",
                    "--start-directory",
                    str(OPS_DIRECTORY),
                    "--top-level-directory",
                    str(OPS_DIRECTORY),
                    "--pattern",
                    "test_*.py",
                ],
                env=environment,
                capture_output=True,
                text=True,
                cwd=OPS_DIRECTORY.parent.parent,
            )

            collected = RAN_TESTS.search(completed.stderr)
            self.assertIsNotNone(
                collected,
                "the inner run never reported a test count, so this case proved "
                f"nothing:\n{completed.stderr[-2000:]}",
            )
            assert collected is not None
            self.assertGreater(
                int(collected.group(1)),
                0,
                "the inner run collected no tests, so an untouched decoy means nothing",
            )

            self.assertEqual(
                config_path.read_text(encoding="utf-8"),
                before,
                "a self-test wrote into the repository named by GIT_DIR. Build "
                "throwaway repositories through scripts/ops/git_sandbox.py, which "
                "strips the inherited git environment.",
            )
