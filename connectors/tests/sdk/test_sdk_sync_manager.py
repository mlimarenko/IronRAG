from __future__ import annotations

import asyncio
import sqlite3
import time
from collections.abc import AsyncIterator
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from unittest.mock import patch
from uuid import UUID

import httpx
import pytest
from sdk_dependent_fixtures import (
    LIB2,
    DependentPageAdapter,
    DependentSpec,
    dependent_ref,
    dependents_fingerprint,
    echo_page,
    router_to,
    seed_document,
)
from test_sdk_orchestrator import FakeIronRag as RichIronRag
from test_sdk_orchestrator import PolicyFailureIronRag

from ironrag_connector.config import BaseConnectorSettings
from ironrag_connector.ironrag import (
    DocumentResource,
    IronRagClient,
    IronRagError,
    IronRagNotFoundError,
    IronRagProblemError,
    OperationStatus,
    ProblemDetails,
)
from ironrag_connector.orchestrator import OrchestrationOutcome, Orchestrator, OutcomeAction
from ironrag_connector.policy import DeleteAction, PushPolicy, UpdateAction
from ironrag_connector.routing import (
    PolicyOverrides,
    ResolvedLibraryTarget,
    Router,
    RoutingConfig,
)
from ironrag_connector.source import (
    IncompleteCensusError,
    SourceDeleteEnforcement,
    SourceExclusion,
    SourceFetchDelete,
    SourceFetchResult,
    SourceItemRef,
)
from ironrag_connector.sources.confluence.adapter import ConfluenceAdapter
from ironrag_connector.sources.confluence.config import ConfluenceSettings
from ironrag_connector.sources.confluence.confluence import ConfluenceClient
from ironrag_connector.sources.echo.adapter import EchoAdapter, EchoPage
from ironrag_connector.state import CursorRow, DocumentLocation, MemberTarget, StateStore
from ironrag_connector.sync import (
    SyncAlreadyRunningError,
    SyncManager,
    SyncReport,
    _snapshot_reap_state,
    _tally,
)

WS = UUID("00000000-0000-0000-0000-000000000099")
LIB = UUID("00000000-0000-0000-0000-000000000000")
LIB_HISTORICAL = UUID("00000000-0000-0000-0000-000000000001")


class EmptyBlockingAdapter:
    name = "blocking"
    kinds = ("page",)
    primary_kinds = ("page",)

    def __init__(self, started: asyncio.Event, release: asyncio.Event) -> None:
        self._started = started
        self._release = release

    async def iter_items(self) -> Any:
        self._started.set()
        await self._release.wait()
        if False:
            yield SourceItemRef(
                item_id="never",
                kind="page",
                external_key="blocking:page:never",
            )

    def external_key(self, kind: str, item_id: str) -> str:
        return f"blocking:{kind}:{item_id}"

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        parts = external_key.split(":", 2)
        if len(parts) != 3 or parts[0] != "blocking":
            return None
        return parts[1], parts[2]

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        return SourceDeleteEnforcement.POLICY


class OneRefAdapter(EmptyBlockingAdapter):
    async def iter_items(self) -> Any:
        yield SourceItemRef(item_id="1", kind="page", external_key="blocking:page:1")


class FakeIronRag:
    async def ensure_visibility_contract(self) -> None:
        return None

    async def list_documents(self, *_: Any, **__: Any) -> AsyncIterator[DocumentResource]:
        return
        yield  # pragma: no cover -- makes this an async generator

    async def get_document(self, *_: Any, **__: Any) -> DocumentResource | None:
        return None


class SlowListIronRag(FakeIronRag):
    def __init__(self) -> None:
        self.started = asyncio.Event()

    async def list_documents(self, *_: Any, **__: Any) -> AsyncIterator[DocumentResource]:
        self.started.set()
        await asyncio.Event().wait()
        raise AssertionError("slow reaper list should time out")
        yield  # pragma: no cover -- makes this an async generator


class ErrorListIronRag(FakeIronRag):
    async def list_documents(self, *_: Any, **__: Any) -> AsyncIterator[DocumentResource]:
        raise IronRagError("synthetic list failure")
        yield  # pragma: no cover -- makes this an async generator


class ListingIronRag(FakeIronRag):
    def __init__(self, documents: dict[UUID, tuple[DocumentResource, ...]]) -> None:
        self.documents = documents
        self.list_calls: list[tuple[UUID, str | None]] = []

    async def list_documents(
        self,
        library_id: UUID,
        *,
        search: str | None = None,
        **_: Any,
    ) -> AsyncIterator[DocumentResource]:
        self.list_calls.append((library_id, search))
        for document in self.documents.get(library_id, ()):
            yield document


class PausingListingIronRag(ListingIronRag):
    def __init__(self, documents: dict[UUID, tuple[DocumentResource, ...]]) -> None:
        super().__init__(documents)
        self.listed = asyncio.Event()
        self.resume = asyncio.Event()
        self.delete_calls: list[str] = []

    async def list_documents(
        self,
        library_id: UUID,
        *,
        search: str | None = None,
        **_: Any,
    ) -> AsyncIterator[DocumentResource]:
        self.list_calls.append((library_id, search))
        for document in self.documents.get(library_id, ()):
            yield document
            self.listed.set()
            await self.resume.wait()

    async def delete_document(
        self,
        document_id: UUID | str,
        *,
        idempotency_key: str,
    ) -> None:
        del idempotency_key
        self.delete_calls.append(str(document_id))


class NoopOrchestrator:
    def reset_sweep_cache(self, _: dict[str, set[UUID]] | None = None) -> None:
        pass

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        return OrchestrationOutcome(
            ref=ref,
            action="noop_unchanged",
            workspace_id=WS,
            library_id=LIB,
            rule_description=None,
            ironrag_document_id="doc-1",
            detail="test",
        )

    async def reap_orphan(self, *_: Any, **__: Any) -> OrchestrationOutcome:
        raise AssertionError("no documents should be reaped in these tests")

    async def confirm_orphan_absent(self, *_: Any, **__: Any) -> OrchestrationOutcome:
        raise AssertionError("no absent documents should be confirmed in these tests")


