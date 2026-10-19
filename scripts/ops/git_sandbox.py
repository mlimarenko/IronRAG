"""Create throwaway git repositories without touching the real one.

A test that builds a temporary repository is running inside somebody else's
git environment more often than it looks. Git exports `GIT_DIR` (and friends)
to every hook it runs, so a self-test invoked from `pre-push` inherits a
pointer to the developer's actual repository. `git init` then does not create
the temporary repository at all: it re-initialises the inherited `GIT_DIR`,
and because no work tree is named it writes `core.bare = true` into the real
config. Every checkout and every worktree of that repository immediately stops
working with "fatal: this operation must be run in a work tree", and the
damage outlives the test run.

This has now happened twice, in two different scripts, so the environment is
scrubbed in one place instead of being remembered per call site.
"""

from __future__ import annotations

import os
from pathlib import Path
import subprocess

# The variables through which an ambient repository reaches a child git
# process. `lint_migrations.sh` unsets exactly this set for the same reason;
# keep the two lists identical so there is one answer to "what is git state".
INHERITED_GIT_VARIABLES = (
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
)


def sandbox_environment() -> dict[str, str]:
    """The current environment with every pointer to an ambient repository removed."""
    environment = dict(os.environ)
    for name in INHERITED_GIT_VARIABLES:
        environment.pop(name, None)
    return environment


def run_git(arguments: list[str], *, cwd: Path) -> subprocess.CompletedProcess[bytes]:
    """Run one git command against `cwd` and nothing else."""
    return subprocess.run(
        [
            "git",
            # A throwaway repository must not run hooks at all, ambient or
            # global. `sandbox_environment()` only strips the GIT_DIR-family
            # pointers; a global `core.hooksPath` (e.g. a workstation-wide
            # commit-msg policy hook) is ordinary config and survives that,
            # so a freshly `git init`-ed sandbox still inherits it. A
            # hooksPath that does not exist is git's documented way to run
            # with no hooks at all.
            "-c", f"core.hooksPath={cwd / '.no-hooks'}",
            *arguments,
        ],
        cwd=cwd,
        check=True,
        env=sandbox_environment(),
    )


def init_repository(root: Path) -> None:
    """Turn an empty directory into a git repository that owns itself."""
    run_git(["init", "--quiet"], cwd=root)
