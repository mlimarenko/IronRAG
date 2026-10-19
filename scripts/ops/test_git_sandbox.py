"""A sandboxed repository must never run an ambient hook.

`init_repository()` scrubs the `GIT_DIR`-family environment variables so a
freshly created repository does not resolve to somebody else's `.git`
directory (see `git_sandbox.py`). Config is a second, independent leak path:
`git init` also reads machine-wide config (global or system), so a
harness-installed `core.hooksPath` applies to the throwaway repository just
as it would to a real checkout. A decoy commit made for a self-test must not
be silently rejected by a commit-msg hook that has nothing to do with the
sandbox.
"""

from __future__ import annotations

import importlib.util
import os
from pathlib import Path
from tempfile import TemporaryDirectory
import unittest


OPS_DIRECTORY = Path(__file__).parent


def _load(module_name: str):
    path = OPS_DIRECTORY / f"{module_name}.py"
    spec = importlib.util.spec_from_file_location(module_name, path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {module_name} module")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


GIT_SANDBOX = _load("git_sandbox")


class GitSandboxTests(unittest.TestCase):
    def test_init_repository_disables_ambient_hooks_path(self) -> None:
        with TemporaryDirectory(prefix="ironrag-git-sandbox-") as directory:
            root = Path(directory)
            hooks_directory = root / "ambient-hooks"
            hooks_directory.mkdir()
            reject_everything = hooks_directory / "commit-msg"
            reject_everything.write_text("#!/bin/sh\nexit 1\n")
            reject_everything.chmod(0o755)

            global_config = root / "ambient-gitconfig"
            global_config.write_text(
                f"[core]\n\thooksPath = {hooks_directory}\n"
            )

            checkout = root / "checkout"
            checkout.mkdir()

            ambient_environment = dict(os.environ)
            ambient_environment["GIT_CONFIG_GLOBAL"] = str(global_config)
            ambient_environment["GIT_CONFIG_NOSYSTEM"] = "1"
            original_environ = os.environ
            os.environ = ambient_environment  # noqa: B003 - scoped restore below
            try:
                GIT_SANDBOX.init_repository(checkout)
                # Would raise CalledProcessError if the ambient commit-msg
                # hook fired, since it unconditionally exits 1.
                GIT_SANDBOX.run_git(
                    ["-c", "user.email=sandbox@invalid", "-c", "user.name=sandbox",
                     "commit", "--quiet", "--allow-empty", "--message", "decoy"],
                    cwd=checkout,
                )
            finally:
                os.environ = original_environ

            log = GIT_SANDBOX.run_git(
                ["log", "--oneline"], cwd=checkout,
            )
            self.assertEqual(log.returncode, 0)


if __name__ == "__main__":
    unittest.main()