class RecordingReaperOrchestrator(NoopOrchestrator):
    def __init__(self) -> None:
        self.pre_sweep_libraries: dict[str, set[UUID]] = {}
        self.reap_calls: list[tuple[SourceItemRef, UUID, str, SourceDeleteEnforcement]] = []
        self.absent_calls: list[tuple[SourceItemRef, UUID, SourceDeleteEnforcement]] = []

    def reset_sweep_cache(
        self,
        pre_sweep_libraries: dict[str, set[UUID]] | None = None,
    ) -> None:
        self.pre_sweep_libraries = pre_sweep_libraries or {}

    async def reap_orphan(
        self,
        ref: SourceItemRef,
        library_id: UUID,
        document_id: str,
        _: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> OrchestrationOutcome:
        del expected_cursor, expected_location
        self.reap_calls.append((ref, library_id, document_id, enforcement))
        return OrchestrationOutcome(
            ref=ref,
            action="deleted",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=document_id,
            detail="test reaper",
        )

    async def confirm_orphan_absent(
        self,
        ref: SourceItemRef,
        library_id: UUID,
        _: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> OrchestrationOutcome:
        del expected_cursor, expected_location
        self.absent_calls.append((ref, library_id, enforcement))
        return OrchestrationOutcome(
            ref=ref,
            action="source_absent",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=None,
            detail="test absence confirmation",
        )


class RouteMovingOrchestrator(RecordingReaperOrchestrator):
    def __init__(self, state: StateStore) -> None:
        super().__init__()
        self._state = state

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        self._state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token=ref.change_token,
            external_key=ref.external_key,
            ironrag_document_id=str(UUID(int=1)),
            ironrag_library_id=str(LIB),
        )
        return await super().push_ref(ref)


class ErrorPushOrchestrator(RecordingReaperOrchestrator):
    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        raise IronRagError(f"synthetic fetch failure for {ref.external_key}")


class FailingReaperOrchestrator(RecordingReaperOrchestrator):
    async def reap_orphan(self, *_: Any, **__: Any) -> OrchestrationOutcome:
        raise IronRagError("synthetic delete failure")


class RacingConfirmationOrchestrator(Orchestrator):
    async def confirm_orphan_absent(
        self,
        ref: SourceItemRef,
        library_id: UUID,
        policy: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> OrchestrationOutcome:
        # Deterministic list -> webhook interleaving: this fresh observation
        # lands after the reaper's successful empty list but before its CAS.
        self._state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token="v2",
            external_key=ref.external_key,
            ironrag_document_id=str(UUID(int=2)),
            ironrag_library_id=str(library_id),
        )
        return await super().confirm_orphan_absent(
            ref,
            library_id,
            policy,
            enforcement=enforcement,
            expected_cursor=expected_cursor,
            expected_location=expected_location,
        )


class ExistingRouteOrchestrator(Orchestrator):
    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        return OrchestrationOutcome(
            ref=ref,
            action="noop_unchanged",
            workspace_id=WS,
            library_id=LIB,
            rule_description=None,
            ironrag_document_id=str(UUID(int=2)),
            detail="test current route",
        )


class CountingReloader:
    def __init__(self) -> None:
        self.calls = 0

    async def reload_if_changed(self) -> bool:
        self.calls += 1
        return False


class BlockingOrchestrator(NoopOrchestrator):
    def __init__(self, started: asyncio.Event, cancelled: asyncio.Event) -> None:
        self._started = started
        self._cancelled = cancelled

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        self._started.set()
        try:
            await asyncio.Event().wait()
        except asyncio.CancelledError:
            self._cancelled.set()
            raise
        raise AssertionError("blocking push_ref should not return")


class RaisingOrchestrator(NoopOrchestrator):
    """Fails every item with the same exception, so the sweep must classify it."""

    def __init__(self, exc: BaseException) -> None:
        self._exc = exc

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        del ref
        raise self._exc


class TimeoutThenSlowSuccessOrchestrator(NoopOrchestrator):
    """Blocks past the budget once, then answers with per-call delays.

    The first call never returns; later calls sleep for ``replay_delay`` (call 2)
    and ``restored_delay`` (call 3 onwards). The two delays are chosen around the
    retry and full budgets to show which one is in force.
    """

    def __init__(self, *, replay_delay: float, restored_delay: float) -> None:
        self._delays: list[float | None] = [None, replay_delay, restored_delay]
        self.calls = 0

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        delay = self._delays[min(self.calls, len(self._delays) - 1)]
        self.calls += 1
        if delay is None:
            await asyncio.Event().wait()
        await asyncio.sleep(delay)
        return await super().push_ref(ref)


class SlowSuccessOrchestrator(NoopOrchestrator):
    """Always completes, after a fixed delay, with a noop outcome.

    The delay sits above the retry budget and below the full one, so a sweep's
    verdict shows which budget was in force.
    """

    def __init__(self, delay: float) -> None:
        self._delay = delay
        self.calls = 0

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        self.calls += 1
        await asyncio.sleep(self._delay)
        return await super().push_ref(ref)


class _BrokenTimeoutState(StateStore):
    """A StateStore whose item-timeout bookkeeping fails on demand.

    F2 regression double: a SQLite failure while reading, recording or clearing
    the timeout count must not propagate out of ``process()`` and abort a sweep.
    """

    def __init__(
        self,
        db_path: Path,
        *,
        break_read: bool = False,
        break_write: bool = False,
        break_clear: bool = False,
    ) -> None:
        super().__init__(db_path)
        self._break_read = break_read
        self._break_write = break_write
        self._break_clear = break_clear

    def item_timeout_attempts(self, kind: str, item_id: str) -> int:
        if self._break_read:
            raise sqlite3.OperationalError("synthetic item_timeout read failure")
        return super().item_timeout_attempts(kind, item_id)

    def record_item_timeout(self, *, kind: str, item_id: str) -> int:
        if self._break_write:
            raise sqlite3.OperationalError("synthetic item_timeout write failure")
        return super().record_item_timeout(kind=kind, item_id=item_id)

    def clear_item_timeout(self, *, kind: str, item_id: str) -> None:
        if self._break_clear:
            raise sqlite3.OperationalError("synthetic item_timeout clear failure")
        super().clear_item_timeout(kind=kind, item_id=item_id)


def _router() -> Router:
    library_ref = "tests/default-library"
    return Router(
        RoutingConfig.model_validate({"default": {"library": library_ref}}),
        resolved_targets={
            library_ref: ResolvedLibraryTarget(
                library_ref=library_ref,
                workspace_id=WS,
                library_id=LIB,
            )
        },
    )


def _manager(
    tmp_path: Path,
    *,
    adapter: Any,
    orchestrator: NoopOrchestrator,
    ironrag: Any | None = None,
    item_timeout_seconds: float = 300.0,
    item_retry_timeout_seconds: float = 30.0,
    reaper_list_timeout_seconds: float = 30.0,
    routing_reloader: Any | None = None,
    policies: PolicyOverrides | None = None,
    state: StateStore | None = None,
    router: Router | None = None,
) -> SyncManager:
    router = router if router is not None else _router()
    return SyncManager(
        adapter=adapter,
        ironrag=ironrag or FakeIronRag(),  # type: ignore[arg-type]
        orchestrator=orchestrator,  # type: ignore[arg-type]
        router=router,
        state=state if state is not None else StateStore(tmp_path / "state.sqlite"),
        policies=(
            policies if policies is not None else PolicyOverrides(default=PushPolicy(), by_kind={})
        ),
        concurrency=1,
        interval_seconds=60,
        item_timeout_seconds=item_timeout_seconds,
        item_retry_timeout_seconds=item_retry_timeout_seconds,
        reaper_list_timeout_seconds=reaper_list_timeout_seconds,
        routing_reloader=routing_reloader,
    )


@pytest.mark.asyncio
async def test_run_once_rejects_overlapping_sweeps(tmp_path: Path) -> None:
    started = asyncio.Event()
    release = asyncio.Event()
    manager = _manager(
        tmp_path,
        adapter=EmptyBlockingAdapter(started, release),
        orchestrator=NoopOrchestrator(),
    )

    first = asyncio.create_task(manager.run_once(reason="first"))
    await asyncio.wait_for(started.wait(), timeout=1)

    with pytest.raises(SyncAlreadyRunningError):
        await manager.run_once(reason="second")

    release.set()
    report = await asyncio.wait_for(first, timeout=1)
    assert report.errors == 0


@pytest.mark.asyncio
async def test_requested_cleanup_waits_for_manual_sweep_and_coalesces_deliveries(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    started, release, requested = asyncio.Event(), asyncio.Event(), asyncio.Event()
    request_waiting = asyncio.Event()
    completed: asyncio.Queue[str] = asyncio.Queue()
    manager = _manager(
        tmp_path,
        adapter=EmptyBlockingAdapter(started, release),
        orchestrator=NoopOrchestrator(),
    )
    run_once = manager.run_once

    async def observed_run_once(*, reason: str, wait_for_idle: bool = False) -> SyncReport:
        if wait_for_idle:
            request_waiting.set()
        report = await run_once(reason=reason, wait_for_idle=wait_for_idle)
        completed.put_nowait(reason)
        return report

    monkeypatch.setattr(manager, "run_once", observed_run_once)
    manual = asyncio.create_task(manager.run_once(reason="manual"))
    worker = asyncio.create_task(manager.run_on_request(requested))
    try:
        await asyncio.wait_for(started.wait(), timeout=1)
        requested.set()
        await asyncio.wait_for(request_waiting.wait(), timeout=1)
        assert completed.empty()
        assert requested.is_set() is False
        # Several arrivals while the worker waits behind the manual sweep
        # become one follow-up request, not a dropped request or busy retry.
        requested.set()
        requested.set()
        release.set()
        await asyncio.wait_for(manual, timeout=1)
        assert await asyncio.wait_for(completed.get(), timeout=1) == "manual"
        for _ in range(2):
            assert await asyncio.wait_for(completed.get(), timeout=1) == "webhook-dependent-cleanup"
        with pytest.raises(TimeoutError):
            await asyncio.wait_for(completed.get(), timeout=0.05)
        assert manager._run_lock.locked() is False
    finally:
        manual.cancel()
        worker.cancel()
        await asyncio.gather(manual, worker, return_exceptions=True)


@pytest.mark.asyncio
async def test_run_once_cancellation_cancels_item_tasks(tmp_path: Path) -> None:
    started = asyncio.Event()
    cancelled = asyncio.Event()
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=BlockingOrchestrator(started, cancelled),
    )

    task = asyncio.create_task(manager.run_once(reason="cancel-test"))
    await asyncio.wait_for(started.wait(), timeout=1)
    task.cancel()

    with pytest.raises(asyncio.CancelledError):
        await task
    await asyncio.wait_for(cancelled.wait(), timeout=1)


@pytest.mark.asyncio
async def test_item_timeout_is_counted_separately_from_errors(tmp_path: Path) -> None:
    started = asyncio.Event()
    cancelled = asyncio.Event()
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=BlockingOrchestrator(started, cancelled),
        item_timeout_seconds=0.05,
    )

    report = await manager.run_once(reason="timeout-test")

    assert report.items_seen == 1
    assert report.item_timeouts == 1
    assert report.errors == 0
    await asyncio.wait_for(cancelled.wait(), timeout=1)

    second = await manager.run_once(reason="after-timeout")
    assert second.item_timeouts == 1
    assert second.errors == 0


def test_effective_item_timeout_escalates_to_full_every_fifth_attempt() -> None:
    from ironrag_connector.sync import (
        FULL_BUDGET_ESCALATION_ATTEMPTS,
        _effective_item_timeout,
    )

    assert _effective_item_timeout(300.0, 30.0, 0) == 300.0
    for attempts in range(1, FULL_BUDGET_ESCALATION_ATTEMPTS):
        assert _effective_item_timeout(300.0, 30.0, attempts) == 30.0
    assert _effective_item_timeout(300.0, 30.0, FULL_BUDGET_ESCALATION_ATTEMPTS) == 300.0
    assert _effective_item_timeout(300.0, 30.0, FULL_BUDGET_ESCALATION_ATTEMPTS + 1) == 30.0
    # A retry budget larger than the full budget never raises it.
    assert _effective_item_timeout(60.0, 300.0, 1) == 60.0
    assert _effective_item_timeout(60.0, 300.0, FULL_BUDGET_ESCALATION_ATTEMPTS) == 60.0


@pytest.mark.asyncio
async def test_second_sweep_of_blocked_item_uses_reduced_budget(tmp_path: Path) -> None:
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=BlockingOrchestrator(asyncio.Event(), asyncio.Event()),
        item_timeout_seconds=0.4,
        item_retry_timeout_seconds=0.05,
    )

    first_start = time.monotonic()
    first = await manager.run_once(reason="first")
    first_duration = time.monotonic() - first_start

    second_start = time.monotonic()
    second = await manager.run_once(reason="second")
    second_duration = time.monotonic() - second_start

    assert first.item_timeouts == 1
    assert first.errors == 0
    assert second.item_timeouts == 1
    assert second.errors == 0
    # 4-8x margins: the blocked ref burns the full budget once, then the short one.
    assert first_duration >= 0.3
    assert second_duration < 0.2


@pytest.mark.asyncio
async def test_consecutive_retry_timeouts_escalate_back_to_the_full_budget(
    tmp_path: Path,
) -> None:
    """F1: a slow-but-real ref must not stay starved on the retry budget.

    The ref is seeded one timeout behind, which is exactly the state the old
    ``attempts >= 1`` rule starved forever. Every call then takes 0.3s: six
    times the 0.05s retry budget, three times under the 1.0s full budget. Sweeps
    1-4 exhaust the retry budget; sweep 5 escalates back to the full budget and
    completes, clearing the count.
    """
    state = StateStore(tmp_path / "state.sqlite")
    state.record_item_timeout(kind="page", item_id="1")
    orchestrator = SlowSuccessOrchestrator(0.3)
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=orchestrator,
        state=state,
        item_timeout_seconds=1.0,
        item_retry_timeout_seconds=0.05,
    )

    reports = [await manager.run_once(reason=f"sweep-{n}") for n in range(1, 7)]

    for sweep, report in enumerate(reports[:4], start=1):
        assert report.item_timeouts == 1, f"sweep {sweep} must exhaust the retry budget"
        assert report.noop_unchanged == 0, f"sweep {sweep} must not ingest yet"
    assert reports[4].item_timeouts == 0, "sweep 5 escalates to the full budget"
    assert reports[4].noop_unchanged == 1
    assert reports[5].noop_unchanged == 1, "the cleared count keeps the full budget in force"
    assert state.item_timeout_attempts("page", "1") == 0
    assert orchestrator.calls == 6


@pytest.mark.asyncio
async def test_successful_item_clears_timeout_attempts(tmp_path: Path) -> None:
    orchestrator = TimeoutThenSlowSuccessOrchestrator(replay_delay=0.01, restored_delay=0.3)
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=orchestrator,
        item_timeout_seconds=1.0,
        item_retry_timeout_seconds=0.1,
    )

    first = await manager.run_once(reason="first")
    assert first.item_timeouts == 1
    assert manager._state.item_timeout_attempts("page", "1") == 1

    # The replay is short (0.01s) and succeeds inside the 0.1s retry budget,
    # which clears the counter.
    second = await manager.run_once(reason="second")
    assert second.item_timeouts == 0
    assert second.errors == 0
    assert second.noop_unchanged == 1
    assert manager._state.item_timeout_attempts("page", "1") == 0

    # With the counter cleared the next sweep is back on the full budget: 0.3s
    # exceeds the 0.1s retry budget but fits inside the 1.0s full one.
    third = await manager.run_once(reason="third")
    assert third.item_timeouts == 0
    assert third.noop_unchanged == 1


@pytest.mark.asyncio
async def test_non_timeout_failure_clears_timeout_attempts(tmp_path: Path) -> None:
    state = StateStore(tmp_path / "state.sqlite")
    state.record_item_timeout(kind="page", item_id="1")
    assert state.item_timeout_attempts("page", "1") == 1
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(IronRagError("synthetic backend failure")),
        state=state,
    )

    report = await manager.run_once(reason="non-timeout-failure")

    assert report.errors == 1
    assert state.item_timeout_attempts("page", "1") == 0


@pytest.mark.asyncio
async def test_http_problem_is_an_error_and_clears_timeout_attempts(tmp_path: Path) -> None:
    """An HTTP problem is never a content skip, whatever its code.

    The backend's ``unsupported_upload_type`` was once read off a synchronous
    problem response; IRG-173 makes the polled operation's disposition the only
    proof, so the problem stays in ``errors``."""
    problem = ProblemDetails(
        type="urn:ironrag:error:unsupported",
        title="Unsupported",
        status=400,
        detail="unsupported file type",
        code="unsupported_upload_type",
    )
    state = StateStore(tmp_path / "state.sqlite")
    state.record_item_timeout(kind="page", item_id="1")
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(IronRagProblemError(problem)),
        state=state,
    )

    report = await manager.run_once(reason="http-problem")

    assert report.errors == 1
    assert report.skipped_unsupported == 0
    assert state.item_timeout_attempts("page", "1") == 0


