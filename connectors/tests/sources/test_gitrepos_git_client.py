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

from ironrag_connector.sources.gitrepos.git_client import (
    GitClient,
    GitCommandError,
    GitTreeParseError,
)
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


async def test_missing_local_path_fails_before_any_git_read(tmp_path: Path) -> None:
    client = GitClient(tmp_path / "cache")
    spec = RepoSpec(name="gone", local_path=tmp_path / "absent", branches=["main"])

    with pytest.raises(GitCommandError) as excinfo:
        await client.ensure_repo(spec)

    assert "does not exist" in excinfo.value.stderr


async def test_missing_git_binary_is_reported_as_a_failed_read(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """A spawn failure must not escape as a raw OSError."""

    def no_process(*args: object, **kwargs: object) -> object:
        raise FileNotFoundError("git")

    monkeypatch.setattr("asyncio.create_subprocess_exec", no_process)
    client = GitClient(tmp_path / "cache")

    with pytest.raises(GitCommandError) as excinfo:
        await client._run(tmp_path, ["rev-parse", "HEAD"])

    assert excinfo.value.code == -1
    assert "git" in excinfo.value.stderr


async def test_hung_git_process_is_killed_and_reported_as_a_timeout(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    killed: list[bool] = []

    class HangingProcess:
        returncode = None

        async def communicate(self) -> tuple[bytes, bytes]:
            raise TimeoutError

        def kill(self) -> None:
            killed.append(True)

        async def wait(self) -> int:
            return 0

    async def hanging_process(*args: object, **kwargs: object) -> HangingProcess:
        return HangingProcess()

    monkeypatch.setattr("asyncio.create_subprocess_exec", hanging_process)
    client = GitClient(tmp_path / "cache")

    with pytest.raises(GitCommandError) as excinfo:
        await client._run(tmp_path, ["rev-parse", "HEAD"], timeout_seconds=0.01)

    assert excinfo.value.code == -1
    assert excinfo.value.stderr == "timeout"
    assert killed == [True]


async def test_head_sha_propagates_a_non_missing_ref_failure(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Only a missing/ambiguous ref means ``None``; other failures stay loud."""

    async def failing_run(*args: object, **kwargs: object) -> bytes:
        raise GitCommandError(["git", "rev-parse"], 128, "fatal: not a git repository")

    client = GitClient(tmp_path / "cache")
    spec = RepoSpec(name="origin", url="file:///nowhere", branches=["main"])
    monkeypatch.setattr(client, "_run", failing_run)

    with pytest.raises(GitCommandError):
        await client.head_sha(spec, "main")


def _tree_sha() -> bytes:
    return b"0" * 40


async def test_ls_tree_census_skips_non_blob_entries(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    async def listing(*args: object, **kwargs: object) -> bytes:
        return b"040000 tree " + _tree_sha() + b" -\tsubdir\x00"

    client = GitClient(tmp_path / "cache")
    spec = RepoSpec(name="origin", url="file:///nowhere", branches=["main"])
    monkeypatch.setattr(client, "_run", listing)

    assert await client.list_tree(spec, "main") == []


async def test_ls_tree_census_skips_a_non_utf8_path(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """An unrepresentable path is outside the index scope, not a census failure."""

    async def listing(*args: object, **kwargs: object) -> bytes:
        return b"100644 blob " + _tree_sha() + b" 3\t" + b"\xff.txt\x00"

    client = GitClient(tmp_path / "cache")
    spec = RepoSpec(name="origin", url="file:///nowhere", branches=["main"])
    monkeypatch.setattr(client, "_run", listing)

    assert await client.list_tree(spec, "main") == []


@pytest.mark.parametrize(
    ("record", "detail"),
    [
        (b"100644 blob " + b"0" * 40, "no path"),
        (b"100644 blob " + b"0" * 40 + b"\tpath", "expected mode, type, object id and size"),
        (b"100644 xblob " + b"0" * 40 + b" 3\tpath", "unknown mode or object type"),
        (b"100644 blob zzzz 3\tpath", "object id is not a git object name"),
        (b"100644 blob " + b"0" * 40 + b" big\tpath", "blob size is not an integer"),
    ],
)
async def test_ls_tree_census_refuses_an_untrustworthy_record(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, record: bytes, detail: str
) -> None:
    """A dropped record would later read as a deleted file, so it fails loudly."""

    async def listing(*args: object, **kwargs: object) -> bytes:
        return record

    client = GitClient(tmp_path / "cache")
    spec = RepoSpec(name="origin", url="file:///nowhere", branches=["main"])
    monkeypatch.setattr(client, "_run", listing)

    with pytest.raises(GitTreeParseError, match=detail):
        await client.list_tree(spec, "main")
