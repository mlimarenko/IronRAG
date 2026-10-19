"""End-to-end adapter tests against real local git repositories.

These tests do not touch the network and do not need IronRAG running:
they exercise the adapter's iter_items + fetch against repos created
in tmp dirs, which is the slice of the connector that actually
interacts with git.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Protocol

import pytest
from test_gitrepos_support import LocalRepo, make_settings, write_repos_yaml

from ironrag_connector.source import (
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceItemRef,
)
from ironrag_connector.sources.gitrepos.adapter import GitReposAdapter
from ironrag_connector.sources.gitrepos.git_client import (
    GitCommandError,
    GitTreeParseError,
    TreeEntry,
)
from ironrag_connector.sources.gitrepos.mapping import (
    KIND_FILE,
    file_external_key,
    parse_file_item_id,
)
from ironrag_connector.sources.gitrepos.repos import ReposConfig

pytestmark = pytest.mark.asyncio


class TmpRepoFactory(Protocol):
    def __call__(self, name: str, *, branch: str = "main") -> LocalRepo: ...


async def _collect(adapter: GitReposAdapter) -> list[SourceItemRef]:
    return [ref async for ref in adapter.iter_items()]


async def test_missing_enforcement_is_explicit_for_every_declared_kind(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
) -> None:
    repo = tmp_repo_factory("missing-policy")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "missing-policy", "local_path": str(repo.path), "branches": ["main"]}],
    )
    adapter = GitReposAdapter(make_settings(state_root=state_root, repos_yaml=repos_yaml))

    assert {kind: adapter.missing_enforcement(kind) for kind in adapter.kinds} == {
        KIND_FILE: SourceDeleteEnforcement.POLICY
    }
    with pytest.raises(ValueError, match="unsupported gitrepos item kind: future-kind"):
        adapter.missing_enforcement("future-kind")


async def test_iter_local_repo_single_branch(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("alpha")
    repo.write("README.md", "# alpha\n")
    repo.write("src/main.py", "print('hello')\n")
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "alpha",
                "local_path": str(repo.path),
                "branches": ["main"],
                "include": ["**/*.md", "**/*.py"],
                "facts": {"project": "alpha"},
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    paths = sorted(r.routing_facts["path"] for r in refs)
    assert paths == ["README.md", "src/main.py"]
    for ref in refs:
        assert ref.kind == KIND_FILE
        assert ref.routing_facts["repo"] == "alpha"
        assert ref.routing_facts["branch"] == "main"
        assert ref.routing_facts["project"] == "alpha"
        assert ref.change_token  # blob sha must be present


async def test_multibranch_emits_one_ref_per_branch(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("multi")
    repo.write("shared.md", "common")
    repo.commit("init")
    repo.switch("feature", create=True)
    repo.write("feature-only.md", "feature")
    repo.commit("feature branch addition")
    repo.switch("main")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "multi",
                "local_path": str(repo.path),
                "branches": ["main", "feature"],
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    by_branch: dict[str, set[str]] = {}
    for ref in refs:
        by_branch.setdefault(ref.routing_facts["branch"], set()).add(ref.routing_facts["path"])
    assert by_branch == {
        "main": {"shared.md"},
        "feature": {"shared.md", "feature-only.md"},
    }


async def test_include_exclude_filters(tmp_repo_factory: TmpRepoFactory, state_root: Path) -> None:
    repo = tmp_repo_factory("filtered")
    repo.write("docs/intro.md", "intro")
    repo.write("docs/drafts/notes.md", "drafts")
    repo.write("src/server.py", "py")
    repo.write("dist/bundle.js", "compiled")
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "filtered",
                "local_path": str(repo.path),
                "branches": ["main"],
                "include": ["docs/**/*.md", "src/**/*.py"],
                "exclude": ["docs/drafts/**"],
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    paths = sorted(r.routing_facts["path"] for r in refs)
    # docs/drafts is excluded; dist/* never matched include; .git/* is in DEFAULT_EXCLUDES.
    assert paths == ["docs/intro.md", "src/server.py"]


async def test_repos_yaml_rules_reload_between_sweeps(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("reloadable")
    repo.write("docs/intro.md", "intro")
    repo.write("docs/drafts/notes.md", "drafts")
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "reloadable",
                "local_path": str(repo.path),
                "branches": ["main"],
                "include": ["docs/**/*.md"],
                "exclude": ["docs/drafts/**"],
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs_before = await _collect(adapter)
    assert sorted(r.routing_facts["path"] for r in refs_before) == ["docs/intro.md"]

    write_repos_yaml(
        repos_yaml,
        [
            {
                "name": "reloadable",
                "local_path": str(repo.path),
                "branches": ["main"],
                "include": ["docs/**/*.md"],
            }
        ],
    )
    next_mtime_ns = repos_yaml.stat().st_mtime_ns + 1_000_000
    os.utime(repos_yaml, ns=(next_mtime_ns, next_mtime_ns))

    refs_after = await _collect(adapter)
    assert sorted(r.routing_facts["path"] for r in refs_after) == [
        "docs/drafts/notes.md",
        "docs/intro.md",
    ]


async def test_same_token_repo_reappearance_after_config_race_is_retried(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
) -> None:
    repo = tmp_repo_factory("config-race")
    repo.write("README.md", "stable bytes")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "config-race", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    live_adapter = GitReposAdapter(settings)
    ref = (await _collect(live_adapter))[0]

    missing_adapter = GitReposAdapter(settings, repos_config=ReposConfig())
    missing = await missing_adapter.fetch(ref)
    assert isinstance(missing, SourceFetchDelete)
    assert missing.cacheable is False

    reappeared = await live_adapter.fetch(ref)
    assert isinstance(reappeared, SourceFetchMaterialized)
    assert reappeared.item.ref.change_token == ref.change_token


async def test_fetch_returns_blob_payload(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("payload")
    repo.write("hello.md", "# Hi\nThe quick brown fox.")
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "payload",
                "local_path": str(repo.path),
                "branches": ["main"],
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    ref = next(r for r in refs if r.routing_facts["path"] == "hello.md")
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchMaterialized)
    item = result.item
    assert item.payload == b"# Hi\nThe quick brown fox."
    assert item.mime_type == "text/markdown"
    assert item.file_name == "hello.md"
    assert item.title == "payload:main — hello.md"
    assert item.document_hint == "payload:main:hello.md"


async def test_change_token_advances_on_content_change(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("evolving")
    repo.write("doc.md", "v1")
    repo.commit("v1")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "evolving",
                "local_path": str(repo.path),
                "branches": ["main"],
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs_v1 = await _collect(adapter)
    blob_v1 = refs_v1[0].change_token
    assert blob_v1

    # Empty commit must NOT change the blob sha — same content, same blob.
    repo.commit("touch")
    refs_after_empty = await _collect(GitReposAdapter(settings))
    assert refs_after_empty[0].change_token == blob_v1

    # Real content change must change the blob sha.
    repo.write("doc.md", "v2 — new content")
    repo.commit("update")
    refs_v2 = await _collect(GitReposAdapter(settings))
    assert refs_v2[0].change_token != blob_v1


async def test_max_file_bytes_skips_large_files(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("sized")
    repo.write("small.md", "ok")
    repo.write("big.bin", b"x" * 4096)
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "sized",
                "local_path": str(repo.path),
                "branches": ["main"],
                "max_file_bytes": 1024,
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)
    refs = await _collect(adapter)
    paths = sorted(r.routing_facts["path"] for r in refs)
    assert paths == ["small.md"]


async def test_routing_facts_payload(tmp_repo_factory: TmpRepoFactory, state_root: Path) -> None:
    repo = tmp_repo_factory("facts-rich")
    repo.write("apps/web/main.tsx", "x")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "facts-rich",
                "local_path": str(repo.path),
                "branches": ["main"],
                "facts": {"team": "frontend", "tier": "production"},
            }
        ],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)
    refs = await _collect(adapter)
    assert len(refs) == 1
    facts = refs[0].routing_facts
    assert facts["repo"] == "facts-rich"
    assert facts["branch"] == "main"
    assert facts["path"] == "apps/web/main.tsx"
    assert facts["dir"] == "apps/web"
    assert facts["ext"] == "tsx"
    assert facts["top_dir"] == "apps"
    assert facts["team"] == "frontend"
    assert facts["tier"] == "production"
    # repo_url omitted for local-path repos
    assert "repo_url" not in facts


async def test_binary_files_are_skipped(tmp_repo_factory: TmpRepoFactory, state_root: Path) -> None:
    repo = tmp_repo_factory("withbin")
    repo.write("readme.md", "# text\n")
    # PNG-ish header followed by a NUL byte -> classified binary by content.
    repo.write("logo.png", b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDRbinary")
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "withbin", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    # Both files are tracked, so both are enumerated...
    paths = sorted(r.routing_facts["path"] for r in refs)
    assert paths == ["logo.png", "readme.md"]
    # ...but the binary blob is dropped at fetch time, the text file is kept.
    text_ref = next(r for r in refs if r.routing_facts["path"] == "readme.md")
    bin_ref = next(r for r in refs if r.routing_facts["path"] == "logo.png")
    assert isinstance(await adapter.fetch(text_ref), SourceFetchMaterialized)
    binary_result = await adapter.fetch(bin_ref)
    assert isinstance(binary_result, SourceFetchDelete)
    assert binary_result.enforcement is SourceDeleteEnforcement.REQUIRED
    assert binary_result.cacheable is True


async def test_text_file_becoming_binary_requests_required_removal(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
) -> None:
    repo = tmp_repo_factory("text-to-binary")
    repo.write("artifact.dat", "readable text\n")
    repo.commit("text")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "text-to-binary", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)

    text_adapter = GitReposAdapter(settings)
    text_ref = (await _collect(text_adapter))[0]
    assert isinstance(await text_adapter.fetch(text_ref), SourceFetchMaterialized)

    repo.write("artifact.dat", b"readable prefix\x00binary payload")
    repo.commit("binary")
    binary_adapter = GitReposAdapter(settings)
    binary_ref = (await _collect(binary_adapter))[0]
    result = await binary_adapter.fetch(binary_ref)

    assert binary_ref.item_id == text_ref.item_id
    assert binary_ref.change_token != text_ref.change_token
    assert isinstance(result, SourceFetchDelete)
    assert result.enforcement is SourceDeleteEnforcement.REQUIRED
    assert result.cacheable is True


async def test_transient_blob_read_failure_is_deferred(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo = tmp_repo_factory("temporarily-unreadable")
    repo.write("readme.md", "# text\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "temporarily-unreadable",
                "local_path": str(repo.path),
                "branches": ["main"],
            }
        ],
    )
    adapter = GitReposAdapter(make_settings(state_root=state_root, repos_yaml=repos_yaml))
    ref = (await _collect(adapter))[0]

    async def fail_read_blob(*_: object) -> bytes:
        raise GitCommandError(["git", "cat-file"], 1, "synthetic transient failure")

    monkeypatch.setattr(adapter._client, "read_blob", fail_read_blob)

    result = await adapter.fetch(ref)

    assert isinstance(result, SourceFetchDeferred)
    assert "read" in result.reason


async def test_non_utf8_text_is_ingested(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    """A legacy 8-bit encoded text file (no NUL byte) is text, not binary."""
    repo = tmp_repo_factory("legacy")
    payload = "café déjà vu".encode("latin-1")  # not valid utf-8, but no NUL
    repo.write("notes.txt", payload)
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "legacy", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    result = await adapter.fetch(refs[0])
    assert isinstance(result, SourceFetchMaterialized)
    item = result.item
    assert item.payload == payload
    assert item.mime_type == "text/plain"


async def test_gitignore_excludes_untracked_files(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    """.gitignore is obeyed for free: git does not track ignored files, so
    ls-tree never lists them. Force-added files would be tracked and thus
    visible — which is correct (they are part of the repo)."""
    repo = tmp_repo_factory("ignoring")
    repo.write(".gitignore", "*.log\nsecrets/\n")
    repo.write("keep.md", "tracked content")
    repo.write("debug.log", "ignored noise")  # matched by *.log
    repo.write("secrets/key.txt", "ignored dir")  # matched by secrets/
    repo.commit("init")  # `git add -A` skips ignored paths

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "ignoring", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)

    refs = await _collect(adapter)
    paths = sorted(r.routing_facts["path"] for r in refs)
    assert "keep.md" in paths
    assert ".gitignore" in paths  # itself tracked
    assert "debug.log" not in paths
    assert "secrets/key.txt" not in paths


async def test_deleted_file_no_longer_emitted(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    """A committed deletion drops the file from the sweep set; the framework
    reaper then deletes the orphaned IronRAG document."""
    repo = tmp_repo_factory("deleting")
    repo.write("a.md", "alpha")
    repo.write("b.md", "bravo")
    repo.commit("init")

    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "deleting", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)

    refs_before = await _collect(GitReposAdapter(settings))
    assert sorted(r.routing_facts["path"] for r in refs_before) == ["a.md", "b.md"]

    repo.remove("b.md")
    repo.commit("remove b")

    refs_after = await _collect(GitReposAdapter(settings))
    paths_after = sorted(r.routing_facts["path"] for r in refs_after)
    assert paths_after == ["a.md"]


async def test_external_key_round_trip_through_adapter(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("rt")
    repo.write("nested/path/file.txt", "hi")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "rt", "local_path": str(repo.path), "branches": ["main"]}],
    )
    settings = make_settings(state_root=state_root, repos_yaml=repos_yaml)
    adapter = GitReposAdapter(settings)
    refs = await _collect(adapter)
    [ref] = refs
    expected_key = file_external_key("rt", "main", "nested/path/file.txt")
    assert ref.external_key == expected_key
    parsed = adapter.parse_external_key(ref.external_key)
    assert parsed is not None
    kind, item_id = parsed
    assert kind == KIND_FILE
    assert parse_file_item_id(item_id) == ("rt", "main", "nested/path/file.txt")


@pytest.mark.parametrize(
    "record",
    [
        pytest.param(
            b"100644 blob 0123456789abcdef0123456789abcdef01234567 12 no-tab.md", id="no-tab"
        ),
        pytest.param(
            b"100644 blob 0123456789abcdef0123456789abcdef01234567\tshort.md", id="few-fields"
        ),
        pytest.param(
            b"100644 blob 0123456789abcdef0123456789abcdef01234567 -\tdash.md", id="dash-size"
        ),
        pytest.param(
            b"100644 blob 0123456789abcdef0123456789abcdef01234567 many\tbad.md", id="nan-size"
        ),
        pytest.param(b"100644 blob nothex 12\tsha.md", id="bad-sha"),
        pytest.param(
            b"100644 blob 0123456789abcdef0123456789abcdef01234567 12 extra\tx.md", id="extra-field"
        ),
        pytest.param(
            b"100644 tag 0123456789abcdef0123456789abcdef01234567 12\tx.md", id="unknown-type"
        ),
        pytest.param(b"100644 blob 0123456789abcdef0123456789abcdef01234567 12\t", id="empty-path"),
    ],
)
async def test_list_tree_rejects_malformed_records(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
    monkeypatch: pytest.MonkeyPatch,
    record: bytes,
) -> None:
    repo = tmp_repo_factory("malformed")
    repo.write("ok.md", "ok\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "malformed", "local_path": str(repo.path), "branches": ["main"]}],
    )
    adapter = GitReposAdapter(make_settings(state_root=state_root, repos_yaml=repos_yaml))
    client = adapter._client
    original_run = client._run

    async def corrupted_run(cwd: Path, args: list[str], **kwargs: object) -> bytes:
        out = await original_run(cwd, args, **kwargs)
        return out + record + b"\x00" if args[0] == "ls-tree" else out

    monkeypatch.setattr(client, "_run", corrupted_run)

    spec = adapter._spec_by_name("malformed")
    with pytest.raises(GitTreeParseError):
        await client.list_tree(spec, "main")


async def test_list_tree_skips_submodule_entries(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo = tmp_repo_factory("with-submodule")
    repo.write("ok.md", "ok\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "with-submodule", "local_path": str(repo.path), "branches": ["main"]}],
    )
    adapter = GitReposAdapter(make_settings(state_root=state_root, repos_yaml=repos_yaml))
    client = adapter._client
    original_run = client._run

    async def run_with_submodule(cwd: Path, args: list[str], **kwargs: object) -> bytes:
        out = await original_run(cwd, args, **kwargs)
        gitlink = b"160000 commit 0123456789abcdef0123456789abcdef01234567 -\tvendor/lib\x00"
        return out + gitlink if args[0] == "ls-tree" else out

    monkeypatch.setattr(client, "_run", run_with_submodule)

    entries = await client.list_tree(adapter._spec_by_name("with-submodule"), "main")

    assert [entry.path for entry in entries] == ["ok.md"]


async def test_list_tree_accepts_sha256_object_names(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    repo = tmp_repo_factory("sha256")
    repo.write("ok.md", "ok\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "sha256", "local_path": str(repo.path), "branches": ["main"]}],
    )
    adapter = GitReposAdapter(make_settings(state_root=state_root, repos_yaml=repos_yaml))
    client = adapter._client
    sha256 = "ab" * 32

    async def sha256_run(cwd: Path, args: list[str], **kwargs: object) -> bytes:
        assert args[0] == "ls-tree"
        return f"100644 blob {sha256}       3\twide.md\x00".encode()

    monkeypatch.setattr(client, "_run", sha256_run)

    entries = await client.list_tree(adapter._spec_by_name("sha256"), "main")

    assert entries == [TreeEntry(path="wide.md", blob_sha=sha256, size=3)]