@pytest.mark.asyncio
async def test_timeout_bookkeeping_read_failure_does_not_abort_the_sweep(
    tmp_path: Path,
) -> None:
    """F2: a SQLite failure reading the count must not escape ``process()``."""
    state = _BrokenTimeoutState(tmp_path / "state.sqlite", break_read=True)
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=NoopOrchestrator(),
        state=state,
    )

    report = await manager.run_once(reason="read-failure")

    assert report.census_complete is True
    assert report.items_seen == 1
    assert report.noop_unchanged == 1
    assert report.errors == 0
    assert not manager._run_lock.locked()


@pytest.mark.asyncio
async def test_timeout_bookkeeping_write_failure_does_not_abort_a_timeout_sweep(
    tmp_path: Path,
) -> None:
    """F2: a SQLite failure recording the timeout must not abort the sweep."""
    state = _BrokenTimeoutState(tmp_path / "state.sqlite", break_write=True)
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=BlockingOrchestrator(asyncio.Event(), asyncio.Event()),
        state=state,
        item_timeout_seconds=0.05,
    )

    report = await manager.run_once(reason="write-failure")

    assert report.census_complete is True
    assert report.item_timeouts == 1
    assert report.errors == 0
    assert not manager._run_lock.locked()


@pytest.mark.asyncio
async def test_timeout_bookkeeping_clear_failure_does_not_abort_the_sweep(
    tmp_path: Path,
) -> None:
    """F2: a SQLite failure clearing the count must not abort the sweep."""
    state = _BrokenTimeoutState(tmp_path / "state.sqlite", break_clear=True)
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=NoopOrchestrator(),
        state=state,
    )

    report = await manager.run_once(reason="clear-failure")

    assert report.census_complete is True
    assert report.noop_unchanged == 1
    assert report.errors == 0
    assert not manager._run_lock.locked()


def _failed_operation_body(**overrides: object) -> dict[str, object]:
    body: dict[str, object] = {
        "id": str(UUID(int=90)),
        "workspaceId": str(WS),
        "libraryId": str(LIB),
        "operationKind": "revise_document",
        "status": "failed",
        "createdAt": "2026-09-23T00:00:00Z",
        "progress": {"total": 1, "completed": 0, "failed": 1, "inFlight": 0},
    }
    body.update(overrides)
    return body


async def _operation_failure(body: dict[str, object]) -> BaseException:
    """The exception the production client raises for this operation body.

    Drives the real :class:`IronRagClient` over a mock transport, so the sync
    tests exercise the one disposition mapping instead of re-deriving it.
    """
    client = IronRagClient(
        BaseConnectorSettings(
            ironrag_base_url="http://ironrag.example.com",
            ironrag_api_token="token",
            request_timeout_seconds=5.0,
            operation_poll_interval_seconds=0.05,
            operation_poll_budget_seconds=1.0,
        ),
        client=httpx.AsyncClient(
            base_url="http://ironrag.example.com",
            transport=httpx.MockTransport(lambda request: httpx.Response(200, json=body)),
        ),
    )
    try:
        await client.wait_for_operation(UUID(str(body["id"])))
    except BaseException as exc:
        return exc
    finally:
        await client.aclose()
    raise AssertionError("the operation body did not raise")


@pytest.mark.asyncio
async def test_content_unsupported_disposition_is_skipped_not_an_error(tmp_path: Path) -> None:
    """Only the server-proven disposition skips; the failure code is audit only.

    ``upload_extraction_failed`` alone was always an error, and still is: this
    operation is a skip only because the server wrote
    ``failureDisposition=content_unsupported``."""
    exc = await _operation_failure(
        _failed_operation_body(
            failureCode="upload_extraction_failed",
            failureDisposition="content_unsupported",
        )
    )
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="content-unsupported")

    assert report.items_seen == 1
    assert report.skipped_unsupported == 1
    assert report.errors == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "disposition",
    [None, "retry_exhausted", "operational_terminal", "future_server_value"],
)
async def test_unproven_disposition_stays_an_error_even_with_a_known_code(
    tmp_path: Path, disposition: str | None
) -> None:
    """``failureCode`` no longer decides anything.

    Every non-``content_unsupported`` disposition -- absent (an older server),
    retryable, operational or unknown -- is an error, even when the code is one
    the connector used to skip."""
    overrides: dict[str, object] = {"failureCode": "unsupported_upload_type"}
    if disposition is not None:
        overrides["failureDisposition"] = disposition
    exc = await _operation_failure(_failed_operation_body(**overrides))
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="unproven-disposition")

    assert report.errors == 1
    assert report.skipped_unsupported == 0


@pytest.mark.asyncio
@pytest.mark.parametrize("code", ["unsupported_upload_type", "invalid_text_encoding"])
async def test_http_problem_code_is_not_a_skip(tmp_path: Path, code: str) -> None:
    """A synchronous HTTP problem is never a content rejection."""
    exc = IronRagProblemError(
        ProblemDetails(
            type="urn:ironrag:error:unsupported",
            title="Unsupported",
            status=400,
            detail="unsupported file type",
            code=code,
        )
    )
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="http-problem-code")

    assert report.errors == 1
    assert report.skipped_unsupported == 0


@pytest.mark.asyncio
async def test_repeated_http_429_never_becomes_a_skip(tmp_path: Path) -> None:
    """A repeated transport failure is not terminal evidence.

    The sweep must count it as an error on every pass, never accumulate it into
    a skip from repetition."""
    exc = IronRagProblemError(
        ProblemDetails(
            type="urn:ironrag:error:rate_limited",
            title="Too many requests",
            status=429,
            detail="slow down",
            code="too_many_requests",
        )
    )
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    first = await manager.run_once(reason="429-first")
    second = await manager.run_once(reason="429-second")

    assert first.errors == 1
    assert first.skipped_unsupported == 0
    assert second.errors == 1
    assert second.skipped_unsupported == 0


@pytest.mark.asyncio
async def test_typed_backend_code_outside_the_set_still_counts_as_an_error(
    tmp_path: Path,
) -> None:
    """A typed operation failure with no content disposition is an error."""
    exc = await _operation_failure(_failed_operation_body(failureCode="upload_extraction_failed"))
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="unlisted-backend-code")

    assert report.errors == 1
    assert report.skipped_unsupported == 0


@pytest.mark.asyncio
async def test_unknown_backend_error_still_counts_as_an_error(tmp_path: Path) -> None:
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(IronRagError("synthetic backend failure")),
    )

    report = await manager.run_once(reason="unknown-error")

    assert report.errors == 1
    assert report.skipped_unsupported == 0
    assert report.item_timeouts == 0


def test_report_as_dict_exposes_separated_item_counters() -> None:
    from datetime import UTC, datetime

    from ironrag_connector.sync import SyncReport

    now = datetime.now(tz=UTC)
    report = SyncReport(started_at=now, finished_at=now)
    report.item_timeouts = 2
    report.skipped_unsupported = 3

    as_dict = report.as_dict()
    assert as_dict["item_timeouts"] == 2
    assert as_dict["skipped_unsupported"] == 3


@pytest.mark.asyncio
async def test_reaper_list_timeout_releases_run_lock(tmp_path: Path) -> None:
    ironrag = SlowListIronRag()
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=NoopOrchestrator(),
        ironrag=ironrag,
        reaper_list_timeout_seconds=0.01,
    )

    report = await asyncio.wait_for(manager.run_once(reason="reaper-timeout"), timeout=1)

    assert report.items_seen == 1
    assert report.errors == 0
    assert ironrag.started.is_set()

    second = await asyncio.wait_for(manager.run_once(reason="after-reaper"), timeout=1)
    assert second.items_seen == 1


@pytest.mark.asyncio
async def test_run_once_reloads_routing_before_sweep(tmp_path: Path) -> None:
    reloader = CountingReloader()
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=NoopOrchestrator(),
        routing_reloader=reloader,
    )

    report = await manager.run_once(reason="reload-test")

    assert report.items_seen == 1
    assert reloader.calls == 1


def test_report_separates_items_with_nothing_to_take_from_deleted_ones() -> None:
    """`skipped_missing` used to absorb both, and they mean opposite things.

    An empty page still exists upstream and may be filled in tomorrow; a missing
    one is gone and its document should be reaped. Reading a steady
    `skipped_missing: 103` as deletions is exactly the wrong conclusion.
    """
    from datetime import UTC, datetime

    from ironrag_connector.orchestrator import OrchestrationOutcome
    from ironrag_connector.sync import SyncReport, _tally

    now = datetime.now(tz=UTC)
    report = SyncReport(started_at=now, finished_at=now)
    ref = SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token="v1",
        routing_facts={},
        raw={},
    )

    def outcome(action: str, *, deferred: bool = False) -> OrchestrationOutcome:
        return OrchestrationOutcome(
            ref=ref,
            action=action,  # type: ignore[arg-type]
            workspace_id=None,
            library_id=None,
            rule_description=None,
            ironrag_document_id=None,
            detail="",
            deferred=deferred,
        )

    _tally(report, outcome("skipped_missing"))
    _tally(report, outcome("source_absent"))
    _tally(report, outcome("source_absent"))
    _tally(report, outcome("fetch_deferred", deferred=True))

    assert report.skipped_missing == 1
    assert report.skipped_empty == 2
    assert report.deferred == 1
    assert report.as_dict()["skipped_empty"] == 2
    assert report.as_dict()["deferred"] == 1


def test_report_counts_limited_visibility_deletes_separately_from_reaps() -> None:
    """A visibility-forced delete is neither a reap nor an empty source.

    `reaped` counts every applied absence and `skipped_empty` counts a
    `source_absent` result, so on their own neither says why the bytes went.
    `skipped_limited_visibility` is counted from the decision's typed
    exclusion, independently of the action the delete produced; an ordinary
    `on_missing` reap leaves it at zero.
    """
    now = datetime.now(tz=UTC)
    report = SyncReport(started_at=now, finished_at=now)
    ref = SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token="v1",
        routing_facts={},
        raw={},
    )

    def outcome(
        action: OutcomeAction, *, exclusion: SourceExclusion = SourceExclusion.NONE
    ) -> OrchestrationOutcome:
        return OrchestrationOutcome(
            ref=ref,
            action=action,
            workspace_id=None,
            library_id=None,
            rule_description=None,
            ironrag_document_id=None,
            detail="",
            exclusion=exclusion,
        )

    _tally(report, outcome("deleted", exclusion=SourceExclusion.LIMITED_VISIBILITY))
    _tally(report, outcome("source_absent", exclusion=SourceExclusion.LIMITED_VISIBILITY))
    _tally(report, outcome("deleted"))

    assert report.reaped == 2
    assert report.skipped_empty == 1
    assert report.skipped_limited_visibility == 2
    assert report.as_dict()["skipped_limited_visibility"] == 2


class _EmptyAdapter(EmptyBlockingAdapter):
    """Enumerates nothing, so a sweep is just the reaper."""

    def __init__(self) -> None:
        super().__init__(asyncio.Event(), asyncio.Event())

    async def iter_items(self) -> Any:
        return
        yield  # pragma: no cover -- makes this an async generator


