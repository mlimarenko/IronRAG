"""Periodic full-sweep synchroniser.

The sync loop is the framework's heartbeat. Each pass:

1. Calls ``adapter.iter_items()`` to enumerate every primary item.
2. For each ref: hands it to :meth:`Orchestrator.push_ref`, which
   handles fetch + policy + push. Concurrency is bounded by
   ``sync_concurrency``.
3. After the enumeration completes successfully, runs the orphan reaper in
   two phases. The primary phase walks every primary ``kind`` the adapter
   declares, lists IronRAG documents under the adapter's external-key prefix
   and deletes any whose item was not seen in this sweep, or whose item was
   routed to a different library in this sweep, depending on policy; a
   primary that vanished from the census also loses its durable dependent
   membership. The dependent phase then walks the dependent kinds
   (attachments, images) the same way, keeping exactly the copies some
   parent still claims in that library.

The reaper is gated on a complete census. ``iter_items`` finishing normally
is the adapter's proof that every live primary item was yielded; an adapter
that lost that proof raises :class:`IncompleteCensusError` after yielding
what it could, and any other exception is a failed enumeration. In both
cases the enumerated items are still pushed, ``SyncReport.census_complete``
stays ``False`` and nothing is reaped: absence from an incomplete census is
not evidence of deletion. The next sweep retries.

The dependent phase has a second gate, ``SyncReport.dependent_census_complete``:
every enumerated primary must hold a parent census, i.e. at least one
complete fetch of its dependent set was applied. Until then a listed
dependent copy might belong to a parent nobody has proven yet, so the phase
is withheld; the orchestrator's proving refetch closes that gap on the next
sweeps. One never-proven parent therefore holds the whole dependent phase,
which is deliberate: it is visible in the report, and the alternative is
guessing.

A dependent the server proved unsupported is terminally accounted inside its
parent's census: the orchestrator keeps its claim, records it as an enumerated
member (claimed, not materialized) and settles the census normally, so the
claim keeps any legacy or server copy from being reaped and the census does
not stay unresolved. Only a server-proven ``content_unsupported`` disposition
is a skip -- carried either by a failed operation or by an HTTP 400 admission
problem -- and both carriers share this one accounting branch;
``retry_exhausted``, ``operational_terminal``, a null/unknown disposition and
every other HTTP problem stay errors. A validated, identity-bound terminal
POLICY primary failure still counts as an error, but its complete source fetch
can independently settle the actual dependents. A dependent failure or deferral
records no census; unvalidated primary failures and REQUIRED materializations
do not enter that independent phase.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable
from dataclasses import dataclass, field
from datetime import UTC, datetime
from typing import Any
from uuid import UUID

from .ironrag import (
    IronRagClient,
    IronRagContentUnsupportedError,
    IronRagContentUnsupportedProblemError,
    IronRagError,
    IronRagNotFoundError,
)
from .observability import get_logger
from .orchestrator import OrchestrationFailedError, OrchestrationOutcome, Orchestrator
from .policy import DeleteAction, PushPolicy
from .routing import PolicyOverrides, Router, RoutingReloader
from .source import (
    IncompleteCensusError,
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceExclusion,
    SourceItemRef,
    SourceMaterializationEnforcement,
    dependent_kinds,
)
from .state import CursorRow, DocumentLocation, StateStore

log = get_logger(__name__)


# A ref that keeps timing out still gets the full budget back this often, so a
# legitimately slow ref (a page with many attachments behind a vendor rate
# limit that spaces request starts) can still ingest; a genuinely stuck ref
# pays one full budget per cycle instead of one per sweep.
FULL_BUDGET_ESCALATION_ATTEMPTS = 5


def _effective_item_timeout(item_timeout: float, retry_timeout: float, attempts: int) -> float:
    """Full budget on the first attempt and every escalation attempt, the
    shorter replay budget in between. Never exceeds ``item_timeout``."""
    if attempts == 0 or attempts % FULL_BUDGET_ESCALATION_ATTEMPTS == 0:
        return item_timeout
    return min(item_timeout, retry_timeout)


@dataclass
class SyncReport:
    started_at: datetime
    finished_at: datetime
    items_seen: int = 0
    created: int = 0
    replaced: int = 0
    noop_unchanged: int = 0
    skipped: int = 0
    skipped_duplicate_content: int = 0
    skipped_missing: int = 0
    # Items that exist upstream but have nothing to ingest — an empty page, say.
    # Folded into skipped_missing before, which made "nothing to take" read as
    # "the vendor deleted it".
    skipped_empty: int = 0
    # Deterministic content rejections: the item is real and will never ingest.
    skipped_unsupported: int = 0
    # A REQUIRED delete decision forced by limited or unknown visibility,
    # counted per sweep. Independent of `reaped`/`skipped_empty`, exactly as
    # `deferred` is independent of `action`: the same decision also lands in one
    # of those counters, and a still-limited entity keeps producing it on every
    # sweep, so only this one says the visibility gate is what excluded it.
    skipped_limited_visibility: int = 0
    deferred: int = 0
    unrouted: int = 0
    # Per-item wall-clock budget exceeded; transient, so it is its own counter
    # rather than an error.
    item_timeouts: int = 0
    reaped: int = 0
    errors: int = 0
    census_complete: bool = False
    """``iter_items`` finished normally, so the reaper ran on proven absence."""
    dependent_census_complete: bool = False
    """Every enumerated parent has a recorded dependent census, so dependent kinds were reaped."""
    by_kind: dict[str, int] = field(default_factory=dict)

    def as_dict(self) -> dict[str, Any]:
        return {
            "started_at": self.started_at.isoformat(),
            "finished_at": self.finished_at.isoformat(),
            "duration_seconds": (self.finished_at - self.started_at).total_seconds(),
            "items_seen": self.items_seen,
            "created": self.created,
            "replaced": self.replaced,
            "noop_unchanged": self.noop_unchanged,
            "skipped": self.skipped,
            "skipped_duplicate_content": self.skipped_duplicate_content,
            "skipped_missing": self.skipped_missing,
            "skipped_empty": self.skipped_empty,
            "skipped_unsupported": self.skipped_unsupported,
            "skipped_limited_visibility": self.skipped_limited_visibility,
            "deferred": self.deferred,
            "unrouted": self.unrouted,
            "item_timeouts": self.item_timeouts,
            "reaped": self.reaped,
            "errors": self.errors,
            "census_complete": self.census_complete,
            "dependent_census_complete": self.dependent_census_complete,
            "by_kind": self.by_kind,
        }


class SyncAlreadyRunningError(RuntimeError):
    """A full sweep is already active for this connector process."""


@dataclass(frozen=True)
class _ReapPass:
    """One kind's reaper sweep: what is being reaped, and where the tally goes.

    Carried as one value so the sweep's helpers take the library they are
    working on rather than re-threading six unchanging arguments through
    each of them.
    """

    kind: str
    policy: PushPolicy
    enforcement: SourceDeleteEnforcement
    keep: Callable[[str, UUID], bool]
    seen_targets: dict[tuple[str, str], set[UUID]]
    report: SyncReport


@dataclass(frozen=True)
class _ReapStateEvidence:
    """Pre-sweep state whose target absence may be confirmed by CAS."""

    item_id: str
    cursor: CursorRow | None = None
    location: DocumentLocation | None = None


class SyncManager:
    def __init__(
        self,
        *,
        adapter: SourceAdapter,
        ironrag: IronRagClient,
        orchestrator: Orchestrator,
        router: Router,
        state: StateStore,
        policies: PolicyOverrides,
        concurrency: int,
        interval_seconds: int,
        item_timeout_seconds: float = 300.0,
        item_retry_timeout_seconds: float = 30.0,
        reaper_list_timeout_seconds: float = 30.0,
        routing_reloader: RoutingReloader | None = None,
    ) -> None:
        self._adapter = adapter
        self._dependent_kinds = dependent_kinds(adapter)
        self._ironrag = ironrag
        self._orchestrator = orchestrator
        self._router = router
        self._state = state
        self._policies = policies
        self._concurrency = concurrency
        self._interval = interval_seconds
        self._item_timeout = item_timeout_seconds
        self._item_retry_timeout = item_retry_timeout_seconds
        self._reaper_list_timeout = reaper_list_timeout_seconds
        self._routing_reloader = routing_reloader
        self._run_lock = asyncio.Lock()

    async def run_once(self, *, reason: str, wait_for_idle: bool = False) -> SyncReport:
        """Run the canonical sweep; background requests can await the single-flight owner."""
        if self._run_lock.locked() and not wait_for_idle:
            log.info(
                "sync.already_running",
                reason=reason,
                connector=self._adapter.name,
            )
            raise SyncAlreadyRunningError(
                f"sync already running for connector {self._adapter.name}"
            )
        async with self._run_lock:
            return await self._run_once_unlocked(reason=reason)

    async def run_on_request(self, requested: asyncio.Event) -> None:
        """Coalesce webhook cleanup requests into background canonical sweeps.

        Only incoming deliveries set the event. Clear it before the sweep so
        deliveries during a run request one follow-up, never a self-retry loop.
        Cancellation belongs to the server lifespan, including while awaiting
        an active manual sweep's single-flight lock.
        """
        while True:
            await requested.wait()
            requested.clear()
            try:
                await self.run_once(reason="webhook-dependent-cleanup", wait_for_idle=True)
            except Exception as exc:
                log.error("sync.webhook_cleanup_error", error=str(exc))

    async def _run_once_unlocked(self, *, reason: str) -> SyncReport:
        await self._ironrag.ensure_visibility_contract()
        started = datetime.now(tz=UTC)
        log.info("sync.start", reason=reason, connector=self._adapter.name)
        report = SyncReport(started_at=started, finished_at=started)
        if self._routing_reloader is not None:
            await self._routing_reloader.reload_if_changed()
        # Snapshot scan scope and CAS evidence BEFORE enumeration can advance a
        # cursor or register a new location. Cursor-only state survives legacy
        # migration and duplicate-content skips, so it is scan evidence even
        # though it is deliberately not materialized-copy ownership.
        pre_sweep_library_scope, pre_sweep_state = _snapshot_reap_state(
            self._adapter,
            self._state,
        )
        self._orchestrator.reset_sweep_cache(pre_sweep_library_scope)

        sem = asyncio.Semaphore(self._concurrency)
        seen: dict[str, set[str]] = {k: set() for k in self._adapter.kinds}
        seen_targets: dict[tuple[str, str], set[UUID]] = {}
        required_seen: set[tuple[str, str]] = set()
        tasks: list[asyncio.Task[None]] = []

        async def process(ref: Any) -> None:
            if ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED:
                required_seen.add((ref.kind, ref.item_id))
            seen.setdefault(ref.kind, set()).add(ref.item_id)
            report.items_seen += 1
            report.by_kind[ref.kind] = report.by_kind.get(ref.kind, 0) + 1
            async with sem:
                attempts = self._item_timeout_attempts(ref)
                budget = _effective_item_timeout(
                    self._item_timeout, self._item_retry_timeout, attempts
                )
                try:
                    log.info(
                        "sync.item.start",
                        kind=ref.kind,
                        item_id=ref.item_id,
                        external_key=ref.external_key,
                        timeout_seconds=budget,
                        attempts=attempts,
                    )
                    outcome = await asyncio.wait_for(
                        self._orchestrator.push_ref(ref),
                        timeout=budget,
                    )
                except TimeoutError:
                    recorded = self._record_item_timeout(ref)
                    log.error(
                        "sync.item_timeout",
                        kind=ref.kind,
                        item_id=ref.item_id,
                        external_key=ref.external_key,
                        timeout_seconds=budget,
                        attempts=recorded,
                    )
                    report.item_timeouts += 1
                    return
                except (
                    IronRagContentUnsupportedError,
                    IronRagContentUnsupportedProblemError,
                ) as exc:
                    # The one skip, for both server-proven carriers: the
                    # synchronous 400 admission problem and the failed
                    # operation each carry ``failureDisposition=
                    # content_unsupported``, so the bytes can never be
                    # ingested. Every other failure -- any other HTTP problem,
                    # or a failed operation whose disposition is null, unknown,
                    # retry_exhausted or operational_terminal -- is an error on
                    # the next branch (fail closed).
                    self._clear_item_timeout(ref)
                    log.warning(
                        "sync.item_content_unsupported",
                        kind=ref.kind,
                        item_id=ref.item_id,
                        external_key=ref.external_key,
                        failure_code=exc.failure_code,
                        disposition=exc.failure_disposition,
                    )
                    report.skipped_unsupported += 1
                    return
                except OrchestrationFailedError as exc:
                    self._clear_item_timeout(ref)
                    report.errors += 1 + int(exc.dependent_error is not None)
                    log.error(
                        "sync.item_error",
                        kind=ref.kind,
                        item_id=ref.item_id,
                        error=str(exc.primary_error),
                        dependent_error=(str(exc.dependent_error) if exc.dependent_error else None),
                    )
                    for dep in exc.dependent_outcomes:
                        seen.setdefault(dep.ref.kind, set()).add(dep.ref.item_id)
                        _record_seen_target(seen_targets, dep)
                        _log_outcome(dep)
                        _tally(report, dep)
                    return
                except Exception as exc:
                    self._clear_item_timeout(ref)
                    log.error(
                        "sync.item_error",
                        kind=ref.kind,
                        item_id=ref.item_id,
                        external_key=ref.external_key,
                        error_type=type(exc).__name__,
                        error=str(exc) or repr(exc),
                    )
                    report.errors += 1
                    return
            self._clear_item_timeout(ref)
            _record_seen_target(seen_targets, outcome)
            _log_outcome(outcome)
            _tally(report, outcome)
            for dep in outcome.dependent_outcomes:
                seen.setdefault(dep.ref.kind, set()).add(dep.ref.item_id)
                _record_seen_target(seen_targets, dep)
                _log_outcome(dep)
                _tally(report, dep)

        try:
            async for ref in self._adapter.iter_items():
                tasks.append(asyncio.create_task(process(ref)))
            report.census_complete = True
        except asyncio.CancelledError:
            await self._abandon_sweep(tasks, report, reason)
            raise
        except IncompleteCensusError as exc:
            log.error(
                "sync.census_incomplete",
                reason=exc.reason,
                scopes=list(exc.scopes),
            )
            report.errors += 1
        except Exception as exc:
            log.error(
                "sync.enumeration_error",
                error_type=type(exc).__name__,
                error=str(exc) or repr(exc),
            )
            report.errors += 1

        # Every enumerated item is pushed whether or not the census finished:
        # the items are real, and returning (and releasing the single-flight
        # lock) with pushes still in flight would let the next sweep race them.
        try:
            await asyncio.gather(*tasks, return_exceptions=False)
        except asyncio.CancelledError:
            await self._abandon_sweep(tasks, report, reason)
            raise

        if report.census_complete:
            # A present REQUIRED source retires old routes through accepted
            # denial, not the orphan delete path. Retain every durable copy,
            # including copies discovered before a failed/unknown admission.
            # Explicit source deletion has already removed its location rows.
            for kind, item_id in required_seen:
                seen_targets.setdefault((kind, item_id), set()).update(
                    UUID(location.ironrag_library_id)
                    for location in self._state.locations_for(kind, item_id)
                )
            await self._reap(
                seen,
                seen_targets,
                pre_sweep_library_scope,
                pre_sweep_state,
                report,
            )
            unproven = self._unproven_parents(seen)
            report.dependent_census_complete = not unproven
            if report.dependent_census_complete:
                await self._reap_dependents(
                    seen_targets, pre_sweep_library_scope, pre_sweep_state, report
                )
            else:
                log.warning(
                    "sync.dependent_census_incomplete",
                    connector=self._adapter.name,
                    reason="an enumerated parent holds claims no census proves",
                    unproven_parents={
                        kind: sorted(ids)[:20] for kind, ids in sorted(unproven.items())
                    },
                    unproven_count=sum(len(ids) for ids in unproven.values()),
                )

        report.finished_at = datetime.now(tz=UTC)
        log.info(
            "sync.done",
            reason=reason,
            **{k: v for k, v in report.as_dict().items() if k not in ("started_at", "finished_at")},
        )
        return report

    def _item_timeout_attempts(self, ref: Any) -> int:
        """Consecutive timeout count; bookkeeping failure falls back to the full budget."""
        try:
            return self._state.item_timeout_attempts(ref.kind, ref.item_id)
        except Exception:
            log.warning(
                "sync.item_timeout_state_read_failed",
                kind=ref.kind,
                item_id=ref.item_id,
            )
            return 0

    def _record_item_timeout(self, ref: Any) -> int | None:
        """Record one timeout; a bookkeeping failure must not end the sweep.

        ``None`` means the count was not recorded, so the log shows that rather
        than reporting a made-up attempt number.
        """
        try:
            return self._state.record_item_timeout(kind=ref.kind, item_id=ref.item_id)
        except Exception:
            log.warning(
                "sync.item_timeout_state_write_failed",
                kind=ref.kind,
                item_id=ref.item_id,
            )
            return None

    def _clear_item_timeout(self, ref: Any) -> None:
        """Clear the count; a bookkeeping failure must not end the sweep."""
        try:
            self._state.clear_item_timeout(kind=ref.kind, item_id=ref.item_id)
        except Exception:
            log.warning(
                "sync.item_timeout_state_clear_failed",
                kind=ref.kind,
                item_id=ref.item_id,
            )

    async def _abandon_sweep(
        self,
        tasks: list[asyncio.Task[None]],
        report: SyncReport,
        reason: str,
    ) -> None:
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        report.finished_at = datetime.now(tz=UTC)
        log.warning(
            "sync.cancelled",
            reason=reason,
            pending_tasks=sum(not task.done() for task in tasks),
            **{k: v for k, v in report.as_dict().items() if k not in ("started_at", "finished_at")},
        )

    def _reaping_terms(self, kind: str) -> tuple[PushPolicy, SourceDeleteEnforcement] | None:
        """Policy and enforcement under which ``kind`` reaps, or ``None`` if it never does."""
        policy = self._policies.for_kind(kind)
        enforcement = self._adapter.missing_enforcement(kind)
        if (
            enforcement is SourceDeleteEnforcement.POLICY
            and policy.on_missing is DeleteAction.IGNORE
        ):
            return None
        return policy, enforcement

    async def _reap(
        self,
        seen: dict[str, set[str]],
        seen_targets: dict[tuple[str, str], set[UUID]],
        pre_sweep_library_scope: dict[str, set[UUID]],
        pre_sweep_state: dict[tuple[str, UUID], tuple[_ReapStateEvidence, ...]],
        report: SyncReport,
    ) -> None:
        """Primary phase: delete IronRAG docs whose primary item vanished.

        A primary absent from the complete census is also forgotten as a
        parent: its census and claims go, so the copies only it claimed are
        unclaimed for the dependent phase. A primary the policy retains keeps
        its membership and, with it, its dependents.
        """
        for kind in self._adapter.primary_kinds:
            terms = self._reaping_terms(kind)
            if terms is None:
                continue
            policy, enforcement = terms

            def still_expected(item_id: str, library_id: UUID, *, kind: str = kind) -> bool:
                return _document_still_expected(kind, item_id, library_id, seen, seen_targets)

            await self._reap_kind(
                kind,
                policy,
                enforcement,
                still_expected,
                pre_sweep_library_scope,
                pre_sweep_state,
                seen_targets,
                report,
            )
            self._forget_absent_parents(kind, seen.get(kind, set()))

    def _forget_absent_parents(self, kind: str, seen_ids: set[str]) -> None:
        """Withdraw parents the complete census did not list.

        Every parent this store knows is considered, not only the proven ones:
        a parent whose fetch died between its first claim and its census holds
        claims with no census row, and skipping it would leave the copies it
        claimed unreapable for good.
        """
        if not self._dependent_kinds:
            return
        for item_id in sorted(self._state.known_parents(kind) - seen_ids):
            released = self._state.release_parent(kind, item_id)
            log.info(
                "sync.reap.parent_forgotten",
                kind=kind,
                item_id=item_id,
                released=len(released),
            )

    def _unproven_parents(self, seen: dict[str, set[str]]) -> dict[str, set[str]]:
        """Enumerated parents that own copies no census describes, by kind.

        A parent owns copies once this connector has pushed it (a cursor) or
        claimed a member for it. Owning them without a census is exactly the
        pre-membership state -- legacy dependents pushed before this contract,
        or a fetch that died before proving its set -- and the dependent phase
        would read those unclaimed copies as orphans.

        An enumerated item this connector never pushed and never claimed for is
        not such a parent: an item that is unrouted, or whose only copy has
        already been deleted, cannot own an unclaimed dependent, so it must not
        withhold the dependent phase from the whole connector for good.
        """
        if not self._dependent_kinds:
            return {}
        unproven: dict[str, set[str]] = {}
        for kind in self._adapter.primary_kinds:
            enumerated = seen.get(kind, set())
            if not enumerated:
                continue
            proven = {census.item_id for census in self._state.parents_with_census(kind)}
            owning = {row.item_id for row in self._state.items_of_kind(kind)}
            owning |= self._state.parents_with_claims(kind)
            blocked = (enumerated & owning) - proven
            if blocked:
                unproven[kind] = blocked
        return unproven

    async def _reap_dependents(
        self,
        seen_targets: dict[tuple[str, str], set[UUID]],
        pre_sweep_library_scope: dict[str, set[UUID]],
        pre_sweep_state: dict[tuple[str, UUID], tuple[_ReapStateEvidence, ...]],
        report: SyncReport,
    ) -> None:
        """Dependent phase: delete dependent copies no proven parent claims.

        Membership is the only evidence. A copy pushed this sweep is claimed
        before its push, so it is kept; a copy left from an older parent
        fetch, a forgotten parent or a route move is not.
        """
        for kind in self._dependent_kinds:
            terms = self._reaping_terms(kind)
            if terms is None:
                continue
            policy, enforcement = terms

            def claimed(item_id: str, library_id: UUID, *, kind: str = kind) -> bool:
                return self._state.has_claims(kind, item_id, str(library_id))

            await self._reap_kind(
                kind,
                policy,
                enforcement,
                claimed,
                pre_sweep_library_scope,
                pre_sweep_state,
                seen_targets,
                report,
            )

    async def _reap_kind(
        self,
        kind: str,
        policy: PushPolicy,
        enforcement: SourceDeleteEnforcement,
        keep: Callable[[str, UUID], bool],
        pre_sweep_library_scope: dict[str, set[UUID]],
        pre_sweep_state: dict[tuple[str, UUID], tuple[_ReapStateEvidence, ...]],
        seen_targets: dict[tuple[str, str], set[UUID]],
        report: SyncReport,
    ) -> None:
        """List every scoped library for ``kind`` and reap what ``keep`` rejects."""
        sweep = _ReapPass(
            kind=kind,
            policy=policy,
            enforcement=enforcement,
            keep=keep,
            seen_targets=seen_targets,
            report=report,
        )
        prefix = self._adapter.external_key(kind, "")
        # Union current routing with every pre-sweep cursor/location
        # library. A migrated cursor has no trusted document id or location,
        # but its old route is still required exact-key discovery scope.
        target_libs = self._router.target_libraries() | pre_sweep_library_scope.get(kind, set())
        for library_id in target_libs:
            pairs = await self._list_reap_candidates(library_id, prefix)
            if pairs is None:
                continue
            await self._reap_library(sweep, library_id, pairs, pre_sweep_state)

    async def _list_reap_candidates(
        self,
        library_id: UUID,
        prefix: str,
    ) -> list[tuple[str, str]] | None:
        """Exact-key listing for one library, or ``None`` when it must be skipped."""
        try:
            async with asyncio.timeout(self._reaper_list_timeout):
                return await self._list_by_prefix(library_id, prefix)
        except TimeoutError:
            log.warning(
                "sync.reap.list_timeout",
                library_id=str(library_id),
                prefix=prefix,
                timeout_seconds=self._reaper_list_timeout,
            )
            return None
        except IronRagNotFoundError:
            # The library itself is gone. Its documents went with it,
            # so there is nothing left to reap. Drop its locations and
            # current-route cursors so the registry does not nominate
            # the missing library again on every sweep.
            dropped = self._state.delete_by_library(str(library_id))
            log.info(
                "sync.reap.library_gone",
                library_id=str(library_id),
                prefix=prefix,
                current_cursor_rows_dropped=dropped,
            )
            return None
        except IronRagError as exc:
            log.warning(
                "sync.reap.list_error",
                library_id=str(library_id),
                prefix=prefix,
                error=str(exc),
            )
            return None

    async def _reap_library(
        self,
        sweep: _ReapPass,
        library_id: UUID,
        pairs: list[tuple[str, str]],
        pre_sweep_state: dict[tuple[str, UUID], tuple[_ReapStateEvidence, ...]],
    ) -> None:
        """Reap one library: what the listing shows, then what it fails to show."""
        valid_evidence, invalid_evidence = self._partition_pre_sweep_evidence(
            sweep,
            library_id,
            pre_sweep_state,
        )
        listed_keys = {external_key for external_key, _ in pairs}
        await self._reap_listed_documents(
            sweep,
            library_id,
            pairs,
            valid_evidence,
            invalid_evidence,
        )
        await self._confirm_unlisted_evidence(sweep, library_id, valid_evidence, listed_keys)

    def _partition_pre_sweep_evidence(
        self,
        sweep: _ReapPass,
        library_id: UUID,
        pre_sweep_state: dict[tuple[str, UUID], tuple[_ReapStateEvidence, ...]],
    ) -> tuple[dict[str, tuple[_ReapStateEvidence, str]], set[str]]:
        """Split this library's evidence into what carries a canonical key and what does not."""
        valid_evidence: dict[str, tuple[_ReapStateEvidence, str]] = {}
        invalid_evidence: set[str] = set()
        for evidence in pre_sweep_state.get((sweep.kind, library_id), ()):
            canonical_key = _canonical_evidence_key(
                self._adapter,
                sweep.kind,
                evidence,
            )
            if canonical_key is None:
                log.error(
                    "sync.reap.noncanonical_state",
                    kind=sweep.kind,
                    item_id=evidence.item_id,
                    library_id=str(library_id),
                )
                sweep.report.errors += 1
                invalid_evidence.add(evidence.item_id)
                continue
            valid_evidence[evidence.item_id] = (evidence, canonical_key)
        return valid_evidence, invalid_evidence

    def _reapable_item_id(
        self,
        sweep: _ReapPass,
        library_id: UUID,
        external_key: str,
        document_id: str,
        invalid_evidence: set[str],
    ) -> str | None:
        """The item id this listed pair may be reaped under, or ``None`` to leave it alone."""
        parsed = self._adapter.parse_external_key(external_key)
        if parsed is None:
            return None
        parsed_kind, parsed_item_id = parsed
        if parsed_kind != sweep.kind:
            return None
        if external_key != self._adapter.external_key(sweep.kind, parsed_item_id):
            log.error(
                "sync.reap.noncanonical_document",
                kind=sweep.kind,
                item_id=parsed_item_id,
                library_id=str(library_id),
                document_id=document_id,
            )
            sweep.report.errors += 1
            return None
        if parsed_item_id in invalid_evidence:
            return None
        if sweep.keep(parsed_item_id, library_id):
            return None
        return parsed_item_id

    async def _reap_listed_documents(
        self,
        sweep: _ReapPass,
        library_id: UUID,
        pairs: list[tuple[str, str]],
        valid_evidence: dict[str, tuple[_ReapStateEvidence, str]],
        invalid_evidence: set[str],
    ) -> None:
        """Delete every listed document the census no longer keeps."""
        for external_key, document_id in pairs:
            item_id = self._reapable_item_id(
                sweep,
                library_id,
                external_key,
                document_id,
                invalid_evidence,
            )
            if item_id is None:
                continue
            canonical_key = self._adapter.external_key(sweep.kind, item_id)
            expected = valid_evidence.get(item_id)
            expected_state = expected[0] if expected is not None else None
            try:
                outcome = await self._orchestrator.reap_orphan(
                    SourceItemRef(
                        item_id=item_id,
                        kind=sweep.kind,
                        external_key=canonical_key,
                    ),
                    library_id,
                    document_id,
                    sweep.policy,
                    enforcement=sweep.enforcement,
                    expected_cursor=_expected_target_cursor(
                        expected_state,
                        sweep.kind,
                        item_id,
                        library_id,
                        sweep.seen_targets,
                    ),
                    expected_location=(
                        expected_state.location if expected_state is not None else None
                    ),
                )
                _log_outcome(outcome)
                _tally(sweep.report, outcome)
            except IronRagError as exc:
                log.warning(
                    "sync.reap.delete_error",
                    external_key=external_key,
                    document_id=document_id,
                    error=str(exc),
                )
                sweep.report.errors += 1

    async def _confirm_unlisted_evidence(
        self,
        sweep: _ReapPass,
        library_id: UUID,
        valid_evidence: dict[str, tuple[_ReapStateEvidence, str]],
        listed_keys: set[str],
    ) -> None:
        """Confirm absence for state the listing did not show and the census dropped."""
        for evidence, canonical_key in valid_evidence.values():
            if sweep.keep(evidence.item_id, library_id):
                continue
            if canonical_key in listed_keys:
                continue
            try:
                outcome = await self._orchestrator.confirm_orphan_absent(
                    SourceItemRef(
                        item_id=evidence.item_id,
                        kind=sweep.kind,
                        external_key=canonical_key,
                    ),
                    library_id,
                    sweep.policy,
                    enforcement=sweep.enforcement,
                    expected_cursor=_expected_target_cursor(
                        evidence,
                        sweep.kind,
                        evidence.item_id,
                        library_id,
                        sweep.seen_targets,
                    ),
                    expected_location=evidence.location,
                )
                _log_outcome(outcome)
                if outcome.deferred:
                    sweep.report.deferred += 1
            except IronRagError as exc:
                log.warning(
                    "sync.reap.confirm_error",
                    external_key=canonical_key,
                    library_id=str(library_id),
                    error=str(exc),
                )
                sweep.report.errors += 1

    async def _list_by_prefix(self, library_id: UUID, prefix: str) -> list[tuple[str, str]]:
        """Every ``(externalKey, documentId)`` pair under ``prefix`` in a library.

        Built on the single ``list_documents`` walk (plan S7.7: no
        independent pagination reimplementation) narrowed server-side via
        ``search`` -- the same ILIKE-substring narrowing
        ``IronRagClient.find_document`` used to do by hand, now just a
        client-side ``startswith`` filter over one search-narrowed walk
        instead of a full unfiltered scan. There is no server-side prefix
        filter on the list endpoint (plan S7.1's field list is cursor/
        limit/search/externalKey/includeDeleted/status), so this is the
        correct way to reconstruct prefix listing from that surface.
        """
        pairs: list[tuple[str, str]] = []
        async for document in self._ironrag.list_documents(library_id, search=prefix):
            if document.external_key.startswith(prefix):
                pairs.append((document.external_key, str(document.id)))
        return pairs

    async def run_forever(self, cancel_event: asyncio.Event) -> None:
        while True:
            try:
                await asyncio.wait_for(cancel_event.wait(), timeout=float(self._interval))
                return
            except TimeoutError:
                pass
            try:
                await self.run_once(reason="periodic")
            except SyncAlreadyRunningError as exc:
                log.info("sync.periodic_skipped", reason=str(exc))
            except asyncio.CancelledError:
                raise
            except Exception as exc:
                log.error("sync.periodic_error", error=str(exc))


