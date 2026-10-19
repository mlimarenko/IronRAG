"""Repository tracking configuration for the git connector.

Operators describe what to track in ``repos.yaml`` (path configurable
via the ``GITREPOS_CONFIG_PATH`` env var). Schema::

    repos:
      - name: my-repo                 # unique slug; becomes part of external_key
        url: git@github.com:org/repo.git
        # OR
        local_path: /srv/checkouts/my-repo
        branches: [main, develop]     # at least one
        include:
          - "**/*.md"
          - "**/*.py"
        exclude:
          - ".git/**"
          - "node_modules/**"
        facts:                        # arbitrary key/value pairs surfaced
          project: alpha              # as routing_facts on every emitted ref
          tier: production

Either ``url`` or ``local_path`` must be set, never both. ``url`` is
fetched into a bare clone under the connector's state directory;
``local_path`` is read directly without cloning (handy for in-place
testing or repos that already live on the host).

Include / exclude use fnmatch glob syntax with ``**`` matching any
path depth. The defaults are sane: include everything, exclude
``.git/**``, lock files, and common binary build outputs.
"""

from __future__ import annotations

import fnmatch
from collections.abc import Iterable
from pathlib import Path
from typing import Any

import yaml
from pydantic import BaseModel, Field, field_validator, model_validator

DEFAULT_EXCLUDES: tuple[str, ...] = (
    ".git/**",
    "**/.git/**",
    "**/node_modules/**",
    "**/__pycache__/**",
    "**/.venv/**",
    "**/target/debug/**",
    "**/target/release/**",
    "**/dist/**",
    "**/build/**",
    "**/.pytest_cache/**",
    "**/.mypy_cache/**",
    "**/.ruff_cache/**",
)

DEFAULT_MAX_FILE_BYTES: int = 1_048_576  # 1 MiB


class RepoSpec(BaseModel):
    """One tracked repository (potentially many branches)."""

    name: str = Field(min_length=1, max_length=128)
    url: str | None = None
    local_path: Path | None = None
    branches: list[str] = Field(min_length=1)
    include: list[str] = Field(default_factory=list)
    exclude: list[str] = Field(default_factory=list)
    facts: dict[str, Any] = Field(default_factory=dict)
    max_file_bytes: int = Field(default=DEFAULT_MAX_FILE_BYTES, ge=1)

    @field_validator("name")
    @classmethod
    def _validate_name(cls, v: str) -> str:
        if not all(c.isalnum() or c in "-_." for c in v):
            raise ValueError(f"repo name {v!r} may only contain alphanumerics, '-', '_', '.'")
        return v

    @field_validator("branches")
    @classmethod
    def _validate_branches(cls, v: list[str]) -> list[str]:
        for branch in v:
            if not branch or branch.startswith("-"):
                raise ValueError(f"invalid branch name: {branch!r}")
        return v

    @model_validator(mode="after")
    def _validate_source(self) -> RepoSpec:
        if bool(self.url) == bool(self.local_path):
            raise ValueError(f"repo {self.name!r} must set exactly one of `url` or `local_path`")
        if self.local_path is not None:
            self.local_path = self.local_path.expanduser()
        return self

    def effective_excludes(self) -> tuple[str, ...]:
        return tuple(self.exclude) + DEFAULT_EXCLUDES


class ReposConfig(BaseModel):
    repos: list[RepoSpec] = Field(default_factory=list)

    @field_validator("repos")
    @classmethod
    def _unique_names(cls, v: list[RepoSpec]) -> list[RepoSpec]:
        seen: set[str] = set()
        for spec in v:
            if spec.name in seen:
                raise ValueError(f"duplicate repo name {spec.name!r}")
            seen.add(spec.name)
        return v


class ReposConfigError(RuntimeError):
    """Thrown when repos.yaml is missing or malformed."""


def load_repos_config(path: Path) -> ReposConfig:
    if not path.is_file():
        raise ReposConfigError(f"repos config not found at {path}")
    raw = yaml.safe_load(path.read_text(encoding="utf-8")) or {}
    if not isinstance(raw, dict):
        raise ReposConfigError(f"repos config at {path} must be a YAML mapping")
    try:
        return ReposConfig.model_validate(raw)
    except Exception as exc:
        raise ReposConfigError(f"invalid repos config at {path}: {exc}") from exc


def path_matches(path: str, patterns: Iterable[str]) -> bool:
    """fnmatch with `**` semantics across path separators."""
    return any(_glob_match(pat, path) for pat in patterns)


def _glob_match(pattern: str, path: str) -> bool:
    """Glob matcher with segment-aware ``*`` and depth-spanning ``**``.

    Single ``*`` matches inside one path segment only — ``src/*.py`` does
    not match ``src/nested/foo.py``. Use ``**`` to span depth. This
    matches gitignore-style globs and avoids the common fnmatch trap
    where ``*`` greedily eats slashes.
    """
    return _match_segments(pattern.split("/"), path.split("/"))


def _match_segments(pat: list[str], path: list[str]) -> bool:
    if not pat:
        return not path
    head, *rest = pat
    if head == "**":
        if not rest:
            return True
        return any(_match_segments(rest, path[i:]) for i in range(len(path) + 1))
    if not path:
        return False
    if fnmatch.fnmatchcase(path[0], head):
        return _match_segments(rest, path[1:])
    return False


def should_include(path: str, include: Iterable[str], exclude: Iterable[str]) -> bool:
    excluded = path_matches(path, exclude)
    if excluded:
        return False
    include_patterns = list(include)
    if not include_patterns:
        return True
    return path_matches(path, include_patterns)