class _MixedMissingEnforcementAdapter(_EmptyAdapter):
    kinds = ("ordinary", "security")
    primary_kinds = kinds

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind == "security":
            return SourceDeleteEnforcement.REQUIRED
        return SourceDeleteEnforcement.POLICY


class _RequiredMissingAdapter(_EmptyAdapter):
    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        return SourceDeleteEnforcement.REQUIRED


class _RequiredOneRefAdapter(OneRefAdapter):
    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        return SourceDeleteEnforcement.REQUIRED


class _FetchOnlyEchoAdapter(EchoAdapter):
    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        return
        yield  # pragma: no cover -- makes this an async generator


def _document(
    document_id: int,
    library_id: UUID,
    external_key: str,
) -> DocumentResource:
    return DocumentResource(
        id=UUID(int=document_id),
        library_id=library_id,
        external_key=external_key,
        status="ready",
    )


def _ignore_policies() -> PolicyOverrides:
    return PolicyOverrides(
        default=PushPolicy(on_missing=DeleteAction.IGNORE),
        by_kind={},
    )


def _create_legacy_cursor_database(
    path: Path,
    *,
    item_id: str,
    external_key: str,
    document_id: str,
    library_id: UUID,
) -> None:
    connection = sqlite3.connect(path)
    try:
        connection.executescript(
            """
            CREATE TABLE cursor (
                kind TEXT NOT NULL,
                item_id TEXT NOT NULL,
                change_token TEXT,
                external_key TEXT NOT NULL,
                ironrag_document_id TEXT,
                ironrag_library_id TEXT NOT NULL,
                last_pushed_at TEXT NOT NULL,
                PRIMARY KEY (kind, item_id)
            );
            """
        )
        connection.execute(
            "INSERT INTO cursor VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                "page",
                item_id,
                "v1",
                external_key,
                document_id,
                str(library_id),
                "2026-01-01T00:00:00+00:00",
            ),
        )
        connection.commit()
    finally:
        connection.close()


@pytest.mark.asyncio
async def test_complete_sweep_reaps_required_missing_kind_despite_ignore_policy(
    tmp_path: Path,
) -> None:
    ironrag = ListingIronRag(
        {
            LIB: (
                _document(1, LIB, "blocking:ordinary:gone"),
                _document(2, LIB, "blocking:security:gone"),
            )
        }
    )
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=_MixedMissingEnforcementAdapter(),
        orchestrator=orchestrator,
        ironrag=ironrag,
        policies=_ignore_policies(),
    )

    report = await manager.run_once(reason="complete-census")

    assert ironrag.list_calls == [(LIB, "blocking:security:")]
    assert [
        (call[0].kind, call[0].item_id, call[1], call[3]) for call in orchestrator.reap_calls
    ] == [("security", "gone", LIB, SourceDeleteEnforcement.REQUIRED)]
    assert report.reaped == 1


@pytest.mark.asyncio
async def test_required_missing_reaper_recovers_historical_route_after_restart(
    tmp_path: Path,
) -> None:
    state_path = tmp_path / "state.sqlite"
    first_state = StateStore(state_path)
    first_state.upsert(
        kind="page",
        item_id="gone",
        change_token="v1",
        external_key="blocking:page:gone",
        ironrag_document_id=str(UUID(int=2)),
        ironrag_library_id=str(LIB_HISTORICAL),
    )
    first_state.close()

    ironrag = ListingIronRag(
        {
            LIB: (_document(1, LIB, "blocking:page:gone"),),
            LIB_HISTORICAL: (_document(2, LIB_HISTORICAL, "blocking:page:gone"),),
        }
    )
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=_RequiredMissingAdapter(),
        orchestrator=orchestrator,
        ironrag=ironrag,
        policies=_ignore_policies(),
    )

    report = await manager.run_once(reason="after-restart")

    assert orchestrator.pre_sweep_libraries == {"page": {LIB_HISTORICAL}}
    assert {library_id for library_id, _ in ironrag.list_calls} == {
        LIB,
        LIB_HISTORICAL,
    }
    assert {(call[1], call[2], call[3]) for call in orchestrator.reap_calls} == {
        (LIB, str(UUID(int=1)), SourceDeleteEnforcement.REQUIRED),
        (LIB_HISTORICAL, str(UUID(int=2)), SourceDeleteEnforcement.REQUIRED),
    }
    assert report.reaped == 2


@pytest.mark.asyncio
async def test_migrated_cursor_scope_reaps_source_missing_historical_exact_key(
    tmp_path: Path,
) -> None:
    legacy_document_id = str(UUID(int=90))
    state_path = tmp_path / "state.sqlite"
    _create_legacy_cursor_database(
        state_path,
        item_id="gone",
        external_key="blocking:page:gone",
        document_id=legacy_document_id,
        library_id=LIB_HISTORICAL,
    )
    state = StateStore(state_path)
    assert state.locations_for("page", "gone") == []

    ironrag = ListingIronRag(
        {
            LIB_HISTORICAL: (
                _document(2, LIB_HISTORICAL, "blocking:page:gone"),
                _document(90, LIB_HISTORICAL, "foreign:page:duplicate"),
            )
        }
    )
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=_RequiredMissingAdapter(),
        orchestrator=orchestrator,
        ironrag=ironrag,
        policies=_ignore_policies(),
        state=state,
    )

    report = await manager.run_once(reason="migrated-source-missing")

    assert orchestrator.pre_sweep_libraries == {"page": {LIB_HISTORICAL}}
    assert [(call[1], call[2]) for call in orchestrator.reap_calls] == [
        (LIB_HISTORICAL, str(UUID(int=2)))
    ]
    assert all(call[2] != legacy_document_id for call in orchestrator.reap_calls)
    assert report.reaped == 1


@pytest.mark.asyncio
async def test_migrated_cursor_scope_survives_route_move_and_reaps_old_exact_key(
    tmp_path: Path,
) -> None:
    legacy_document_id = str(UUID(int=90))
    state_path = tmp_path / "state.sqlite"
    _create_legacy_cursor_database(
        state_path,
        item_id="1",
        external_key="blocking:page:1",
        document_id=legacy_document_id,
        library_id=LIB_HISTORICAL,
    )
    state = StateStore(state_path)
    assert state.locations_for("page", "1") == []

    ironrag = ListingIronRag(
        {
            LIB: (_document(1, LIB, "blocking:page:1"),),
            LIB_HISTORICAL: (
                _document(2, LIB_HISTORICAL, "blocking:page:1"),
                _document(90, LIB_HISTORICAL, "foreign:page:duplicate"),
            ),
        }
    )
    orchestrator = RouteMovingOrchestrator(state)
    manager = _manager(
        tmp_path,
        adapter=_RequiredOneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=orchestrator,
        ironrag=ironrag,
        policies=_ignore_policies(),
        state=state,
    )

    report = await manager.run_once(reason="migrated-route-move")

    assert orchestrator.pre_sweep_libraries == {"page": {LIB_HISTORICAL}}
    assert {library_id for library_id, _ in ironrag.list_calls} == {
        LIB,
        LIB_HISTORICAL,
    }
    assert [(call[1], call[2]) for call in orchestrator.reap_calls] == [
        (LIB_HISTORICAL, str(UUID(int=2)))
    ]
    assert all(call[2] != legacy_document_id for call in orchestrator.reap_calls)
    current = state.get("page", "1")
    assert current is not None
    assert current.current_library_id == str(LIB)
    assert report.reaped == 1


@pytest.mark.parametrize(
    ("enforcement", "on_missing", "state_remains"),
    [
        (SourceDeleteEnforcement.POLICY, DeleteAction.IGNORE, True),
        (SourceDeleteEnforcement.POLICY, DeleteAction.DELETE, False),
        (SourceDeleteEnforcement.REQUIRED, DeleteAction.IGNORE, False),
    ],
    ids=("policy-ignore", "policy-delete", "required-ignore"),
)
@pytest.mark.asyncio
async def test_successful_empty_reaper_scan_reconciles_only_enforced_state(
    tmp_path: Path,
    enforcement: SourceDeleteEnforcement,
    on_missing: DeleteAction,
    state_remains: bool,
) -> None:
    adapter = (
        _RequiredMissingAdapter()
        if enforcement is SourceDeleteEnforcement.REQUIRED
        else _EmptyAdapter()
    )
    policies = PolicyOverrides(default=PushPolicy(on_missing=on_missing), by_kind={})
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="gone",
        change_token="v1",
        external_key="blocking:page:gone",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    ironrag = ListingIronRag({})
    router = _router()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    manager = _manager(
        tmp_path,
        adapter=adapter,
        ironrag=ironrag,
        orchestrator=orchestrator,  # type: ignore[arg-type]
        state=state,
        policies=policies,
    )

    report = await manager.run_once(reason="empty-target")

    assert (state.get("page", "gone") is not None) is state_remains
    assert bool(state.locations_for("page", "gone")) is state_remains
    assert report.reaped == 0


@pytest.mark.parametrize("failure", ["timeout", "error"])
@pytest.mark.asyncio
async def test_incomplete_reaper_scan_retains_cursor_and_location(
    tmp_path: Path,
    failure: str,
) -> None:
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="gone",
        change_token="v1",
        external_key="blocking:page:gone",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    ironrag = SlowListIronRag() if failure == "timeout" else ErrorListIronRag()
    manager = _manager(
        tmp_path,
        adapter=_RequiredMissingAdapter(),
        orchestrator=NoopOrchestrator(),
        ironrag=ironrag,
        policies=_ignore_policies(),
        reaper_list_timeout_seconds=0.01,
        state=state,
    )

    await manager.run_once(reason=f"list-{failure}")

    assert state.get("page", "gone") is not None
    assert len(state.locations_for("page", "gone")) == 1


@pytest.mark.asyncio
async def test_empty_scan_cas_preserves_fresh_write_after_listing(tmp_path: Path) -> None:
    adapter = _EmptyAdapter()
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="gone",
        change_token="v1",
        external_key="blocking:page:gone",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    ironrag = ListingIronRag({})
    router = _router()
    orchestrator = RacingConfirmationOrchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=policies,
        concurrency=1,
        interval_seconds=60,
    )

    report = await manager.run_once(reason="racing-webhook")

    cursor = state.get("page", "gone")
    location = state.get_location("page", "gone", str(LIB))
    assert cursor is not None
    assert cursor.change_token == "v2"
    assert location is not None
    assert location.ironrag_document_id == str(UUID(int=2))
    assert report.reaped == 0
    assert report.deferred == 1


@pytest.mark.asyncio
async def test_returned_orphan_reap_defers_after_concurrent_push_ref(tmp_path: Path) -> None:
    adapter = _FetchOnlyEchoAdapter(
        {"1": EchoPage(item_id="1", title="One", body="fresh", updated_at="v2")}
    )
    policies = PolicyOverrides(
        default=PushPolicy(on_changed=UpdateAction.SKIP),
        by_kind={},
    )
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="1",
        change_token="v1",
        external_key="echo:page:1",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    ironrag = PausingListingIronRag({LIB: (_document(1, LIB, "echo:page:1"),)})
    router = _router()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    manager = _manager(
        tmp_path,
        adapter=adapter,
        ironrag=ironrag,
        orchestrator=orchestrator,  # type: ignore[arg-type]
        state=state,
        policies=policies,
    )

    sweep = asyncio.create_task(manager.run_once(reason="concurrent-webhook"))
    await asyncio.wait_for(ironrag.listed.wait(), timeout=1)
    pushed = await orchestrator.push_ref(
        SourceItemRef(
            item_id="1",
            kind="page",
            external_key="echo:page:1",
            change_token="v2",
        )
    )
    ironrag.resume.set()
    report = await asyncio.wait_for(sweep, timeout=1)

    cursor = state.get("page", "1")
    location = state.get_location("page", "1", str(LIB))
    assert pushed.action == "skipped_changed"
    assert cursor is not None
    assert cursor.change_token == "v2"
    assert location is not None
    assert location.ironrag_document_id == str(UUID(int=1))
    assert ironrag.delete_calls == []
    assert report.reaped == 0
    assert report.deferred == 1


