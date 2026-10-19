from __future__ import annotations

from pathlib import Path

import pytest
import yaml

from ironrag_connector.sources.gitrepos.repos import (
    DEFAULT_EXCLUDES,
    ReposConfigError,
    load_repos_config,
    path_matches,
    should_include,
)


def _write(path: Path, payload: dict) -> Path:
    path.write_text(yaml.safe_dump(payload), encoding="utf-8")
    return path


def test_load_repos_config_minimal(tmp_path: Path) -> None:
    cfg = _write(
        tmp_path / "repos.yaml",
        {"repos": [{"name": "alpha", "local_path": str(tmp_path), "branches": ["main"]}]},
    )
    parsed = load_repos_config(cfg)
    assert len(parsed.repos) == 1
    assert parsed.repos[0].name == "alpha"
    assert parsed.repos[0].branches == ["main"]


def test_load_repos_config_rejects_both_url_and_local_path(tmp_path: Path) -> None:
    cfg = _write(
        tmp_path / "repos.yaml",
        {
            "repos": [
                {
                    "name": "alpha",
                    "url": "git@example.com:org/a.git",
                    "local_path": "/tmp/a",
                    "branches": ["main"],
                }
            ]
        },
    )
    with pytest.raises(ReposConfigError):
        load_repos_config(cfg)


def test_load_repos_config_rejects_neither_url_nor_local_path(tmp_path: Path) -> None:
    cfg = _write(
        tmp_path / "repos.yaml",
        {"repos": [{"name": "alpha", "branches": ["main"]}]},
    )
    with pytest.raises(ReposConfigError):
        load_repos_config(cfg)


def test_load_repos_config_rejects_duplicate_names(tmp_path: Path) -> None:
    cfg = _write(
        tmp_path / "repos.yaml",
        {
            "repos": [
                {"name": "alpha", "local_path": str(tmp_path), "branches": ["main"]},
                {"name": "alpha", "local_path": str(tmp_path), "branches": ["dev"]},
            ]
        },
    )
    with pytest.raises(ReposConfigError):
        load_repos_config(cfg)


def test_load_repos_config_rejects_empty_branches(tmp_path: Path) -> None:
    cfg = _write(
        tmp_path / "repos.yaml",
        {"repos": [{"name": "alpha", "local_path": str(tmp_path), "branches": []}]},
    )
    with pytest.raises(ReposConfigError):
        load_repos_config(cfg)


def test_load_repos_config_rejects_invalid_name(tmp_path: Path) -> None:
    cfg = _write(
        tmp_path / "repos.yaml",
        {"repos": [{"name": "has space", "local_path": str(tmp_path), "branches": ["main"]}]},
    )
    with pytest.raises(ReposConfigError):
        load_repos_config(cfg)


def test_load_repos_config_missing_file(tmp_path: Path) -> None:
    with pytest.raises(ReposConfigError):
        load_repos_config(tmp_path / "missing.yaml")


def test_glob_double_star_matches_any_depth() -> None:
    assert path_matches("a/b/c/d.md", ["**/*.md"])
    assert path_matches("a.md", ["**/*.md"])
    assert path_matches("node_modules/foo/bar.js", ["**/node_modules/**"])
    assert path_matches("node_modules/foo/bar.js", ["node_modules/**"])
    assert not path_matches("src/foo.py", ["**/*.md"])


def test_glob_simple_patterns() -> None:
    assert path_matches("src/foo.py", ["src/*.py"])
    assert not path_matches("src/nested/foo.py", ["src/*.py"])
    assert path_matches("src/nested/foo.py", ["src/**/*.py"])


def test_should_include_with_no_include_means_all() -> None:
    assert should_include("foo.txt", [], [])
    assert should_include("a/b/c.txt", [], [])


def test_should_include_exclude_wins() -> None:
    assert not should_include("a.md", ["**/*.md"], ["a.md"])


def test_should_include_default_excludes_filter_git_internals() -> None:
    # Sanity-check: our default excludes catch typical noise paths.
    assert path_matches(".git/HEAD", DEFAULT_EXCLUDES)
    assert path_matches("apps/web/node_modules/foo/bar.js", DEFAULT_EXCLUDES)
    assert path_matches("crates/api/target/debug/foo", DEFAULT_EXCLUDES)
    assert not path_matches("src/foo.py", DEFAULT_EXCLUDES)