def _log_outcome(outcome: OrchestrationOutcome) -> None:
    """One-line per-document decision log: kind, id, action, reason, doc_id, library."""
    level = log.warning if outcome.action in ("unrouted", "fetch_deferred") else log.info
    level(
        f"sync.item.{outcome.action}",
        kind=outcome.ref.kind,
        item_id=outcome.ref.item_id,
        external_key=outcome.ref.external_key,
        ironrag_document_id=outcome.ironrag_document_id,
        library_id=str(outcome.library_id) if outcome.library_id else None,
        rule=outcome.rule_description,
        detail=outcome.detail,
        deferred=outcome.deferred,
        title=(outcome.ref.raw or {}).get("name") or (outcome.ref.raw or {}).get("title"),
    )


def _record_seen_target(
    seen_targets: dict[tuple[str, str], set[UUID]], outcome: OrchestrationOutcome
) -> None:
    if outcome.library_id is None:
        return
    if outcome.action in {
        "unrouted",
        "fetch_deferred",
        "source_absent",
        "skipped_missing",
        "deleted",
    }:
        return
    seen_targets.setdefault((outcome.ref.kind, outcome.ref.item_id), set()).add(outcome.library_id)


def _document_still_expected(
    kind: str,
    item_id: str,
    library_id: UUID,
    seen: dict[str, set[str]],
    seen_targets: dict[tuple[str, str], set[UUID]],
) -> bool:
    if item_id not in seen.get(kind, set()):
        return False
    expected_libraries = seen_targets.get((kind, item_id), set())
    if not expected_libraries:
        # The source item was enumerated, but no successful route/push target
        # was established in this sweep. Keep existing documents rather than
        # turning transient fetch/routing failures into destructive deletes.
        return True
    return library_id in expected_libraries