@pytest.mark.asyncio
async def test_empty_historical_scan_clears_only_old_route_state(tmp_path: Path) -> None:
    adapter = OneRefAdapter(asyncio.Event(), asyncio.Event())
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="1",
        change_token="v1",
        external_key="blocking:page:1",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB_HISTORICAL),
    )
    state.upsert(
        kind="page",
        item_id="1",
        change_token="v2",
        external_key="blocking:page:1",
        ironrag_document_id=str(UUID(int=2)),
        ironrag_library_id=str(LIB),
    )
    assert {location.ironrag_library_id for location in state.locations_for("page", "1")} == {
        str(LIB),
        str(LIB_HISTORICAL),
    }
    ironrag = ListingIronRag({})
    router = _router()
    orchestrator = ExistingRouteOrchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=policies,
        concurrency=1,
        interval_seconds=60,
    )

    report = await manager.run_once(reason="historical-route-empty")

    cursor = state.get("page", "1")
    assert cursor is not None
    assert cursor.current_library_id == str(LIB)
    assert [location.ironrag_library_id for location in state.locations_for("page", "1")] == [
        str(LIB)
    ]
    assert report.reaped == 0
    assert report.deferred == 0


@pytest.mark.asyncio
async def test_empty_scan_preserves_seen_item_after_transient_push_failure(
    tmp_path: Path,
) -> None:
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="1",
        change_token="v1",
        external_key="blocking:page:1",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    orchestrator = ErrorPushOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=orchestrator,
        ironrag=ListingIronRag({}),
        state=state,
    )

    report = await manager.run_once(reason="transient-fetch-failure")

    assert report.errors == 1
    assert orchestrator.absent_calls == []
    assert state.get("page", "1") is not None
    assert len(state.locations_for("page", "1")) == 1


@pytest.mark.parametrize("listed", [False, True], ids=("empty", "listed-canonical"))
@pytest.mark.asyncio
async def test_reaper_does_not_mutate_noncanonical_persisted_identity(
    tmp_path: Path,
    listed: bool,
) -> None:
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="corrupt",
        change_token="v1",
        external_key="foreign:page:corrupt",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=_EmptyAdapter(),
        orchestrator=orchestrator,
        ironrag=ListingIronRag(
            {LIB: (_document(2, LIB, "blocking:page:corrupt"),)} if listed else {}
        ),
        state=state,
    )

    report = await manager.run_once(reason="noncanonical-state")

    assert report.errors == 1
    assert orchestrator.reap_calls == []
    assert orchestrator.absent_calls == []
    assert state.get("page", "corrupt") is not None
    assert len(state.locations_for("page", "corrupt")) == 1


@pytest.mark.asyncio
async def test_failed_document_reap_is_not_reclassified_as_confirmed_absence(
    tmp_path: Path,
) -> None:
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="gone",
        change_token="v1",
        external_key="blocking:page:gone",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    orchestrator = FailingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=_EmptyAdapter(),
        orchestrator=orchestrator,
        ironrag=ListingIronRag({LIB: (_document(1, LIB, "blocking:page:gone"),)}),
        state=state,
    )

    report = await manager.run_once(reason="delete-failure")

    assert report.errors == 1
    assert orchestrator.absent_calls == []
    assert state.get("page", "gone") is not None
    assert len(state.locations_for("page", "gone")) == 1


class DeletedLibraryIronRag(FakeIronRag):
    """Every listing raises 404: the library was deleted out from under us."""

    def __init__(self) -> None:
        self.list_calls = 0

    async def list_documents(self, *_: Any, **__: Any) -> AsyncIterator[DocumentResource]:
        self.list_calls += 1
        raise IronRagNotFoundError(
            ProblemDetails(
                type="urn:ironrag:error:not_found",
                title="Not Found",
                status=404,
                detail="library not found",
                code="not_found",
            )
        )
        yield  # pragma: no cover -- makes this an async generator


@pytest.mark.asyncio
async def test_a_deleted_library_stops_being_reaped_and_its_state_goes(
    tmp_path: Path,
) -> None:
    """A library that no longer exists must not be nominated forever.

    The reaper deliberately unions the router's targets with every library in
    the ownership registry, so content moved between libraries keeps its old
    copy discoverable. When the old library is deleted, retaining that state
    would turn the union into one 404 per sweep forever.
    """
    dead_library = UUID("019d97f8-90d6-7e10-9495-d31ddf8e4854")
    manager = _manager(
        tmp_path,
        adapter=_EmptyAdapter(),
        orchestrator=NoopOrchestrator(),
        ironrag=DeletedLibraryIronRag(),
    )
    state = manager._state
    for index in range(3):
        state.upsert(
            kind="page",
            item_id=str(index),
            change_token="v1",
            external_key=f"blocking:page:{index}",
            ironrag_document_id=f"doc-{index}",
            ironrag_library_id=str(dead_library),
        )
    assert len(state.items_of_kind("page")) == 3

    await manager.run_once(reason="test")

    assert state.items_of_kind("page") == [], (
        "current-route state for a deleted library must not survive"
    )
    assert state.locations_of_kind("page") == []


@pytest.mark.asyncio
async def test_confluence_malformed_census_aborts_before_reaping_or_state_mutation(
    tmp_path: Path,
) -> None:
    settings = ConfluenceSettings(
        confluence_base_url="https://wiki.example.com",
        confluence_auth_mode="anonymous",
        ironrag_base_url="http://ironrag.example.com",
        ironrag_api_token="token",
        admin_bearer_token="admin",
    )
    transport = httpx.MockTransport(lambda _: httpx.Response(200, json={"results": {"id": "42"}}))
    http_client = httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    )
    confluence_client = ConfluenceClient(settings, client=http_client)
    with patch(
        "ironrag_connector.sources.confluence.adapter.ConfluenceClient",
        return_value=confluence_client,
    ):
        adapter = ConfluenceAdapter(settings)

    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="42",
        change_token="v1",
        external_key="confluence:page:42",
        ironrag_document_id=str(UUID(int=1)),
        ironrag_library_id=str(LIB),
    )
    cursor_before = state.get("page", "42")
    locations_before = state.locations_for("page", "42")
    ironrag = PausingListingIronRag({LIB: (_document(1, LIB, "confluence:page:42"),)})
    ironrag.resume.set()
    router = _router()
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    manager = _manager(
        tmp_path,
        adapter=adapter,
        ironrag=ironrag,
        orchestrator=orchestrator,  # type: ignore[arg-type]
        state=state,
        policies=policies,
    )

    try:
        report = await manager.run_once(reason="malformed-confluence-census")
    finally:
        await adapter.close()
        await http_client.aclose()

    assert report.items_seen == 0
    assert report.errors == 1
    assert report.reaped == 0
    assert ironrag.list_calls == []
    assert ironrag.delete_calls == []
    assert state.get("page", "42") == cursor_before
    assert state.locations_for("page", "42") == locations_before


class _PartialCensusAdapter(_EmptyAdapter):
    """Yields one live item, then admits it could not enumerate the rest."""

    def __init__(self, failure: Exception) -> None:
        super().__init__()
        self._failure = failure

    async def iter_items(self) -> Any:
        yield SourceItemRef(item_id="1", kind="page", external_key="blocking:page:1")
        raise self._failure


class _OneRefAdapter(_EmptyAdapter):
    async def iter_items(self) -> Any:
        yield SourceItemRef(item_id="1", kind="page", external_key="blocking:page:1")


class _SlowRecordingOrchestrator(RecordingReaperOrchestrator):
    """Pushes complete only after the enumeration has already failed."""

    def __init__(self) -> None:
        super().__init__()
        self.pushed: list[str] = []

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        await asyncio.sleep(0.01)
        self.pushed.append(ref.item_id)
        return await super().push_ref(ref)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "failure",
    [
        pytest.param(IncompleteCensusError("one scope unreadable", scopes=("b",)), id="typed"),
        pytest.param(RuntimeError("listing exploded"), id="generic"),
    ],
)
async def test_incomplete_census_pushes_enumerated_items_but_never_reaps(
    tmp_path: Path,
    failure: Exception,
) -> None:
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="page",
        item_id="unseen",
        change_token="v1",
        external_key="blocking:page:unseen",
        ironrag_document_id=str(UUID(int=7)),
        ironrag_library_id=str(LIB),
    )
    cursor_before = state.get("page", "unseen")
    locations_before = state.locations_for("page", "unseen")
    ironrag = ListingIronRag({LIB: (_document(7, LIB, "blocking:page:unseen"),)})
    orchestrator = _SlowRecordingOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=_PartialCensusAdapter(failure),
        orchestrator=orchestrator,
        ironrag=ironrag,
        state=state,
    )

    report = await manager.run_once(reason="incomplete-census")

    assert orchestrator.pushed == ["1"], "enumerated items are pushed before the sweep returns"
    assert report.items_seen == 1
    assert report.errors == 1
    assert report.census_complete is False
    assert report.as_dict()["census_complete"] is False
    assert report.reaped == 0
    assert ironrag.list_calls == []
    assert orchestrator.reap_calls == []
    assert orchestrator.absent_calls == []
    assert state.get("page", "unseen") == cursor_before
    assert state.locations_for("page", "unseen") == locations_before
    assert not manager._run_lock.locked()


@pytest.mark.asyncio
async def test_complete_census_reports_completeness_and_reaps_unseen(tmp_path: Path) -> None:
    ironrag = ListingIronRag(
        {LIB: (_document(1, LIB, "blocking:page:1"), _document(7, LIB, "blocking:page:unseen"))}
    )
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path, adapter=_OneRefAdapter(), orchestrator=orchestrator, ironrag=ironrag
    )

    report = await manager.run_once(reason="complete-census")

    assert report.census_complete is True
    assert report.as_dict()["census_complete"] is True
    assert [call[0].item_id for call in orchestrator.reap_calls] == ["unseen"]
    assert report.reaped == 1


@pytest.mark.asyncio
async def test_cancellation_while_draining_after_incomplete_census_cancels_pushes(
    tmp_path: Path,
) -> None:
    started = asyncio.Event()
    cancelled = asyncio.Event()
    manager = _manager(
        tmp_path,
        adapter=_PartialCensusAdapter(IncompleteCensusError("scope unreadable")),
        orchestrator=BlockingOrchestrator(started, cancelled),
    )

    task = asyncio.create_task(manager.run_once(reason="cancel-while-draining"))
    await asyncio.wait_for(started.wait(), timeout=1)
    task.cancel()

    with pytest.raises(asyncio.CancelledError):
        await task
    await asyncio.wait_for(cancelled.wait(), timeout=1)
    assert not manager._run_lock.locked()


# --- dependent census: two-phase reaper, membership forgetting, gates ---

DOC_PAGE = UUID(int=1)
DOC_IMAGE = UUID(int=3)
DOC_STALE_IMAGE = UUID(int=4)
DOC_ATTACHMENT = UUID(int=5)


