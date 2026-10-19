"""Census completeness of the git connector, end to end through the reaper.

Every test seeds a previously materialized document (cursor + location in the
state store and a listing row in the fake IronRAG) and then runs one real
``SyncManager`` sweep with the real ``GitReposAdapter``. The only fake on the
push side is the orchestrator: it records what the reaper asked for instead of
talking to IronRAG, because the decision under test is *whether* the framework
may delete, not the HTTP call that does it.
"""

from __future__ import annotations

import shutil
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Protocol
from uuid import UUID

import pytest
from test_gitrepos_support import LocalRepo, make_settings, write_repos_yaml

from ironrag_connector.ironrag import DocumentResource
from ironrag_connector.orchestrator import OrchestrationOutcome
from ironrag_connector.policy import DeleteAction, PushPolicy
from ironrag_connector.routing import (
    PolicyOverrides,
    ResolvedLibraryTarget,
    Router,
    RoutingConfig,
)
from ironrag_connector.source import SourceItemRef
from ironrag_connector.sources.gitrepos.adapter import GitReposAdapter
from ironrag_connector.sources.gitrepos.mapping import (
    KIND_FILE,
    file_external_key,
    file_item_id,
)
from ironrag_connector.state import StateStore
from ironrag_connector.sync import SyncManager, SyncReport

pytestmark = pytest.mark.asyncio

WS = UUID("00000000-0000-0000-0000-000000000099")
LIB = UUID("00000000-0000-0000-0000-000000000000")


class TmpRepoFactory(Protocol):
    def __call__(self, name: str, *, branch: str = "main") -> LocalRepo: ...


class _ListingIronRag:
    async def ensure_visibility_contract(self) -> None:
        return None

    def __init__(self, documents: tuple[DocumentResource, ...]) -> None:
        self._documents = documents
        self.list_calls: list[tuple[UUID, str | None]] = []

    async def list_documents(self, library_id: UUID, *, search: str | None = None, **_: Any) -> Any:
        self.list_calls.append((library_id, search))
        for document in self._documents:
            yield document

    async def get_document(self, *_: Any, **__: Any) -> DocumentResource | None:
        return None


class _RecordingOrchestrator:
    """Accepts every push, records every reaper decision, deletes nothing."""

    def __init__(self) -> None:
        self.pushed: list[str] = []
        self.reaped: list[str] = []
        self.confirmed_absent: list[str] = []

    def reset_sweep_cache(self, _: dict[str, set[UUID]] | None = None) -> None:
        pass

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        self.pushed.append(ref.item_id)
        return OrchestrationOutcome(
            ref=ref,
            action="noop_unchanged",
            workspace_id=WS,
            library_id=LIB,
            rule_description=None,
            ironrag_document_id=str(UUID(int=1)),
            detail="test push",
        )

    async def reap_orphan(
        self, ref: SourceItemRef, library_id: UUID, document_id: str, *_: Any, **__: Any
    ) -> OrchestrationOutcome:
        self.reaped.append(ref.item_id)
        return OrchestrationOutcome(
            ref=ref,
            action="deleted",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=document_id,
            detail="test reap",
        )

    async def confirm_orphan_absent(
        self, ref: SourceItemRef, library_id: UUID, *_: Any, **__: Any
    ) -> OrchestrationOutcome:
        self.confirmed_absent.append(ref.item_id)
        return OrchestrationOutcome(
            ref=ref,
            action="source_absent",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=None,
            detail="test absence",
        )


@dataclass
class _Sweep:
    report: SyncReport
    orchestrator: _RecordingOrchestrator
    ironrag: _ListingIronRag
    state: StateStore
    seeded: dict[str, tuple[Any, list[Any]]] = field(default_factory=dict)

    def state_unchanged(self, item_id: str) -> bool:
        cursor_before, locations_before = self.seeded[item_id]
        return (
            self.state.get(KIND_FILE, item_id) == cursor_before
            and self.state.locations_for(KIND_FILE, item_id) == locations_before
        )