def _snapshot_reap_state(
    adapter: SourceAdapter,
    state: StateStore,
) -> tuple[
    dict[str, set[UUID]],
    dict[tuple[str, UUID], tuple[_ReapStateEvidence, ...]],
]:
    """Capture restart-safe scan scope and compare-and-delete evidence."""
    library_scope: dict[str, set[UUID]] = {}
    by_identity: dict[tuple[str, UUID, str], _ReapStateEvidence] = {}
    for kind in adapter.kinds:
        kind_libraries: set[UUID] = set()
        for cursor in state.items_of_kind(kind):
            library_id = UUID(cursor.current_library_id)
            kind_libraries.add(library_id)
            key = (kind, library_id, cursor.item_id)
            current = by_identity.get(key)
            by_identity[key] = _ReapStateEvidence(
                item_id=cursor.item_id,
                cursor=cursor,
                location=current.location if current is not None else None,
            )
        for location in state.locations_of_kind(kind):
            library_id = UUID(location.ironrag_library_id)
            kind_libraries.add(library_id)
            key = (kind, library_id, location.item_id)
            current = by_identity.get(key)
            by_identity[key] = _ReapStateEvidence(
                item_id=location.item_id,
                cursor=current.cursor if current is not None else None,
                location=location,
            )
        library_scope[kind] = kind_libraries

    grouped: dict[tuple[str, UUID], list[_ReapStateEvidence]] = {}
    for (kind, library_id, _), evidence in by_identity.items():
        grouped.setdefault((kind, library_id), []).append(evidence)
    return library_scope, {
        key: tuple(sorted(evidence, key=lambda candidate: candidate.item_id))
        for key, evidence in grouped.items()
    }