def _dependent_sweep(
    tmp_path: Path,
    adapter: DependentPageAdapter,
    *,
    state: StateStore,
    ironrag: RichIronRag,
    policies: PolicyOverrides | None = None,
    router: Router | None = None,
) -> tuple[Orchestrator, SyncManager]:
    resolved_policies = policies or PolicyOverrides(default=PushPolicy(), by_kind={})
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router or _router(),
        state=state,
        policies=resolved_policies,
    )
    manager = _manager(
        tmp_path,
        adapter=adapter,
        orchestrator=orchestrator,  # type: ignore[arg-type]
        ironrag=ironrag,
        state=state,
        policies=resolved_policies,
        router=router,
    )
    return orchestrator, manager


def _seed_legacy_page_with_images(
    state: StateStore, ironrag: RichIronRag, adapter: DependentPageAdapter
) -> None:
    """Pre-membership state: documents and cursors exist, claims do not."""
    seed_document(
        state,
        ironrag.documents,
        SourceItemRef(item_id="1", kind="page", external_key="echo:page:1", change_token="t1"),
        change_token="t1",
        document_id=DOC_PAGE,
    )
    seed_document(
        state,
        ironrag.documents,
        dependent_ref("image", "img-1", "i1"),
        change_token="i1",
        document_id=DOC_IMAGE,
    )
    seed_document(
        state,
        ironrag.documents,
        dependent_ref("image", "stale", "i0"),
        change_token="i0",
        document_id=DOC_STALE_IMAGE,
    )


class _ContentUnsupportedRevisionIronRag(RichIronRag):
    """Fails the polled revision of named external keys with the disposition the
    production client maps from ``body``.

    The failure is produced by the real client mapping, so the sync test proves
    the end-to-end disposition path rather than re-deriving it."""

    def __init__(self, *, unsupported_keys: set[str], body: dict[str, object]) -> None:
        super().__init__()
        self._unsupported_keys = unsupported_keys
        self._failure_body = body
        self._operation_keys: dict[UUID, str] = {}

    async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> Any:
        handle = await super().create_revision(document_id, **kwargs)
        key = next(
            (
                stored[1]
                for stored, doc in self.documents.items()
                if str(doc.id) == str(document_id)
            ),
            "",
        )
        self._operation_keys[handle.operation_id] = key
        return handle

    async def wait_for_operation(self, operation_id: UUID | str, **kwargs: Any) -> OperationStatus:
        if self._operation_keys.get(UUID(str(operation_id))) in self._unsupported_keys:
            raise await _operation_failure({**self._failure_body, "id": str(operation_id)})
        return await super().wait_for_operation(operation_id, **kwargs)


@pytest.mark.asyncio
@pytest.mark.parametrize("with_dependent", [False, True])
@pytest.mark.parametrize("legacy_claim", [False, True])
async def test_policy_primary_failure_settles_census_across_restart_and_successful_retry(
    tmp_path: Path, with_dependent: bool, legacy_claim: bool
) -> None:
    specs = (DependentSpec("image", "img-1", "i2"),) if with_dependent else ()
    adapter = DependentPageAdapter({"1": echo_page("1", "t2")}, {"1": specs})
    state_path = tmp_path / "state.sqlite"
    state = StateStore(state_path)
    server = PolicyFailureIronRag("echo:page:1")
    _seed_legacy_page_with_images(state, server, adapter)
    if legacy_claim:
        state.claim_member(
            parent_kind="page",
            parent_item_id="1",
            member=MemberTarget(
                kind="image",
                item_id="img-1",
                external_key="echo:image:img-1",
                ironrag_library_id=str(LIB),
            ),
        )
    for attempt in range(2):
        _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=server)
        report = await manager.run_once(reason=f"failed-primary-{attempt}")
        assert report.errors == 1
        assert report.skipped_unsupported == 0
        assert report.dependent_census_complete is True
        assert state.get("page", "1").change_token == "t1"
        assert state.parent_census("page", "1").change_token == "t2"
        assert state.has_claims("image", "img-1", str(LIB)) is with_dependent
        assert ((LIB, "echo:image:img-1") in server.documents) is with_dependent
        assert (LIB, "echo:image:stale") not in server.documents
        if with_dependent:
            assert report.replaced == (1 if attempt == 0 else 0)
        state.close()
        state = StateStore(state_path)

    failed_ids = set(server.failures)
    assert len(failed_ids) == 2
    server.failed_keys.clear()
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=server)
    report = await manager.run_once(reason="primary-retry-ready")
    assert report.errors == 0
    assert report.replaced == 1
    assert report.dependent_census_complete is True
    assert state.get("page", "1").change_token == "t2"
    assert all(server._operations[op].status.value == "failed" for op in failed_ids)


@pytest.mark.asyncio
@pytest.mark.parametrize("dependent_failure", ["error", "defer"])
async def test_primary_failure_counts_completed_dependents_but_incomplete_phase_stays_closed(
    tmp_path: Path, dependent_failure: str
) -> None:
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t2")},
        {"1": (DependentSpec("image", "new", "i1"), DependentSpec("image", "img-1", "i2"))},
    )
    state = StateStore(tmp_path / "state.sqlite")
    server = PolicyFailureIronRag("echo:page:1")
    _seed_legacy_page_with_images(state, server, adapter)
    if dependent_failure == "error":
        server.failed_keys.add("echo:image:img-1")
    else:
        server.revision_conflicts.add(str(DOC_IMAGE))
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=server)

    report = await manager.run_once(reason="failed-primary-incomplete-dependent")

    assert report.errors == (2 if dependent_failure == "error" else 1)
    assert report.created == 1
    assert report.dependent_census_complete is False
    assert state.parent_census("page", "1") is None
    assert (LIB, "echo:image:stale") in server.documents
    assert state.get("page", "1").change_token == "t1"


@pytest.mark.asyncio
@pytest.mark.parametrize("primary_result", ["ready", "failed", "deleted"])
async def test_released_shared_copy_waits_for_legacy_owner_census(
    tmp_path: Path, primary_result: str
) -> None:
    """An absent claim is not orphan proof while a legacy owner is unproven."""

    class SharedOwnerAdapter(DependentPageAdapter):
        async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
            if ref.item_id == "1" and primary_result == "deleted":
                return SourceFetchDelete(SourceDeleteEnforcement.POLICY, "parent deleted")
            return await super().fetch(ref)

    primary_failure = primary_result == "failed"
    shared = DependentSpec("image", "img-1", "i1")
    adapter = SharedOwnerAdapter(
        {"1": echo_page("1", "t2"), "2": echo_page("2", "t1")},
        {"1": (), "2": (shared,)},
    )
    adapter.deferred_pages.add("2")
    state = StateStore(tmp_path / "state.sqlite")
    server = PolicyFailureIronRag(*(["echo:page:1"] if primary_failure else []))
    _seed_legacy_page_with_images(state, server, adapter)
    seed_document(
        state,
        server.documents,
        SourceItemRef(item_id="2", kind="page", external_key="echo:page:2"),
        change_token="t1",
        document_id=UUID(int=2),
    )
    state.claim_member(
        parent_kind="page",
        parent_item_id="1",
        member=MemberTarget(
            kind="image",
            item_id="img-1",
            external_key="echo:image:img-1",
            ironrag_library_id=str(LIB),
        ),
    )
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=server)

    blocked = await manager.run_once(reason="legacy-owner-deferred")

    assert blocked.errors == int(primary_failure)
    assert blocked.dependent_census_complete is False
    assert (state.parent_census("page", "1") is not None) == (primary_result != "deleted")
    assert state.claims_of_parent("page", "1") == []
    assert state.parent_census("page", "2") is None
    assert state.has_claims("image", "img-1", str(LIB)) is False
    assert (LIB, "echo:image:img-1") in server.documents
    assert str(DOC_IMAGE) not in server.deletes
    assert (LIB, "echo:image:stale") in server.documents

    adapter.deferred_pages.clear()
    proven = await manager.run_once(reason="legacy-owner-proven")
    assert proven.errors == int(primary_failure)
    assert proven.dependent_census_complete is True
    assert state.has_claims("image", "img-1", str(LIB))
    assert (LIB, "echo:image:img-1") in server.documents
    assert str(DOC_IMAGE) not in server.deletes
    assert (LIB, "echo:image:stale") not in server.documents

    adapter.set_dependents("2")
    orphaned = await manager.run_once(reason="both-owners-omit-copy")
    assert orphaned.errors == int(primary_failure)
    assert orphaned.dependent_census_complete is True
    assert state.has_claims("image", "img-1", str(LIB)) is False
    assert (LIB, "echo:image:img-1") not in server.documents
    assert str(DOC_IMAGE) in server.deletes


def _content_unsupported_client(*keys: str) -> _ContentUnsupportedRevisionIronRag:
    """A production-client double that proves the named keys unsupported."""
    return _ContentUnsupportedRevisionIronRag(
        unsupported_keys=set(keys),
        body=_failed_operation_body(
            failureCode="upload_extraction_failed",
            failureDisposition="content_unsupported",
        ),
    )


@pytest.mark.asyncio
async def test_unsupported_dependent_is_terminally_accounted_and_its_copy_kept(
    tmp_path: Path,
) -> None:
    """A server-proven unsupported dependent must not withhold the census.

    The claim is recorded before the push, the dependent is terminally
    accounted in the parent's census (claimed, not materialized), and its legacy
    copy is kept -- the dependent phase must not reap a copy a live parent still
    enumerates."""
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")},
        {"1": (DependentSpec("image", "img-1", "i2"),)},
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = _content_unsupported_client("echo:image:img-1")
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="unsupported-dependent")

    assert report.errors == 0
    assert report.skipped_unsupported == 1
    assert report.dependent_census_complete is True
    assert state.parent_census("page", "1") is not None
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert (LIB, "echo:image:img-1") in ironrag.documents
    assert str(DOC_IMAGE) not in ironrag.deletes
    assert str(DOC_STALE_IMAGE) in ironrag.deletes


@pytest.mark.asyncio
async def test_operational_dependent_failure_withholds_the_census(tmp_path: Path) -> None:
    """A dependent failure the server did not prove is a content rejection is
    an error: the parent gets no census and the dependent phase is withheld."""
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")},
        {"1": (DependentSpec("image", "img-1", "i2"),)},
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = _ContentUnsupportedRevisionIronRag(
        unsupported_keys={"echo:image:img-1"},
        body=_failed_operation_body(
            failureCode="upload_extraction_failed",
            failureDisposition="retry_exhausted",
        ),
    )
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="operational-dependent")

    assert report.errors == 1
    assert report.skipped_unsupported == 0
    assert report.dependent_census_complete is False
    assert state.parent_census("page", "1") is None
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert (LIB, "echo:image:img-1") in ironrag.documents


@pytest.mark.asyncio
async def test_unsupported_primary_keeps_its_owning_dependents_unproven(
    tmp_path: Path,
) -> None:
    """A skipped primary is not a materialized parent.

    It stays enumerated and still owns the claim a previous pass recorded, so
    the dependent phase must remain withheld until a real census is written."""
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t2")},
        {"1": (DependentSpec("image", "img-1", "i1"),)},
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = _ContentUnsupportedRevisionIronRag(
        unsupported_keys={"echo:page:1"},
        body=_failed_operation_body(
            failureCode="upload_extraction_failed",
            failureDisposition="content_unsupported",
        ),
    )
    seed_document(
        state,
        ironrag.documents,
        SourceItemRef(item_id="1", kind="page", external_key="echo:page:1", change_token="t1"),
        change_token="t1",
        document_id=DOC_PAGE,
    )
    seed_document(
        state,
        ironrag.documents,
        dependent_ref("image", "img-1", "i1"),
        change_token="i1",
        document_id=DOC_IMAGE,
    )
    # A prior pass claimed img-1 before its push died: page 1 owns a copy but
    # holds no census row.
    state.claim_member(
        parent_kind="page",
        parent_item_id="1",
        member=MemberTarget(
            kind="image",
            item_id="img-1",
            external_key="echo:image:img-1",
            ironrag_library_id=str(LIB),
        ),
    )
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="unsupported-primary")

    assert report.skipped_unsupported == 1
    assert report.errors == 0
    assert report.dependent_census_complete is False
    assert state.parent_census("page", "1") is None
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert (LIB, "echo:image:img-1") in ironrag.documents