def _item(repo: str, branch: str, path: str) -> str:
    return file_item_id(repo, branch, path)


async def _sweep(
    state_root: Path,
    repos_yaml: Path,
    *,
    seeded: list[tuple[str, str, str]],
    on_missing: DeleteAction = DeleteAction.DELETE,
    adapter: GitReposAdapter | None = None,
) -> _Sweep:
    adapter = adapter or GitReposAdapter(
        make_settings(state_root=state_root, repos_yaml=repos_yaml)
    )
    state = StateStore(state_root / "state.sqlite")
    documents: list[DocumentResource] = []
    snapshot: dict[str, tuple[Any, list[Any]]] = {}
    for index, (repo, branch, path) in enumerate(seeded, start=1):
        item_id = _item(repo, branch, path)
        external_key = file_external_key(repo, branch, path)
        state.upsert(
            kind=KIND_FILE,
            item_id=item_id,
            change_token=f"blob-{index}",
            external_key=external_key,
            ironrag_document_id=str(UUID(int=index)),
            ironrag_library_id=str(LIB),
        )
        snapshot[item_id] = (
            state.get(KIND_FILE, item_id),
            state.locations_for(KIND_FILE, item_id),
        )
        documents.append(
            DocumentResource(
                id=UUID(int=index),
                library_id=LIB,
                external_key=external_key,
                status="ready",
            )
        )
    ironrag = _ListingIronRag(tuple(documents))
    orchestrator = _RecordingOrchestrator()
    library_ref = "tests/default-library"
    router = Router(
        RoutingConfig.model_validate({"default": {"library": library_ref}}),
        resolved_targets={
            library_ref: ResolvedLibraryTarget(
                library_ref=library_ref, workspace_id=WS, library_id=LIB
            )
        },
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=PolicyOverrides(default=PushPolicy(on_missing=on_missing), by_kind={}),
        concurrency=2,
        interval_seconds=60,
    )
    report = await manager.run_once(reason="test")
    return _Sweep(report, orchestrator, ironrag, state, snapshot)


def _assert_incomplete_and_untouched(sweep: _Sweep, item_id: str) -> None:
    assert sweep.report.census_complete is False
    assert sweep.report.errors >= 1
    assert sweep.report.reaped == 0
    assert sweep.orchestrator.reaped == []
    assert sweep.orchestrator.confirmed_absent == []
    assert sweep.ironrag.list_calls == []
    assert sweep.state_unchanged(item_id)


async def test_unreadable_repo_keeps_its_documents_and_still_pushes_other_repos(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    alive = tmp_repo_factory("alive")
    alive.write("a.md", "alive\n")
    alive.commit("init")
    gone = tmp_repo_factory("gone")
    gone.write("doc.md", "gone\n")
    gone.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {"name": "alive", "local_path": str(alive.path), "branches": ["main"]},
            {"name": "gone", "local_path": str(gone.path), "branches": ["main"]},
        ],
    )
    shutil.rmtree(gone.path)

    sweep = await _sweep(state_root, repos_yaml, seeded=[("gone", "main", "doc.md")])

    _assert_incomplete_and_untouched(sweep, _item("gone", "main", "doc.md"))
    assert sweep.orchestrator.pushed == [_item("alive", "main", "a.md")]