def _canonical_evidence_key(
    adapter: SourceAdapter,
    kind: str,
    evidence: _ReapStateEvidence,
) -> str | None:
    canonical_key = adapter.external_key(kind, evidence.item_id)
    persisted_keys = {
        row.external_key for row in (evidence.cursor, evidence.location) if row is not None
    }
    if persisted_keys != {canonical_key}:
        return None
    if adapter.parse_external_key(canonical_key) != (kind, evidence.item_id):
        return None
    return canonical_key


def _expected_target_cursor(
    evidence: _ReapStateEvidence | None,
    kind: str,
    item_id: str,
    library_id: UUID,
    seen_targets: dict[tuple[str, str], set[UUID]],
) -> CursorRow | None:
    """Account for a route move applied by this sweep before its reap."""
    if evidence is None:
        return None
    established_targets = seen_targets.get((kind, item_id), set())
    if established_targets and library_id not in established_targets:
        return None
    return evidence.cursor


def _tally(report: SyncReport, outcome: OrchestrationOutcome) -> None:
    if outcome.deferred:
        report.deferred += 1
    action = outcome.action
    if action == "created":
        report.created += 1
    elif action == "replaced":
        report.replaced += 1
    elif action == "noop_unchanged":
        report.noop_unchanged += 1
    elif action in ("skipped_new", "skipped_changed"):
        report.skipped += 1
    elif action == "skipped_duplicate_content":
        report.skipped_duplicate_content += 1
    elif action == "skipped_unsupported":
        report.skipped_unsupported += 1
    elif action == "skipped_missing":
        report.skipped_missing += 1
    elif action == "source_absent":
        report.skipped_empty += 1
    elif action == "unrouted":
        report.unrouted += 1
    elif action == "deleted":
        report.reaped += 1
    if outcome.exclusion is SourceExclusion.LIMITED_VISIBILITY:
        report.skipped_limited_visibility += 1