@pytest.mark.asyncio
async def test_unsupported_revision_of_a_proven_parent_keeps_its_dependent_census(
    tmp_path: Path,
) -> None:
    """A proven parent whose new revision is rejected invents no proof.

    Page 1 already holds a census and a claimed, materialized image. Its
    revision then moves and the server proves the new bytes unsupported. The
    failed primary must not settle a fresh census for a token it never applied,
    and the copy its existing census legitimately covers must survive the
    dependent reaper."""
    specs = (DependentSpec("image", "img-1", "i1"),)
    adapter = DependentPageAdapter({"1": echo_page("1", "t2")})
    adapter.set_dependents("1", *specs)
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = _content_unsupported_client("echo:page:1")
    _seed_legacy_page_with_images(state, ironrag, adapter)
    # The preexisting proof: page 1's complete dependent set at t1, and the
    # claim on img-1 that came with it.
    state.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t1",
        dependents_token=dependents_fingerprint(specs),
        members=(
            MemberTarget(
                kind="image",
                item_id="img-1",
                external_key="echo:image:img-1",
                ironrag_library_id=str(LIB),
            ),
        ),
    )
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="unsupported-revision")

    assert report.skipped_unsupported == 1
    assert report.errors == 0
    # The failure wrote no census of its own: the t1 proof is the only one, and
    # no proof exists for the rejected t2 revision.
    census = state.parent_census("page", "1")
    assert census is not None
    assert census.change_token == "t1"
    assert census.dependents_token == dependents_fingerprint(specs)
    # The census legitimately permits the dependent phase, which keeps the
    # claimed copy and still reaps the copy no parent claims.
    assert report.dependent_census_complete is True
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert (LIB, "echo:image:img-1") in ironrag.documents
    assert str(DOC_IMAGE) not in ironrag.deletes
    assert str(DOC_PAGE) not in ironrag.deletes
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]


@pytest.mark.asyncio
async def test_repeated_unsupported_dependent_keeps_claim_census_and_copy(
    tmp_path: Path,
) -> None:
    """Two rejected sweeps of one dependent neither drift nor overcount.

    ``bounded=False`` makes the adapter unable to bound its dependent drift, so
    the proven census can never be matched and the parent is re-fetched -- and
    the dependent re-pushed -- on both sweeps. Each pass must count exactly one
    ``skipped_unsupported``, keep the claim and the census, and leave the copy
    alone: repetition must not invent a reaper delete or turn the skip into an
    error."""
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")},
        {"1": (DependentSpec("image", "img-1", "i2"),)},
        bounded=False,
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = _content_unsupported_client("echo:image:img-1")
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    first = await manager.run_once(reason="unsupported-dependent-1")
    second = await manager.run_once(reason="unsupported-dependent-2")

    for report in (first, second):
        assert report.skipped_unsupported == 1
        assert report.errors == 0
        assert report.dependent_census_complete is True
        assert state.parent_census("page", "1") is not None
        assert state.has_claims("image", "img-1", str(LIB)) is True
        assert (LIB, "echo:image:img-1") in ironrag.documents
        assert str(DOC_IMAGE) not in ironrag.deletes
    # Only the copy no parent claims was reaped, and only once.
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]


@pytest.mark.asyncio
async def test_first_sweep_proves_parents_then_reaps_unclaimed_dependents_and_restart_is_quiet(
    tmp_path: Path,
) -> None:
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")}, {"1": (DependentSpec("image", "img-1", "i1"),)}
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="upgrade")

    assert report.census_complete is True
    assert report.dependent_census_complete is True
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]
    assert (LIB, "echo:image:img-1") in ironrag.documents
    assert report.reaped == 1
    assert ironrag.revisions == []
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert state.get("image", "stale") is None
    state.close()

    reopened = StateStore(tmp_path / "state.sqlite")
    adapter.fetch_calls.clear()
    _, restarted = _dependent_sweep(tmp_path, adapter, state=reopened, ironrag=ironrag)
    quiet = await restarted.run_once(reason="restart")

    assert adapter.fetch_calls == [], "a proven parent is not refetched after restart"
    assert quiet.dependent_census_complete is True
    assert quiet.reaped == 0
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]
    assert ironrag.creates == []


@pytest.mark.asyncio
async def test_unproven_live_parent_withholds_the_dependent_phase(tmp_path: Path) -> None:
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")}, {"1": (DependentSpec("image", "img-1", "i1"),)}
    )
    adapter.deferred_pages = {"1"}
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="deferred-parent")

    assert report.census_complete is True
    assert report.dependent_census_complete is False
    assert report.as_dict()["dependent_census_complete"] is False
    assert report.deferred == 1
    assert ironrag.deletes == []
    assert (LIB, "echo:image:stale") in ironrag.documents
    assert state.get("image", "stale") is not None


@pytest.mark.asyncio
async def test_an_enumerated_item_this_connector_never_pushed_does_not_withhold_the_phase(
    tmp_path: Path,
) -> None:
    """A never-pushed item owns no copies, so it must not stall the phase.

    Page 9 is enumerated but has no route, so it never materializes and never
    gains a census. Withholding the dependent phase on it would leave the
    orphan image unreapable for as long as the source keeps listing page 9.
    """
    library_ref = "tests/default-library"
    tagged_router = Router(
        RoutingConfig.model_validate(
            {"rules": [{"match": {"tag": "keep"}, "target": {"library": library_ref}}]}
        ),
        resolved_targets={
            library_ref: ResolvedLibraryTarget(
                library_ref=library_ref, workspace_id=WS, library_id=LIB
            )
        },
    )
    adapter = DependentPageAdapter(
        {
            "1": EchoPage(item_id="1", title="Page 1", body="hello", updated_at="t1", tag="keep"),
            "9": EchoPage(item_id="9", title="Page 9", body="hello", updated_at="t9"),
        },
        {"1": (DependentSpec("image", "img-1", "i1"),)},
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    orphan = dependent_ref("image", "stale", "i0")
    seed_document(state, ironrag.documents, orphan, change_token="i0", document_id=DOC_STALE_IMAGE)
    _, manager = _dependent_sweep(
        tmp_path, adapter, state=state, ironrag=ironrag, router=tagged_router
    )

    report = await manager.run_once(reason="unrouted-page")

    assert report.census_complete is True
    assert report.dependent_census_complete is True
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]
    assert state.parent_census("page", "9") is None


@pytest.mark.asyncio
async def test_a_vanished_parent_with_claims_but_no_census_releases_them(tmp_path: Path) -> None:
    """A fetch that died between the first claim and the census still cleans up.

    The claim is written before the dependent's push, so a parent can hold
    claims with no census row. When such a parent leaves the source, forgetting
    only the proven parents would keep its copies claimed -- and therefore
    unreapable -- for good.
    """
    adapter = DependentPageAdapter({})
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    attachment = dependent_ref("attachment", "att-2", "a1")
    seed_document(
        state, ironrag.documents, attachment, change_token="a1", document_id=DOC_ATTACHMENT
    )
    state.claim_member(
        parent_kind="page",
        parent_item_id="2",
        member=MemberTarget("attachment", "att-2", attachment.external_key, str(LIB)),
    )
    assert state.parent_census("page", "2") is None
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="claim-only-parent-gone")

    assert report.dependent_census_complete is True
    assert state.has_claims("attachment", "att-2", str(LIB)) is False
    assert state.known_parents("page") == set()
    assert ironrag.deletes == [str(DOC_ATTACHMENT)]


def _proven_parent_absent_from_source(tmp_path: Path) -> tuple[StateStore, RichIronRag]:
    """Page 2 with a proven attachment, both materialized; the source no longer lists it."""
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    seed_document(
        state,
        ironrag.documents,
        SourceItemRef(item_id="2", kind="page", external_key="echo:page:2", change_token="t1"),
        change_token="t1",
        document_id=DOC_PAGE,
    )
    attachment = dependent_ref("attachment", "att-2", "a1")
    seed_document(
        state, ironrag.documents, attachment, change_token="a1", document_id=DOC_ATTACHMENT
    )
    state.record_parent_census(
        kind="page",
        item_id="2",
        change_token="t1",
        dependents_token="d1",
        members=(MemberTarget("attachment", "att-2", attachment.external_key, str(LIB)),),
    )
    return state, ironrag


@pytest.mark.asyncio
async def test_parent_absent_from_complete_census_is_forgotten_and_dependents_reaped(
    tmp_path: Path,
) -> None:
    adapter = DependentPageAdapter({})
    state, ironrag = _proven_parent_absent_from_source(tmp_path)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="parent-gone")

    assert report.dependent_census_complete is True
    assert sorted(ironrag.deletes) == sorted([str(DOC_PAGE), str(DOC_ATTACHMENT)])
    assert report.reaped == 2
    assert state.parent_census("page", "2") is None
    assert state.has_claims("attachment", "att-2", str(LIB)) is False
    assert state.locations_for("attachment", "att-2") == []


@pytest.mark.asyncio
async def test_parent_retained_by_ignore_policy_keeps_membership_and_dependents(
    tmp_path: Path,
) -> None:
    adapter = DependentPageAdapter({})
    state, ironrag = _proven_parent_absent_from_source(tmp_path)
    policies = PolicyOverrides(
        default=PushPolicy(), by_kind={"page": PushPolicy(on_missing=DeleteAction.IGNORE)}
    )
    _, manager = _dependent_sweep(
        tmp_path, adapter, state=state, ironrag=ironrag, policies=policies
    )

    report = await manager.run_once(reason="parent-retained")

    assert report.reaped == 0
    assert ironrag.deletes == []
    assert state.parent_census("page", "2") is not None
    assert state.has_claims("attachment", "att-2", str(LIB)) is True


@pytest.mark.asyncio
async def test_incomplete_census_never_reaps_dependents(tmp_path: Path) -> None:
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")}, {"1": (DependentSpec("image", "img-1", "i1"),)}
    )
    adapter.census_failure = IncompleteCensusError("one listing page unreadable")
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    report = await manager.run_once(reason="incomplete")

    assert report.census_complete is False
    assert report.dependent_census_complete is False
    assert ironrag.deletes == []
    assert state.has_claims("image", "img-1", str(LIB)) is True, "pushes still claim"


def test_pre_sweep_snapshot_covers_dependent_kinds(tmp_path: Path) -> None:
    adapter = DependentPageAdapter({})
    state = StateStore(tmp_path / "state.sqlite")
    state.upsert(
        kind="image",
        item_id="img-1",
        change_token="i1",
        external_key="echo:image:img-1",
        ironrag_document_id=str(DOC_IMAGE),
        ironrag_library_id=str(LIB_HISTORICAL),
    )

    scope, evidence = _snapshot_reap_state(adapter, state)

    assert scope["image"] == {LIB_HISTORICAL}
    assert [e.item_id for e in evidence[("image", LIB_HISTORICAL)]] == ["img-1"]