async def test_unlistable_branch_keeps_documents_of_the_whole_source(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("partial")
    repo.write("kept.md", "kept\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "partial", "local_path": str(repo.path), "branches": ["main", "absent"]}],
    )

    # `removed.md` is genuinely gone from `main`; a complete census would reap
    # it. The unlistable sibling branch withdraws that proof for the sweep.
    sweep = await _sweep(state_root, repos_yaml, seeded=[("partial", "main", "removed.md")])

    _assert_incomplete_and_untouched(sweep, _item("partial", "main", "removed.md"))
    assert sweep.orchestrator.pushed == [_item("partial", "main", "kept.md")]


async def test_malformed_tree_record_withdraws_census_proof(
    tmp_repo_factory: TmpRepoFactory, state_root: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    repo = tmp_repo_factory("corrupt")
    repo.write("kept.md", "kept\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "corrupt", "local_path": str(repo.path), "branches": ["main"]}],
    )
    adapter = GitReposAdapter(make_settings(state_root=state_root, repos_yaml=repos_yaml))
    original_run = adapter._client._run

    async def corrupted_run(cwd: Path, args: list[str], **kwargs: Any) -> bytes:
        out = await original_run(cwd, args, **kwargs)
        if args[0] == "ls-tree":
            return out + b"100644 blob 0123456789abcdef0123456789abcdef01234567 many\tkept.md\x00"
        return out

    monkeypatch.setattr(adapter._client, "_run", corrupted_run)

    sweep = await _sweep(
        state_root, repos_yaml, seeded=[("corrupt", "main", "removed.md")], adapter=adapter
    )

    _assert_incomplete_and_untouched(sweep, _item("corrupt", "main", "removed.md"))


@pytest.mark.parametrize(
    ("on_missing", "expect_reaped"),
    [(DeleteAction.DELETE, True), (DeleteAction.IGNORE, False)],
)
async def test_file_grown_past_size_ceiling_leaves_the_admitted_scope(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
    on_missing: DeleteAction,
    expect_reaped: bool,
) -> None:
    repo = tmp_repo_factory("sized")
    repo.write("small.md", "ok\n")
    repo.write("big.md", "x" * 4096)
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

    sweep = await _sweep(
        state_root, repos_yaml, seeded=[("sized", "main", "big.md")], on_missing=on_missing
    )

    assert sweep.report.census_complete is True
    assert sweep.report.errors == 0
    assert sweep.orchestrator.pushed == [_item("sized", "main", "small.md")]
    assert sweep.orchestrator.reaped == (
        [_item("sized", "main", "big.md")] if expect_reaped else []
    )


@pytest.mark.parametrize(
    ("on_missing", "expect_reaped"),
    [(DeleteAction.DELETE, True), (DeleteAction.IGNORE, False)],
)
async def test_file_newly_excluded_by_pattern_leaves_the_admitted_scope(
    tmp_repo_factory: TmpRepoFactory,
    state_root: Path,
    on_missing: DeleteAction,
    expect_reaped: bool,
) -> None:
    repo = tmp_repo_factory("filtered")
    repo.write("src/kept.py", "x = 1\n")
    repo.write("docs/dropped.md", "dropped\n")
    repo.commit("init")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [
            {
                "name": "filtered",
                "local_path": str(repo.path),
                "branches": ["main"],
                "exclude": ["docs/**"],
            }
        ],
    )

    sweep = await _sweep(
        state_root,
        repos_yaml,
        seeded=[("filtered", "main", "docs/dropped.md")],
        on_missing=on_missing,
    )

    assert sweep.report.census_complete is True
    assert sweep.report.errors == 0
    assert sweep.orchestrator.pushed == [_item("filtered", "main", "src/kept.py")]
    assert sweep.orchestrator.reaped == (
        [_item("filtered", "main", "docs/dropped.md")] if expect_reaped else []
    )


async def test_complete_census_reaps_a_file_deleted_from_the_branch(
    tmp_repo_factory: TmpRepoFactory, state_root: Path
) -> None:
    repo = tmp_repo_factory("clean")
    repo.write("kept.md", "kept\n")
    repo.write("removed.md", "removed\n")
    repo.commit("init")
    repo.remove("removed.md")
    repo.commit("drop")
    repos_yaml = write_repos_yaml(
        state_root / "repos.yaml",
        [{"name": "clean", "local_path": str(repo.path), "branches": ["main"]}],
    )

    sweep = await _sweep(state_root, repos_yaml, seeded=[("clean", "main", "removed.md")])

    assert sweep.report.census_complete is True
    assert sweep.report.errors == 0
    assert sweep.orchestrator.pushed == [_item("clean", "main", "kept.md")]
    assert sweep.orchestrator.reaped == [_item("clean", "main", "removed.md")]
    assert sweep.report.reaped == 1
