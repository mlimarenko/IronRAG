"""Shared fixtures: synthetic local git repositories on disk."""

from __future__ import annotations

import os
import subprocess
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any


@dataclass
class LocalRepo:
    path: Path

    def run(self, *args: str, env: dict[str, str] | None = None) -> str:
        full_env = os.environ.copy()
        # Deterministic identity so tests don't pick up the host user.
        full_env.update(
            {
                "GIT_AUTHOR_NAME": "test",
                "GIT_AUTHOR_EMAIL": "test@example.invalid",
                "GIT_COMMITTER_NAME": "test",
                "GIT_COMMITTER_EMAIL": "test@example.invalid",
                "GIT_CONFIG_GLOBAL": "/dev/null",
                "GIT_CONFIG_SYSTEM": "/dev/null",
            }
        )
        if env:
            full_env.update(env)
        result = subprocess.run(
            ["git", *args],
            cwd=str(self.path),
            check=True,
            text=True,
            capture_output=True,
            env=full_env,
        )
        return result.stdout

    def write(self, rel: str, content: str | bytes) -> None:
        target = self.path / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        if isinstance(content, str):
            target.write_text(content, encoding="utf-8")
        else:
            target.write_bytes(content)

    def remove(self, rel: str) -> None:
        (self.path / rel).unlink()

    def commit(self, message: str) -> None:
        self.run("add", "-A")
        self.run("commit", "-m", message, "--allow-empty")

    def switch(self, branch: str, *, create: bool = False) -> None:
        if create:
            self.run("checkout", "-b", branch)
        else:
            self.run("checkout", branch)

    def head_sha(self, branch: str | None = None) -> str:
        ref = branch or "HEAD"
        return self.run("rev-parse", ref).strip()


def _init_repo(path: Path, branch: str = "main") -> LocalRepo:
    path.mkdir(parents=True, exist_ok=True)
    repo = LocalRepo(path)
    repo.run("init", "-b", branch)
    return repo


def make_settings(
    *,
    state_root: Path,
    repos_yaml: Path,
    base_url: str = "http://ironrag.invalid",
    token: str = "test-token",
) -> Any:
    from ironrag_connector.sources.gitrepos.config import GitReposSettings

    return GitReposSettings(
        ironrag_base_url=base_url,
        ironrag_api_token=token,
        request_timeout_seconds=60.0,
        admin_bearer_token="admin-test",
        gitrepos_config_path=repos_yaml,
        gitrepos_clone_root=state_root / "repo-cache",
        state_db_path=state_root / "state.sqlite",
        sync_run_on_startup=False,
    )


def write_repos_yaml(target: Path, repos: Iterable[dict[str, Any]]) -> Path:
    import yaml

    target.write_text(yaml.safe_dump({"repos": list(repos)}), encoding="utf-8")
    return target
