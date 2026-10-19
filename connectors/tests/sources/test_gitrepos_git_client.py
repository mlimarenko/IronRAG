"""``GitClient`` against a real repository addressed by URL.

``git clone --bare`` keeps every branch in ``refs/heads/*`` and sets no
remote-tracking refspec, so a bare clone must both resolve that namespace
and fetch it explicitly. These tests drive the real ``git`` CLI through the
client with a local ``file://`` origin, so clone, fetch, ``ls-tree`` and
``cat-file`` all run against actual repository state: the assertions are on
returned commit shas, tree listings and blob bytes, not on command strings.
"""

from __future__ import annotations

from pathlib import Path
from typing import Protocol

import pytest
from test_gitrepos_support import LocalRepo

from ironrag_connector.sources.gitrepos.git_client import GitClient
from ironrag_connector.sources.gitrepos.repos import RepoSpec

pytestmark = pytest.mark.asyncio


class TmpRepoFactory(Protocol):
    def __call__(self, name: str, *, branch: str = "main") -> LocalRepo: ...


def _url_spec(name: str, origin: LocalRepo, branches: list[str]) -> RepoSpec:
    return RepoSpec(name=name, url=f"file://{origin.path}", branches=branches)


async def test_url_clone_resolves_default_branch_tree_and_blob(
    tmp_repo_factory: TmpRepoFactory, tmp_path: Path
) -> None:
    origin = tmp_repo_factory("origin")
    origin.write("README.md", "# hi\n")
    origin.write("src/app.py", "print(1)\n")
    origin.commit("init")

    client = GitClient(tmp_path / "cache")
    spec = _url_spec("origin", origin, ["main"])
    await client.ensure_repo(spec)

    # ``main`` is the origin's HEAD branch; resolving it must yield the commit.
    assert await client.head_sha(spec, "main") == origin.head_sha("main")

    entries = {e.path: e for e in await client.list_tree(spec, "main")}
    assert sorted(entries) == ["README.md", "src/app.py"]
    readme = entries["README.md"]
    assert readme.size == len(b"# hi\n")
    assert await client.read_blob(spec, readme.blob_sha) == b"# hi\n"


async def test_url_clone_resolves_explicit_named_branch(
    tmp_repo_factory: TmpRepoFactory, tmp_path: Path
) -> None:
    origin = tmp_repo_factory("named")
    origin.write("shared.md", "common\n")
    origin.commit("init")
    origin.switch("release", create=True)
    origin.write("release-only.md", "release\n")
    origin.commit("release")
    origin.switch("main")

    client = GitClient(tmp_path / "cache")
    spec = _url_spec("named", origin, ["main", "release"])
    await client.ensure_repo(spec)

    assert await client.head_sha(spec, "release") == origin.head_sha("release")
    assert {e.path for e in await client.list_tree(spec, "release")} == {
        "shared.md",
        "release-only.md",
    }


async def test_url_fetch_advances_branch_and_prunes_removed_branch(
    tmp_repo_factory: TmpRepoFactory, tmp_path: Path
) -> None:
    origin = tmp_repo_factory("moving")
    origin.write("a.md", "a\n")
    origin.commit("init")
    origin.switch("stale", create=True)
    origin.write("stale.md", "stale\n")
    origin.commit("stale")
    origin.switch("main")

    client = GitClient(tmp_path / "cache")
    spec = _url_spec("moving", origin, ["main", "stale", "fresh"])
    await client.ensure_repo(spec)
    assert await client.head_sha(spec, "main") == origin.head_sha("main")

    origin.write("b.md", "b\n")
    origin.commit("advance")
    origin.switch("fresh", create=True)
    origin.write("fresh.md", "fresh\n")
    origin.commit("fresh")
    origin.switch("main")
    origin.run("branch", "-D", "stale")

    # Second ensure_repo takes the fetch path and must move ``refs/heads/*``.
    await client.ensure_repo(spec)

    assert await client.head_sha(spec, "main") == origin.head_sha("main")
    assert await client.head_sha(spec, "fresh") == origin.head_sha("fresh")
    assert await client.head_sha(spec, "stale") is None
    assert sorted(e.path for e in await client.list_tree(spec, "main")) == ["a.md", "b.md"]


async def test_url_missing_branch_head_is_none(
    tmp_repo_factory: TmpRepoFactory, tmp_path: Path
) -> None:
    origin = tmp_repo_factory("missing")
    origin.write("a.md", "a\n")
    origin.commit("init")

    client = GitClient(tmp_path / "cache")
    spec = _url_spec("missing", origin, ["main", "nope"])
    await client.ensure_repo(spec)

    assert await client.head_sha(spec, "main") is not None
    assert await client.head_sha(spec, "nope") is None


async def test_local_path_repo_reads_working_tree_without_clone(
    tmp_repo_factory: TmpRepoFactory, tmp_path: Path
) -> None:
    origin = tmp_repo_factory("localrepo")
    origin.write("readme.md", "hello\n")
    origin.commit("init")

    clone_root = tmp_path / "cache"
    client = GitClient(clone_root)
    spec = RepoSpec(name="localrepo", local_path=origin.path, branches=["main"])
    await client.ensure_repo(spec)

    assert not (clone_root / "localrepo.git").exists()
    assert await client.head_sha(spec, "main") == origin.head_sha("main")
    assert [e.path for e in await client.list_tree(spec, "main")] == ["readme.md"]