def _proven_parent_state(tmp_path: Path) -> StateStore:
    state = StateStore(tmp_path / "state.sqlite")
    state.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t1",
        dependents_token="d1",
        members=(MemberTarget("image", "img-1", "echo:image:img-1", str(LIB)),),
    )
    return state


def _dependent_listing() -> ListingIronRag:
    return ListingIronRag(
        {
            LIB: (
                _document(1, LIB, "echo:page:1"),
                _document(3, LIB, "echo:image:img-1"),
                _document(4, LIB, "echo:image:stale"),
            )
        }
    )


@pytest.mark.asyncio
async def test_dependent_phase_reaps_only_unclaimed_listed_documents(tmp_path: Path) -> None:
    adapter = DependentPageAdapter({"1": echo_page("1", "t1")})
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=adapter,
        orchestrator=orchestrator,
        ironrag=_dependent_listing(),
        state=_proven_parent_state(tmp_path),
    )

    report = await manager.run_once(reason="dependent-phase")

    assert report.dependent_census_complete is True
    assert [
        (ref.kind, ref.item_id, enforcement) for ref, _, _, enforcement in orchestrator.reap_calls
    ] == [("image", "stale", SourceDeleteEnforcement.POLICY)]
    assert orchestrator.pre_sweep_libraries == {"page": set(), "image": set(), "attachment": set()}


@pytest.mark.asyncio
async def test_dependent_phase_respects_ignore_policy_for_the_dependent_kind(
    tmp_path: Path,
) -> None:
    adapter = DependentPageAdapter({"1": echo_page("1", "t1")})
    orchestrator = RecordingReaperOrchestrator()
    manager = _manager(
        tmp_path,
        adapter=adapter,
        orchestrator=orchestrator,
        ironrag=_dependent_listing(),
        state=_proven_parent_state(tmp_path),
        policies=PolicyOverrides(
            default=PushPolicy(), by_kind={"image": PushPolicy(on_missing=DeleteAction.IGNORE)}
        ),
    )

    report = await manager.run_once(reason="ignore-images")

    assert report.dependent_census_complete is True
    assert orchestrator.reap_calls == []


@pytest.mark.asyncio
async def test_dependent_census_flag_follows_primary_census_without_dependent_kinds(
    tmp_path: Path,
) -> None:
    ironrag = ListingIronRag({LIB: (_document(1, LIB, "blocking:page:1"),)})
    manager = _manager(
        tmp_path,
        adapter=_OneRefAdapter(),
        orchestrator=RecordingReaperOrchestrator(),
        ironrag=ironrag,
    )

    report = await manager.run_once(reason="no-dependent-kinds")

    assert report.census_complete is True
    assert report.dependent_census_complete is True
    assert report.as_dict()["dependent_census_complete"] is True


@pytest.mark.asyncio
async def test_route_move_leaves_no_orphan_dependent_in_the_old_library(tmp_path: Path) -> None:
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")}, {"1": (DependentSpec("image", "img-1", "i1"),)}
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = RichIronRag()
    _, before = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)
    await before.run_once(reason="initial")
    assert {key for key in ironrag.documents} == {(LIB, "echo:page:1"), (LIB, "echo:image:img-1")}

    _, moved = _dependent_sweep(
        tmp_path, adapter, state=state, ironrag=ironrag, router=router_to(LIB2)
    )
    report = await moved.run_once(reason="route-move")

    assert report.dependent_census_complete is True
    assert {key for key in ironrag.documents} == {(LIB2, "echo:page:1"), (LIB2, "echo:image:img-1")}
    assert state.has_claims("image", "img-1", str(LIB)) is False
    assert state.has_claims("image", "img-1", str(LIB2)) is True
    assert state.locations_for("image", "img-1") == [
        state.get_location("image", "img-1", str(LIB2))
    ]

    _, settled = _dependent_sweep(
        tmp_path, adapter, state=state, ironrag=ironrag, router=router_to(LIB2)
    )
    quiet = await settled.run_once(reason="settled")
    assert quiet.reaped == 0
    assert quiet.deferred == 0
    assert quiet.errors == 0


# --- IRG-188: synchronous HTTP 400 content_unsupported admission ---


def _problem_body(**extensions: object) -> dict[str, object]:
    return {
        "type": "urn:ironrag:error:unsupported",
        "title": "Unsupported",
        "status": 400,
        "detail": "synthetic admission rejection",
        "code": "unsupported_upload_type",
        **extensions,
    }


async def _http_problem_failure(body: dict[str, object], *, status: int = 400) -> BaseException:
    """The exception the production client maps from this HTTP problem body."""
    client = _problem_client(body, status=status)
    try:
        await client.create_document(
            LIB,
            external_key="probe",
            file_bytes=b"\x00\xff",
            file_name="probe.bin",
            mime_type="application/octet-stream",
        )
    except BaseException as exc:
        return exc
    finally:
        await client.aclose()
    raise AssertionError("the problem body did not raise")


@pytest.mark.asyncio
async def test_http_400_content_unsupported_primary_skip_survives_a_second_sweep(
    tmp_path: Path,
) -> None:
    exc = await _http_problem_failure(_problem_body(failureDisposition="content_unsupported"))
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    first = await manager.run_once(reason="http-400-content-unsupported-1")
    second = await manager.run_once(reason="http-400-content-unsupported-2")

    for report in (first, second):
        assert report.items_seen == 1
        assert report.skipped_unsupported == 1
        assert report.errors == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("status", "extensions"),
    [
        (400, {}),
        (400, {"failureDisposition": "retry_exhausted"}),
        (400, {"failureDisposition": "operational_terminal"}),
        (400, {"failureDisposition": "future_server_value"}),
        (429, {"failureDisposition": "content_unsupported"}),
        (500, {"failureDisposition": "content_unsupported"}),
    ],
)
async def test_http_problem_is_not_a_skip_unless_400_with_the_proof(
    tmp_path: Path, status: int, extensions: dict[str, object]
) -> None:
    exc = await _http_problem_failure(_problem_body(**extensions), status=status)
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="http-problem-not-a-skip")

    assert report.errors == 1
    assert report.skipped_unsupported == 0


class _ProblemUnsupportedRevisionIronRag(RichIronRag):
    """Raises, for named external keys, the exception the real client maps from
    an HTTP problem body -- the sweep sees the production problem path."""

    def __init__(self, *, unsupported_keys: set[str], body: dict[str, object]) -> None:
        super().__init__()
        self._unsupported_keys = unsupported_keys
        self._failure_body = body

    def _key_for(self, document_id: UUID | str) -> str:
        return next(
            (
                stored[1]
                for stored, doc in self.documents.items()
                if str(doc.id) == str(document_id)
            ),
            "",
        )

    async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> Any:
        if self._key_for(document_id) in self._unsupported_keys:
            raise await _http_problem_failure(self._failure_body)
        return await super().create_revision(document_id, **kwargs)


@pytest.mark.asyncio
async def test_http_400_content_unsupported_dependent_keeps_census_claim_and_copy(
    tmp_path: Path,
) -> None:
    """Two sweeps: the synchronous rejection is a terminal skip that neither
    withholds the census nor lets the reaper take the claimed copy. The census
    the first sweep proves makes the second a no-op for the parent."""
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t1")},
        {"1": (DependentSpec("image", "img-1", "i2"),)},
    )
    state = StateStore(tmp_path / "state.sqlite")
    ironrag = _ProblemUnsupportedRevisionIronRag(
        unsupported_keys={"echo:image:img-1"},
        body=_problem_body(failureDisposition="content_unsupported"),
    )
    _seed_legacy_page_with_images(state, ironrag, adapter)
    _, manager = _dependent_sweep(tmp_path, adapter, state=state, ironrag=ironrag)

    first = await manager.run_once(reason="http-unsupported-dependent-1")
    second = await manager.run_once(reason="http-unsupported-dependent-2")

    assert first.errors == 0
    assert first.skipped_unsupported == 1
    assert first.dependent_census_complete is True
    assert str(DOC_STALE_IMAGE) in ironrag.deletes
    assert second.errors == 0
    assert state.parent_census("page", "1") is not None
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert (LIB, "echo:image:img-1") in ironrag.documents
    assert str(DOC_IMAGE) not in ironrag.deletes


# --- IRG-188 review: malformed problems and non-admission 400 proofs ---


def _problem_client(body: dict[str, object], *, status: int = 400) -> IronRagClient:
    """A real client with the visibility capability and a problem for domain requests."""
    return IronRagClient(
        BaseConnectorSettings(
            ironrag_base_url="http://ironrag.example.com",
            ironrag_api_token="token",
            request_timeout_seconds=5.0,
        ),
        client=httpx.AsyncClient(
            base_url="http://ironrag.example.com",
            transport=httpx.MockTransport(
                lambda request: (
                    httpx.Response(200, json={"contentVisibilityContract": "visibility-intent/1"})
                    if request.url.path == "/v1/version"
                    else httpx.Response(status, json=body)
                )
            ),
        ),
    )


async def _list_problem_failure(body: dict[str, object], *, status: int = 400) -> BaseException:
    """The exception the real client maps from an HTTP problem on a read."""
    client = _problem_client(body, status=status)
    try:
        async for _ in client.list_documents(LIB):
            pass
    except BaseException as exc:
        return exc
    finally:
        await client.aclose()
    raise AssertionError("the problem body did not raise")


@pytest.mark.asyncio
async def test_malformed_problem_disposition_in_the_reap_listing_does_not_abort_the_sweep(
    tmp_path: Path,
) -> None:
    """The reaper's ``IronRagError`` catch must see the malformed problem.

    A raw pydantic ValidationError is not an IronRagError, so before the fix it
    escapes ``_list_reap_candidates`` and aborts the whole sweep.
    """
    client = _problem_client(_problem_body(failureDisposition=True))
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=NoopOrchestrator(),
        ironrag=client,
    )
    try:
        report = await manager.run_once(reason="malformed-problem-reap")
    finally:
        await client.aclose()

    assert report.items_seen == 1
    assert report.census_complete is True
    assert report.errors == 0


@pytest.mark.asyncio
async def test_malformed_problem_disposition_is_reported_as_a_sync_item_error(
    tmp_path: Path,
) -> None:
    """The real client's typed error, not a ValidationError, reaches the sweep.

    ``_list_problem_failure`` drives the production ``list_documents`` read, so
    the exception the sweep classifies is the one the client actually raises.
    """
    exc = await _list_problem_failure(_problem_body(failureDisposition=True))
    assert isinstance(exc, IronRagError)
    assert type(exc).__name__ == "IronRagProblemError"
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="malformed-problem-item")

    assert report.errors == 1
    assert report.skipped_unsupported == 0


@pytest.mark.asyncio
async def test_non_admission_400_proof_stays_a_sync_error(tmp_path: Path) -> None:
    """A GET/list problem carrying the extension is an operational error.

    Only ``create_document``/``create_revision`` submit content for admission;
    the same field seen by a read must not become a terminal skip.
    """
    exc = await _list_problem_failure(_problem_body(failureDisposition="content_unsupported"))
    assert type(exc).__name__ == "IronRagProblemError"
    manager = _manager(
        tmp_path,
        adapter=OneRefAdapter(asyncio.Event(), asyncio.Event()),
        orchestrator=RaisingOrchestrator(exc),
    )

    report = await manager.run_once(reason="non-admission-400-proof")

    assert report.errors == 1
    assert report.skipped_unsupported == 0
