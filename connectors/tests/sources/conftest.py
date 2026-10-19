"""Fixtures for the git-repository source tests.

They live in a conftest rather than in `test_gitrepos_support.py` because
pytest only auto-registers fixtures it finds through conftest collection: as a
plain helper module the fixtures resolved to "fixture not found" at setup, which
reads like a broken test rather than a missing registration.
"""

from __future__ import annotations

import shutil
from collections.abc import Callable, Iterator
from pathlib import Path

import pytest
from test_gitrepos_support import LocalRepo, _init_repo


@pytest.fixture
def tmp_repo_factory(tmp_path: Path) -> Iterator[Callable[..., LocalRepo]]:
    created: list[Path] = []

    def factory(name: str, *, branch: str = "main") -> LocalRepo:
        repo_path = tmp_path / name
        repo = _init_repo(repo_path, branch=branch)
        created.append(repo_path)
        return repo

    yield factory

    for p in created:
        shutil.rmtree(p, ignore_errors=True)


@pytest.fixture
def state_root(tmp_path: Path) -> Path:
    root = tmp_path / "state"
    root.mkdir(parents=True, exist_ok=True)
    return root
