"""Apply one source fetch decision to IronRAG under the resolved policy.

The orchestrator is policy-aware but transport-agnostic: it does not
know about webhooks vs sweeps. Both code paths converge on
:meth:`Orchestrator.push_ref` and receive an outcome or a propagated failure.

Dispatch flow per item
=======================

1. Look up an existing IronRAG document by ``ref.external_key`` in the
   target library (durable location registry first, then a single exact-match
   ``find_document`` call if the registry doesn't already know it).
2. Compare ``ref.change_token`` against the value stored in
   :class:`StateStore`. If equal AND a document exists, short-circuit
   to ``noop_unchanged``.
3. Fetch one closed source decision: materialize, delete, or defer without
   mutation. Source-required deletes override ``on_missing=ignore``.
4. For a materialized item, choose action per policy:
   * No existing doc and ``on_new=create`` -> ``create_document`` (201,
     synchronous).
   * No existing doc and ``on_new=skip`` -> ``skipped_new``.
   * Existing doc and ``on_changed=replace`` -> ``create_revision`` (202,
     polled to terminal via ``wait_for_operation``).
   * Existing doc and ``on_changed=skip`` -> ``skipped_changed``.
5. On a 409 ``duplicate_content`` create conflict: apply
   ``on_duplicate_content`` without adopting the conflicting document as
   source-owned; identical bytes can belong to a different external key.
6. Update the source cursor and materialized-copy registry only after the
   requested target state is established.
7. Dependents: claim each one in durable membership, push it, then record
   the parent's census at the enumerated tokens and release the claims the
   complete fetch omitted. Released copies wait for the completeness-gated
   dependent reaper, since legacy owners may still lack claims. A vanished parent
   (fetch delete, webhook delete, required no-route) releases its whole set.

Async mutations (revisions, deletes) submit then poll: submission
(``create_revision``/``delete_document``) hands back an operation id,
:meth:`IronRagClient.wait_for_operation` polls it to a terminal state,
and this module reacts to that terminal state (ready = success,
superseded/canceled = defer to next sweep, failed = propagate).
"""

from __future__ import annotations

import asyncio
import hashlib
from collections.abc import Mapping
from dataclasses import dataclass, field, replace
from typing import Literal, assert_never
from uuid import UUID, uuid4
from weakref import WeakValueDictionary

import httpx

from .ironrag import (
    DocumentResource,
    IronRagClient,
    IronRagConflictError,
    IronRagContentUnsupportedError,
    IronRagContentUnsupportedProblemError,
    IronRagDuplicateContentError,
    IronRagError,
    IronRagMutationTimeoutError,
    IronRagNotFoundError,
    IronRagOperationFailedError,
    IronRagVisibilityConflictError,
    OperationHandle,
    OperationStatusValue,
    ReplacementIdentity,
    RevisionProvenance,
)
from .observability import get_logger
from .policy import (
    DeleteAction,
    DuplicateContentAction,
    PushPolicy,
    UpdateAction,
    UpsertAction,
)
from .routing import PolicyOverrides, ResolvedRoute, Router, RoutingError, UnroutedError
from .source import (
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceExclusion,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceItem,
    SourceItemRef,
    SourceMaterializationEnforcement,
    dependent_kinds,
)
from .state import (
    CursorRow,
    DocumentLocation,
    MaterializationAttempt,
    MemberTarget,
    PayloadIdentity,
    StateStore,
)

log = get_logger(__name__)

OutcomeAction = Literal[
    "created",
    "replaced",
    "deleted",
    "noop_unchanged",
    "skipped_new",
    "skipped_changed",
    "skipped_missing",
    "skipped_duplicate_content",
    "skipped_duplicate_fail",
    "skipped_unsupported",
    "unrouted",
    "source_absent",
    "fetch_deferred",
]


@dataclass(frozen=True)
class OrchestrationOutcome:
    ref: SourceItemRef
    action: OutcomeAction
    workspace_id: UUID | None
    library_id: UUID | None
    rule_description: str | None
    ironrag_document_id: str | None
    detail: str
    deferred: bool = False
    dependent_outcomes: tuple[OrchestrationOutcome, ...] = field(default_factory=tuple)
    exclusion: SourceExclusion = SourceExclusion.NONE


class OrchestrationFailedError(IronRagOperationFailedError):
    """Validated POLICY primary failure with independently completed dependents.

    Still an exception for webhooks; neither failure is manufactured into success.
    """

    def __init__(
        self,
        primary_error: IronRagOperationFailedError,
        dependent_outcomes: tuple[OrchestrationOutcome, ...] = (),
        dependent_error: Exception | None = None,
    ) -> None:
        self.primary_error = primary_error
        self.dependent_outcomes = dependent_outcomes
        self.dependent_error = dependent_error
        super().__init__(primary_error.status)


@dataclass(frozen=True)
class _ExactTargetSnapshot:
    """Exact-key documents across every library relevant to one source id."""

    cursor: CursorRow | None
    known_locations: tuple[DocumentLocation, ...]
    relevant_libraries: frozenset[UUID]
    documents: dict[UUID, str]


@dataclass(frozen=True)
class _SweepPushMemo:
    """One successfully handled target snapshot within the active sweep."""

    change_token: str | None
    document_id: str | None


def _primary_deferred(outcome: OrchestrationOutcome) -> bool:
    """Primary write is intentionally retried later, so dependents wait too."""
    return outcome.deferred


def _library_scope_fingerprint(library_ids: set[UUID]) -> str:
    canonical = "\n".join(sorted(str(library_id) for library_id in library_ids))
    return hashlib.sha256(canonical.encode("ascii")).hexdigest()


def _missing_requires_delete(
    policy: PushPolicy,
    enforcement: SourceDeleteEnforcement,
) -> bool:
    """One policy gate for source deletes, reaps, and proven absence."""
    return (
        enforcement is SourceDeleteEnforcement.REQUIRED or policy.on_missing is DeleteAction.DELETE
    )


def _validated_document_id(
    document: DocumentResource,
    *,
    library_id: UUID,
    external_key: str,
) -> str:
    """Return an id only for the exact canonical target that was requested."""
    if document.library_id != library_id or document.external_key != external_key:
        raise IronRagError(
            "IronRAG document library/external-key identity did not match "
            f"requested target {library_id}/{external_key}"
        )
    return str(document.id)


class Orchestrator:
    def __init__(
        self,
        *,
        adapter: SourceAdapter,
        ironrag: IronRagClient,
        router: Router,
        state: StateStore,
        policies: PolicyOverrides,
    ) -> None:
        self._adapter = adapter
        self._ironrag = ironrag
        self._router = router
        self._state = state
        self._policies = policies
        self._dependent_kinds = frozenset(dependent_kinds(adapter))
        # In-sweep dedup: collapses repeated push_item calls for the
        # same external_key down to one IronRAG mutation. Necessary
        # because dependents (images, attachments) can be reached from
        # multiple parents in one sweep -- without this every shared
        # image triggers a "same idempotency key used for a different
        # mutation request" 409 from IronRAG.
        self._sweep_pushed: dict[tuple[UUID, str], _SweepPushMemo] = {}
        self._pre_sweep_libraries_by_kind: dict[str, set[UUID]] = {}
        # Serialize all remote + state mutations for one source identity.
        # Weak values avoid retaining a lock forever for every item ever seen.
        # This is a single-process boundary; deployments run one connector
        # replica, so no distributed lock is required by the runtime contract.
        self._identity_locks: WeakValueDictionary[tuple[str, str], asyncio.Lock] = (
            WeakValueDictionary()
        )

    def reset_sweep_cache(
        self,
        pre_sweep_libraries_by_kind: Mapping[str, set[UUID]] | None = None,
    ) -> None:
        """Start a sweep with the library ownership snapshot it inherited."""
        self._sweep_pushed.clear()
        self._pre_sweep_libraries_by_kind = {
            kind: set(library_ids)
            for kind, library_ids in (pre_sweep_libraries_by_kind or {}).items()
        }

    def _identity_lock(self, ref: SourceItemRef) -> asyncio.Lock:
        key = (ref.kind, ref.item_id)
        lock = self._identity_locks.get(key)
        if lock is None:
            lock = asyncio.Lock()
            self._identity_locks[key] = lock
        return lock

    def _validate_ref_identity(self, ref: SourceItemRef) -> None:
        """Reject adapter identity drift before routing, reads, or mutation."""
        if ref.kind not in self._adapter.kinds:
            raise ValueError(
                f"undeclared source kind {ref.kind!r} for adapter {self._adapter.name}"
            )
        expected = self._adapter.external_key(ref.kind, ref.item_id)
        if ref.external_key != expected:
            raise ValueError(f"non-canonical source ref for {ref.kind}:{ref.item_id}")
        parsed = self._adapter.parse_external_key(ref.external_key)
        if parsed != (ref.kind, ref.item_id):
            raise ValueError(f"source ref does not round-trip for {ref.kind}:{ref.item_id}")

    def _validate_materialized_tree(
        self,
        requested_ref: SourceItemRef,
        item: SourceItem,
    ) -> None:
        """Validate a fetched item graph before its resolved route can mutate.

        ``raw`` is an opaque fetch aid and may legitimately be refreshed by an
        adapter. Identity, token, and routing facts are the captured snapshot
        contract: accepting drift in any of them would apply fresh bytes using
        a route and diff decision made for a different source state.
        """
        self._validate_ref_identity(item.ref)
        requested_identity = (
            requested_ref.kind,
            requested_ref.item_id,
            requested_ref.external_key,
        )
        materialized_identity = (
            item.ref.kind,
            item.ref.item_id,
            item.ref.external_key,
        )
        if materialized_identity != requested_identity:
            raise ValueError(
                "materialized source identity does not match requested ref "
                f"{requested_ref.kind}:{requested_ref.item_id}"
            )
        requested_snapshot = (
            requested_ref.change_token,
            requested_ref.dependents_token,
            requested_ref.routing_facts,
            requested_ref.materialization_enforcement,
        )
        materialized_snapshot = (
            item.ref.change_token,
            item.ref.dependents_token,
            item.ref.routing_facts,
            item.ref.materialization_enforcement,
        )
        if materialized_snapshot != requested_snapshot:
            raise ValueError(
                "materialized source snapshot does not match requested ref "
                f"{requested_ref.kind}:{requested_ref.item_id}"
            )

        pending = list(item.dependents)
        while pending:
            dependent = pending.pop()
            self._validate_ref_identity(dependent.ref)
            if dependent.ref.kind not in self._dependent_kinds:
                # A primary kind is proven by the census, not by a parent; a
                # membership claim on it would be meaningless (and this also
                # rejects an item depending on itself).
                raise ValueError(
                    f"dependent {dependent.ref.kind}:{dependent.ref.item_id} has a "
                    f"primary kind of adapter {self._adapter.name}"
                )
            pending.extend(dependent.dependents)

    def _relevant_libraries(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute | None,
        cursor: CursorRow | None,
        locations: list[DocumentLocation],
    ) -> set[UUID]:
        try:
            library_ids = self._router.target_libraries()
        except RoutingError:
            # A delete event remains authoritative when the current routing
            # snapshot itself is unresolved. Durable/cursor/pre-sweep scope is
            # still known and must not become undeletable with the route.
            library_ids = set()
        library_ids.update(self._pre_sweep_libraries_by_kind.get(ref.kind, set()))
        library_ids.update(UUID(location.ironrag_library_id) for location in locations)
        if cursor is not None:
            library_ids.add(UUID(cursor.current_library_id))
        if route is not None:
            library_ids.add(route.library_id)
        return library_ids

    async def _discover_exact_targets(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute | None,
    ) -> _ExactTargetSnapshot:
        """Read the complete authoritative target scope for one exact key.

        This is the sole discovery path used by source deletion and required
        materialization. Scanning only canonical exact keys prevents a stale
        registry id from deleting another source item's bytes.
        """
        cursor = self._state.get(ref.kind, ref.item_id)
        known_locations = tuple(self._state.locations_for(ref.kind, ref.item_id))
        relevant_libraries = self._relevant_libraries(
            ref,
            route,
            cursor,
            list(known_locations),
        )
        documents: dict[UUID, str] = {}
        for library_id in sorted(relevant_libraries, key=str):
            found = await self._ironrag.find_document(library_id, ref.external_key)
            if found is not None:
                documents[library_id] = _validated_document_id(
                    found,
                    library_id=library_id,
                    external_key=ref.external_key,
                )
        return _ExactTargetSnapshot(
            cursor=cursor,
            known_locations=known_locations,
            relevant_libraries=frozenset(relevant_libraries),
            documents=documents,
        )

    def _reconcile_exact_locations(
        self,
        ref: SourceItemRef,
        snapshot: _ExactTargetSnapshot,
    ) -> None:
        """Make the durable registry match a successful exact-key scan.

        A discovered replacement id must be persisted before any later delete
        attempt. If that mutation fails and routing changes before retry, the
        durable location remains the only restart-safe evidence that this
        library still contains source-owned bytes.
        """
        for location in snapshot.known_locations:
            library_id = UUID(location.ironrag_library_id)
            if library_id in snapshot.documents:
                continue
            self._state.remove_location(
                kind=ref.kind,
                item_id=ref.item_id,
                ironrag_library_id=location.ironrag_library_id,
                ironrag_document_id=location.ironrag_document_id,
            )
        for library_id, document_id in snapshot.documents.items():
            self._state.record_document_location(
                kind=ref.kind,
                item_id=ref.item_id,
                external_key=ref.external_key,
                ironrag_document_id=document_id,
                ironrag_library_id=str(library_id),
            )

    async def _delete_exact_targets(
        self,
        ref: SourceItemRef,
        targets: Mapping[UUID, str],
        policy: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
    ) -> str | None:
        """Delete exact-key targets through the canonical orphan mutation."""
        deleted_document_id: str | None = None
        for library_id in sorted(targets, key=str):
            outcome = await self._reap_orphan_locked(
                ref,
                library_id,
                targets[library_id],
                policy,
                enforcement=enforcement,
            )
            deleted_document_id = outcome.ironrag_document_id
        return deleted_document_id

    async def push_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        """Resolve route, fetch payload, push under the kind's policy."""
        self._validate_ref_identity(ref)
        await self._ironrag.ensure_visibility_contract()
        async with self._identity_lock(ref):
            return await self._push_ref_locked(ref)

    async def _resolve_ref_route(
        self,
        ref: SourceItemRef,
        policy: PushPolicy,
    ) -> ResolvedRoute | OrchestrationOutcome:
        try:
            return self._router.resolve(ref)
        except RoutingError as exc:
            log.warning(
                "orchestrator.unrouted",
                kind=ref.kind,
                item_id=ref.item_id,
                reason=str(exc),
            )
            if (
                isinstance(exc, UnroutedError)
                and ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED
            ):
                return await self._apply_fetch_delete(
                    ref,
                    None,
                    policy,
                    SourceFetchDelete(
                        enforcement=SourceDeleteEnforcement.REQUIRED,
                        reason="required source item has no configured route",
                    ),
                )
            return OrchestrationOutcome(
                ref=ref,
                action="unrouted",
                workspace_id=None,
                library_id=None,
                rule_description=None,
                ironrag_document_id=None,
                detail=str(exc),
            )

    def _unchanged_ref_outcome(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute,
        cursor: CursorRow | None,
        current_location: DocumentLocation | None,
    ) -> tuple[OrchestrationOutcome | None, bool]:
        if ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED:
            # An equal source token or an old successful payload receipt cannot
            # prove that this complete projection still owns the public head.
            return None, False
        token_matches = (
            cursor is not None
            and cursor.change_token is not None
            and ref.change_token is not None
            and cursor.change_token == ref.change_token
        )
        if not token_matches:
            return None, False
        assert cursor is not None
        if not self._parent_census_proven(ref):
            # Same primary token, but the dependent set is unproven (never
            # recorded, or the dependents token moved). Fetch so the parent's
            # complete dependent set can be claimed and released; the primary
            # copy itself is confirmed, not rewritten.
            return None, True
        if cursor.current_library_id != str(route.library_id) or current_location is None:
            return None, True
        return (
            OrchestrationOutcome(
                ref=ref,
                action="noop_unchanged",
                workspace_id=route.workspace_id,
                library_id=route.library_id,
                rule_description=route.rule_description,
                ironrag_document_id=current_location.ironrag_document_id,
                detail=f"change_token unchanged ({ref.change_token}); cursor knows document id",
            ),
            False,
        )

    async def _cached_absence_outcome(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute,
        cursor: CursorRow | None,
    ) -> OrchestrationOutcome | None:
        if (
            ref.change_token is None
            or ref.materialization_enforcement is not SourceMaterializationEnforcement.POLICY
        ):
            return None
        known_empty = self._state.get_empty_fetch(ref.kind, ref.item_id)
        if known_empty is None or known_empty.change_token != ref.change_token:
            return None
        known_locations = self._state.locations_for(ref.kind, ref.item_id)
        relevant_libraries = self._relevant_libraries(ref, route, cursor, known_locations)
        if known_empty.scope_fingerprint != _library_scope_fingerprint(relevant_libraries):
            return None
        cached_snapshot = await self._discover_exact_targets(ref, route)
        self._reconcile_exact_locations(ref, cached_snapshot)
        if not cached_snapshot.documents:
            return OrchestrationOutcome(
                ref=ref,
                action="source_absent",
                workspace_id=route.workspace_id,
                library_id=route.library_id,
                rule_description=route.rule_description,
                ironrag_document_id=None,
                detail=(
                    f"authoritative absence at change_token {ref.change_token}; "
                    "exact target absence revalidated"
                ),
            )
        # Enforcement is not stored in the negative cache. Ask the adapter to
        # classify the restored target again before reconciling it.
        self._state.clear_empty_fetch(ref.kind, ref.item_id)
        return None

    async def _prepare_materialization_target(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute,
        policy: PushPolicy,
        *,
        enforcement: SourceMaterializationEnforcement,
        confirm_unchanged_after_materialization: bool,
    ) -> tuple[PushPolicy, bool, OrchestrationOutcome | None]:
        if enforcement is SourceMaterializationEnforcement.REQUIRED:
            return (
                replace(policy, on_changed=UpdateAction.REPLACE),
                False,
                None,
            )
        if not confirm_unchanged_after_materialization:
            return policy, False, None
        found = await self._ironrag.find_document(route.library_id, ref.external_key)
        if found is None:
            return policy, True, None
        document_id = _validated_document_id(
            found,
            library_id=route.library_id,
            external_key=ref.external_key,
        )
        self._state.record_document_location(
            kind=ref.kind,
            item_id=ref.item_id,
            external_key=ref.external_key,
            ironrag_document_id=document_id,
            ironrag_library_id=str(route.library_id),
        )
        return (
            policy,
            False,
            OrchestrationOutcome(
                ref=ref,
                action="noop_unchanged",
                workspace_id=route.workspace_id,
                library_id=route.library_id,
                rule_description=route.rule_description,
                ironrag_document_id=document_id,
                detail=(
                    f"change_token unchanged ({ref.change_token}); server confirms document present"
                ),
            ),
        )

    async def _push_materialized_primary(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        effective_policy: PushPolicy,
        *,
        enforcement: SourceMaterializationEnforcement,
        target_absence_confirmed: bool,
    ) -> OrchestrationOutcome:
        if enforcement is SourceMaterializationEnforcement.REQUIRED:
            return await self._push_required(item, route, effective_policy)
        return await self._push_item_locked(
            item,
            route,
            effective_policy,
            target_absence_confirmed=target_absence_confirmed,
        )

    async def _accept_required_intent(
        self, attempt: MaterializationAttempt
    ) -> MaterializationAttempt | Exception:
        """Replay an unknown intent send without changing its key or captured floor."""
        if attempt.phase == "intent_prepared":
            attempt = self._state.transition_materialization_attempt(
                attempt, phase="intent_submitting"
            )
        if attempt.phase != "intent_submitting":
            return attempt
        try:
            assert attempt.request is not None
            receipt = await self._ironrag.create_visibility_intent(
                attempt.document_id, request=attempt.request
            )
        except (IronRagError, httpx.HTTPError) as exc:
            return exc
        return self._state.transition_materialization_attempt(
            attempt, phase="intent_accepted", receipt=receipt
        )

    async def _reconcile_required_copy(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute,
        library_id: UUID,
        attempt: MaterializationAttempt,
    ) -> str | None:
        """Prove this copy's current intent and historical-route public denial."""
        head = await self._ironrag.get_document(attempt.document_id)
        if head is None:
            self._state.remove_location(
                kind=ref.kind,
                item_id=ref.item_id,
                ironrag_library_id=str(library_id),
                ironrag_document_id=attempt.document_id,
            )
            return "target disappeared during intent reconciliation; refetch required"
        _validated_document_id(head, library_id=library_id, external_key=ref.external_key)
        assert attempt.receipt is not None
        if head.visibility_intent_id != attempt.receipt.visibility_intent_id:
            self._state.transition_materialization_attempt(attempt, phase="intent_superseded")
            return "visibility intent is no longer current; refetch required"
        if library_id != route.library_id and (
            head.readable_revision_id is not None or head.readable_mutation_item_id is not None
        ):
            raise IronRagError("historical route copy is readable despite its retirement intent")
        return None

    async def _push_required(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        policy: PushPolicy,
        parent_external_key: str | None = None,
    ) -> OrchestrationOutcome:
        """Accept independent denial for every exact copy before payload admission."""
        snapshot = await self._discover_exact_targets(item.ref, route)
        self._reconcile_exact_locations(item.ref, snapshot)
        current_id = snapshot.documents.get(route.library_id)
        descriptor = ReplacementIdentity(
            checksum=item.payload_checksum,
            byte_size=len(item.payload),
            mime_type=item.mime_type,
            file_name=item.file_name,
            title=item.title,
            document_hint=item.document_hint,
        )
        observation = self._state.observe_materialization(
            adapter=self._adapter.name,
            kind=item.ref.kind,
            item_id=item.ref.item_id,
            external_key=item.ref.external_key,
            library_id=str(route.library_id),
            target_document_id=current_id,
            materialization_token=item.ref.change_token,
            replacement=descriptor,
        )
        attempts: dict[UUID, MaterializationAttempt] = {}
        unresolved: list[str] = []
        failures: list[Exception] = []
        for library_id, document_id in snapshot.documents.items():
            attempt = self._state.prepare_materialization_attempt(
                observation,
                document_id=document_id,
                library_id=str(library_id),
            )
            if attempt.phase == "intent_superseded":
                unresolved.append("visibility intent superseded; new source observation required")
                continue
            accepted = await self._accept_required_intent(attempt)
            if isinstance(accepted, Exception):
                # Failure on one copy must not starve independent denial of later copies.
                failures.append(accepted)
                continue
            attempt = accepted
            attempts[library_id] = attempt
            try:
                reason = await self._reconcile_required_copy(item.ref, route, library_id, attempt)
            except (IronRagError, httpx.HTTPError) as exc:
                failures.append(exc)
                continue
            if reason is not None:
                unresolved.append(reason)
        if failures:
            raise failures[0]
        if unresolved:
            return _outcome(
                item.ref, route, "skipped_changed", current_id, "; ".join(unresolved), deferred=True
            )
        cache_key = (route.library_id, item.ref.external_key)
        historical = tuple(
            attempt for library, attempt in attempts.items() if library != route.library_id
        )
        if current_id is None:
            return await self._create(
                item,
                route,
                policy,
                cache_key,
                parent_external_key,
                historical_attempts=historical,
                visibility_episode=observation.episode_id,
            )
        return await self._revise(
            item,
            route,
            current_id,
            cache_key,
            parent_external_key,
            visibility_attempt=attempts[route.library_id],
            historical_attempts=historical,
        )

    async def _required_copy_closure(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        current_id: str,
        historical_attempts: tuple[MaterializationAttempt, ...],
    ) -> bool:
        for historical in historical_attempts:
            retired = await self._ironrag.get_document(historical.document_id)
            if retired is None:
                continue
            _validated_document_id(
                retired, library_id=UUID(historical.library_id), external_key=item.ref.external_key
            )
            assert historical.receipt is not None
            if (
                retired.visibility_intent_id != historical.receipt.visibility_intent_id
                or retired.readable_revision_id is not None
                or retired.readable_mutation_item_id is not None
            ):
                raise IronRagError("historical-copy retirement no longer denies public reads")
        snapshot = await self._discover_exact_targets(item.ref, route)
        expected = {
            UUID(library): document
            for library, document, _ in self._required_copy_membership(
                item, route, current_id, historical_attempts
            )
        }
        if snapshot.documents != expected:
            self._reconcile_exact_locations(item.ref, snapshot)
            return False
        return True

    @staticmethod
    def _required_copy_membership(
        item: SourceItem,
        route: ResolvedRoute,
        current_id: str,
        historical_attempts: tuple[MaterializationAttempt, ...],
    ) -> frozenset[tuple[str, str, str]]:
        return frozenset(
            {(str(route.library_id), current_id, item.ref.external_key)}
            | {
                (attempt.library_id, attempt.document_id, item.ref.external_key)
                for attempt in historical_attempts
            }
        )

    def _parent_census_proven(self, ref: SourceItemRef) -> bool:
        """Whether the parent's dependent set is durably proven at these tokens."""
        if not self._dependent_kinds:
            return True
        census = self._state.parent_census(ref.kind, ref.item_id)
        return (
            census is not None
            and census.change_token == ref.change_token
            and ref.dependents_token is not None
            and census.dependents_token == ref.dependents_token
        )

    async def _push_dependents(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        outcomes: list[OrchestrationOutcome],
    ) -> tuple[tuple[OrchestrationOutcome, ...], tuple[MemberTarget, ...]]:
        """Claim and push each dependent, retaining completed outcomes even on failure."""
        members: list[MemberTarget] = []
        for dependent in item.dependents:
            dependent_policy = self._policies.for_kind(dependent.ref.kind)
            try:
                dependent_route = self._router.resolve(dependent.ref)
            except RoutingError:
                dependent_route = route
            member = MemberTarget(
                kind=dependent.ref.kind,
                item_id=dependent.ref.item_id,
                external_key=dependent.ref.external_key,
                ironrag_library_id=str(dependent_route.library_id),
            )
            # The claim precedes the push. A crash between the two leaves a
            # claim without a document, which is harmless; the reverse would
            # leave a live document no parent claims, which the dependent
            # reaper treats as an orphan.
            self._state.claim_member(
                parent_kind=item.ref.kind,
                parent_item_id=item.ref.item_id,
                member=member,
            )
            members.append(member)
            try:
                outcome = await self.push_item(
                    dependent,
                    dependent_route,
                    dependent_policy,
                    parent_external_key=item.ref.external_key,
                )
            except (
                IronRagContentUnsupportedError,
                IronRagContentUnsupportedProblemError,
            ) as exc:
                # The server proved this dependent's content can never be
                # ingested -- whether synchronously (an HTTP 400 admission
                # problem) or as a failed operation; both are the same terminal
                # outcome. It stays an enumerated member -- the claim above
                # keeps any legacy or server copy out of the reaper's reach --
                # but is deliberately not materialized, and it must neither
                # abort the parent nor leave its census unproven. Every other
                # failure still raises: a dependent the server has not proven
                # unsupported must not settle the parent's census.
                outcomes.append(
                    _outcome(
                        dependent.ref,
                        dependent_route,
                        "skipped_unsupported",
                        None,
                        "server-proven content rejection"
                        + (f" ({exc.failure_code})" if exc.failure_code else "")
                        + "; not materialized",
                    )
                )
                continue
            outcomes.append(outcome)
            if outcome.deferred:
                break
        return tuple(outcomes), tuple(members)

    def _cascade_parent_absence(self, ref: SourceItemRef) -> None:
        """Withdraw membership; only a complete dependent sweep proves orphan copies."""
        if not self._dependent_kinds or ref.kind not in self._adapter.primary_kinds:
            return
        released = self._state.release_parent(ref.kind, ref.item_id)
        if released:
            log.info(
                "orchestrator.parent_absent_cascade",
                kind=ref.kind,
                item_id=ref.item_id,
                released=len(released),
            )

    def _fetch_deferred_outcome(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute,
        current_location: DocumentLocation | None,
        deferred: SourceFetchDeferred,
    ) -> OrchestrationOutcome:
        """A read the source declined to answer: nothing is written and nothing is settled."""
        log.info(
            "orchestrator.fetch_deferred",
            kind=ref.kind,
            item_id=ref.item_id,
            reason=deferred.reason,
        )
        return OrchestrationOutcome(
            ref=ref,
            action="fetch_deferred",
            workspace_id=route.workspace_id,
            library_id=route.library_id,
            rule_description=route.rule_description,
            ironrag_document_id=(
                current_location.ironrag_document_id if current_location is not None else None
            ),
            detail=deferred.reason,
            deferred=True,
        )

    async def _push_ref_locked(self, ref: SourceItemRef) -> OrchestrationOutcome:
        """Apply one validated ref while holding its identity mutation lock."""
        policy = self._policies.for_kind(ref.kind)
        route_result = await self._resolve_ref_route(ref, policy)
        if isinstance(route_result, OrchestrationOutcome):
            return route_result
        route = route_result

        cursor = self._state.get(ref.kind, ref.item_id)
        current_location = self._state.get_location(
            ref.kind,
            ref.item_id,
            str(route.library_id),
        )
        (
            unchanged_outcome,
            confirm_unchanged_after_materialization,
        ) = self._unchanged_ref_outcome(
            ref,
            route,
            cursor,
            current_location,
        )
        if unchanged_outcome is not None:
            return unchanged_outcome

        # Only source-authoritative, successfully applied delete decisions enter
        # this cache. Deferred/failed reads never do: retrying them is the only
        # way to avoid freezing stale content behind an ambiguous negative read.
        cached_absence = await self._cached_absence_outcome(ref, route, cursor)
        if cached_absence is not None:
            return cached_absence

        fetch_result = await self._adapter.fetch(ref)
        if isinstance(fetch_result, SourceFetchDeferred):
            return self._fetch_deferred_outcome(ref, route, current_location, fetch_result)
        if isinstance(fetch_result, SourceFetchDelete):
            return await self._apply_fetch_delete(ref, route, policy, fetch_result)
        if isinstance(fetch_result, SourceFetchMaterialized):
            item = fetch_result.item
        else:
            assert_never(fetch_result)

        self._validate_materialized_tree(ref, item)
        if fetch_result.enforcement is not ref.materialization_enforcement:
            raise ValueError(
                "materialization enforcement does not match enumerated ref for "
                f"{ref.kind}:{ref.item_id}"
            )

        (
            effective_policy,
            target_absence_confirmed,
            materialization_outcome,
        ) = await self._prepare_materialization_target(
            ref,
            route,
            policy,
            enforcement=fetch_result.enforcement,
            confirm_unchanged_after_materialization=confirm_unchanged_after_materialization,
        )
        parent_cursor_before_push = self._state.get(ref.kind, ref.item_id)
        if materialization_outcome is not None:
            # Same token and the server confirms the current copy: the primary
            # is settled without a write. The fetch happened for the dependent
            # census below, so do not return before it.
            primary = materialization_outcome
        else:
            # It produced something: any previously established absence is no
            # longer true for this source identity.
            self._state.clear_empty_fetch(ref.kind, ref.item_id)
            try:
                primary = await self._push_materialized_primary(
                    item,
                    route,
                    effective_policy,
                    enforcement=fetch_result.enforcement,
                    target_absence_confirmed=target_absence_confirmed,
                )
            except OrchestrationFailedError as failure:
                completed: list[OrchestrationOutcome] = []
                try:
                    await self._settle_dependents(item, ref, route, completed)
                except Exception as dependent_error:
                    raise OrchestrationFailedError(
                        failure.primary_error, tuple(completed), dependent_error
                    ) from failure.primary_error
                raise OrchestrationFailedError(
                    failure.primary_error, tuple(completed)
                ) from failure.primary_error
        if _primary_deferred(primary):
            log.info(
                "orchestrator.dependents_deferred",
                kind=ref.kind,
                item_id=ref.item_id,
                action=primary.action,
                detail=primary.detail,
            )
            return primary
        if not self._dependent_kinds:
            return primary
        return await self._apply_dependents(item, ref, route, primary, parent_cursor_before_push)

    async def _apply_dependents(
        self,
        item: SourceItem,
        ref: SourceItemRef,
        route: ResolvedRoute,
        primary: OrchestrationOutcome,
        parent_cursor_before_push: CursorRow | None,
    ) -> OrchestrationOutcome:
        """Assemble a successful primary with independently settled dependents.

        Single injection point for parent linkage: every dependent (a page's
        attachments and inline images) is uploaded declaring its source item as
        parent via ``parent_external_key``. The orchestrator already knows the
        parent ref here, so no ``SourceItem.parent`` field is needed -- this is
        the one source of truth for the link. The backend derives
        ``document_role`` (``attached_context`` for image media, ``attachment``
        otherwise) from the declared parent; primary items pass no parent and
        stay ``role=primary``.
        """
        completed: list[OrchestrationOutcome] = []
        has_deferred_dependent = await self._settle_dependents(item, ref, route, completed)
        if has_deferred_dependent:
            self._state.restore(
                parent_cursor_before_push,
                kind=ref.kind,
                item_id=ref.item_id,
            )
            log.info(
                "orchestrator.primary_deferred_by_dependent",
                kind=ref.kind,
                item_id=ref.item_id,
                action=primary.action,
                dependent_count=len(completed),
            )

        return OrchestrationOutcome(
            ref=primary.ref,
            action=primary.action,
            workspace_id=primary.workspace_id,
            library_id=primary.library_id,
            rule_description=primary.rule_description,
            ironrag_document_id=primary.ironrag_document_id,
            detail=(
                f"{primary.detail}; dependent write deferred; retry next sweep"
                if has_deferred_dependent
                else primary.detail
            ),
            deferred=primary.deferred or has_deferred_dependent,
            dependent_outcomes=tuple(completed),
        )

    async def _settle_dependents(
        self,
        item: SourceItem,
        ref: SourceItemRef,
        route: ResolvedRoute,
        completed: list[OrchestrationOutcome],
    ) -> bool:
        """Settle the real fetched set independently of primary success.

        Failure or deferral writes no census. Keep completed outcomes available
        for a failure receipt even when a later dependent raises.
        """
        outcomes, members = await self._push_dependents(item, route, completed)
        if any(dep.deferred for dep in outcomes):
            return True
        # Release membership now, but never infer physical orphanhood from no
        # remaining claims: a legacy owning parent may still be unproven. The
        # completeness-gated dependent reaper owns all released-copy deletion.
        self._state.record_parent_census(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token=ref.change_token,
            dependents_token=ref.dependents_token,
            members=members,
        )
        return False

    @staticmethod
    def _route_context(route: ResolvedRoute | None) -> tuple[UUID | None, UUID | None, str | None]:
        if route is None:
            return None, None, None
        return route.workspace_id, route.library_id, route.rule_description

    def _skipped_fetch_delete_outcome(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute | None,
        decision: SourceFetchDelete,
    ) -> OrchestrationOutcome:
        workspace_id, library_id, rule_description = self._route_context(route)
        current_location = (
            self._state.get_location(ref.kind, ref.item_id, str(route.library_id))
            if route is not None
            else None
        )
        return OrchestrationOutcome(
            ref=ref,
            action="skipped_missing",
            workspace_id=workspace_id,
            library_id=library_id,
            rule_description=rule_description,
            ironrag_document_id=(
                current_location.ironrag_document_id if current_location is not None else None
            ),
            detail=(
                f"source requested policy deletion ({decision.reason}); "
                f"policy on_missing=ignore for kind={ref.kind}"
            ),
            exclusion=decision.exclusion,
        )

    def _record_applied_source_absence(
        self,
        ref: SourceItemRef,
        snapshot: _ExactTargetSnapshot,
        decision: SourceFetchDelete,
    ) -> None:
        for location in snapshot.known_locations:
            self._state.remove_location(
                kind=ref.kind,
                item_id=ref.item_id,
                ironrag_library_id=location.ironrag_library_id,
                ironrag_document_id=location.ironrag_document_id,
            )
        if snapshot.cursor is not None:
            self._state.delete_cursor(ref.kind, ref.item_id)
        if decision.cacheable and ref.change_token is not None:
            self._state.record_empty_fetch(
                kind=ref.kind,
                item_id=ref.item_id,
                change_token=ref.change_token,
                external_key=ref.external_key,
                scope_fingerprint=_library_scope_fingerprint(set(snapshot.relevant_libraries)),
            )

    async def _apply_fetch_delete(
        self,
        ref: SourceItemRef,
        route: ResolvedRoute | None,
        policy: PushPolicy,
        decision: SourceFetchDelete,
    ) -> OrchestrationOutcome:
        """Apply one source-authoritative absence without conflating failures.

        POLICY decisions use the operator's ``on_missing`` setting. REQUIRED
        decisions establish absence even when that setting is ``ignore``. The
        negative-result cache is written only after every known owned document
        has been deleted (or no document existed), so a failed mutation remains
        retryable with the prior cursor intact. A parent that is deleted here
        releases its whole proven dependent set; a parent the policy retains
        keeps its membership, so its dependents stay with it.
        """
        must_delete = _missing_requires_delete(policy, decision.enforcement)
        if not must_delete:
            return self._skipped_fetch_delete_outcome(ref, route, decision)

        snapshot = await self._discover_exact_targets(ref, route)
        self._reconcile_exact_locations(ref, snapshot)
        deleted_document_id = await self._delete_exact_targets(
            ref,
            snapshot.documents,
            policy,
            enforcement=decision.enforcement,
        )

        self._record_applied_source_absence(ref, snapshot, decision)
        self._cascade_parent_absence(ref)

        action: OutcomeAction = "deleted" if snapshot.documents else "source_absent"
        workspace_id, library_id, rule_description = self._route_context(route)
        return OrchestrationOutcome(
            ref=ref,
            action=action,
            workspace_id=workspace_id,
            library_id=library_id,
            rule_description=rule_description,
            ironrag_document_id=deleted_document_id,
            detail=(
                f"source {decision.enforcement.value} deletion applied: {decision.reason}; "
                f"documents={len(snapshot.documents)}"
            ),
            exclusion=decision.exclusion,
        )

    async def push_item(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        policy: PushPolicy,
        parent_external_key: str | None = None,
        *,
        target_absence_confirmed: bool = False,
    ) -> OrchestrationOutcome:
        self._validate_ref_identity(item.ref)
        await self._ironrag.ensure_visibility_contract()
        async with self._identity_lock(item.ref):
            if item.ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED:
                return await self._push_required(item, route, policy, parent_external_key)
            return await self._push_item_locked(
                item,
                route,
                policy,
                parent_external_key,
                target_absence_confirmed=target_absence_confirmed,
            )

    async def _push_item_locked(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        policy: PushPolicy,
        parent_external_key: str | None = None,
        *,
        target_absence_confirmed: bool = False,
    ) -> OrchestrationOutcome:
        """Mutate one target while holding its source identity lock."""
        cache_key = (route.library_id, item.ref.external_key)
        cached = self._sweep_pushed.get(cache_key)
        if cached is not None and cached.change_token == item.ref.change_token:
            return _outcome(
                item.ref,
                route,
                "noop_unchanged",
                cached.document_id,
                "already pushed this external_key earlier in the same sweep",
            )

        # The materialized-copy registry wins over server-side find. It is the
        # durable source of truth for known ownership in this exact library;
        # a cursor only records source/current-route state and cannot represent
        # all copies left by route moves.
        location = self._state.get_location(
            item.ref.kind,
            item.ref.item_id,
            str(route.library_id),
        )
        existing_document_id: str | None = None
        if location is not None:
            existing_document_id = location.ironrag_document_id
        elif not target_absence_confirmed:
            found = await self._ironrag.find_document(route.library_id, item.ref.external_key)
            if found is not None:
                existing_document_id = _validated_document_id(
                    found,
                    library_id=route.library_id,
                    external_key=item.ref.external_key,
                )
                self._state.record_document_location(
                    kind=item.ref.kind,
                    item_id=item.ref.item_id,
                    external_key=item.ref.external_key,
                    ironrag_document_id=existing_document_id,
                    ironrag_library_id=str(route.library_id),
                )

        if existing_document_id is None:
            return await self._create(
                item,
                route,
                policy,
                cache_key,
                parent_external_key,
            )

        if self._copy_is_current(item, route):
            # A proving refetch (parent unchanged, dependent set unproven)
            # reaches here with the same token it already applied: confirm,
            # never rewrite.
            self._sweep_pushed[cache_key] = _SweepPushMemo(
                item.ref.change_token,
                existing_document_id,
            )
            return _outcome(
                item.ref,
                route,
                "noop_unchanged",
                existing_document_id,
                f"change_token unchanged ({item.ref.change_token}); current copy known",
            )

        if policy.on_changed is UpdateAction.SKIP:
            self._state.upsert(
                kind=item.ref.kind,
                item_id=item.ref.item_id,
                change_token=item.ref.change_token,
                external_key=item.ref.external_key,
                ironrag_document_id=existing_document_id,
                ironrag_library_id=str(route.library_id),
            )
            self._sweep_pushed[cache_key] = _SweepPushMemo(
                item.ref.change_token,
                existing_document_id,
            )
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                existing_document_id,
                "policy on_changed=skip",
            )

        return await self._revise(
            item,
            route,
            existing_document_id,
            cache_key,
            parent_external_key,
        )

    def _copy_is_current(self, item: SourceItem, route: ResolvedRoute) -> bool:
        """Whether the applied cursor already covers this item at this route."""
        if item.ref.materialization_enforcement is not SourceMaterializationEnforcement.POLICY:
            return False
        if item.ref.change_token is None:
            return False
        cursor = self._state.get(item.ref.kind, item.ref.item_id)
        return (
            cursor is not None
            and cursor.change_token == item.ref.change_token
            and cursor.current_library_id == str(route.library_id)
        )

    async def _handle_create_duplicate(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        policy: PushPolicy,
        cache_key: tuple[UUID, str],
        exc: IronRagDuplicateContentError,
    ) -> OrchestrationOutcome:
        """Do not adopt another external key's checksum-identical document."""
        existing_id = str(exc.existing_document_id) if exc.existing_document_id else None
        if policy.on_duplicate_content is DuplicateContentAction.FAIL:
            raise IronRagError(
                f"duplicate content for {item.ref.external_key}; existing document {existing_id}"
            ) from exc
        if item.ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED:
            found = await self._ironrag.find_document(route.library_id, item.ref.external_key)
            if found is not None:
                owned_id = _validated_document_id(
                    found, library_id=route.library_id, external_key=item.ref.external_key
                )
                self._state.record_document_location(
                    kind=item.ref.kind,
                    item_id=item.ref.item_id,
                    external_key=item.ref.external_key,
                    ironrag_document_id=owned_id,
                    ironrag_library_id=str(route.library_id),
                )
            return _outcome(
                item.ref,
                route,
                "skipped_duplicate_content",
                None,
                "required create conflicted; exact target reconciliation required",
                deferred=True,
            )
        self._state.upsert(
            kind=item.ref.kind,
            item_id=item.ref.item_id,
            change_token=item.ref.change_token,
            external_key=item.ref.external_key,
            ironrag_document_id=None,
            ironrag_library_id=str(route.library_id),
        )
        self._sweep_pushed[cache_key] = _SweepPushMemo(item.ref.change_token, None)
        log.debug(
            "orchestrator.create.duplicate_content",
            external_key=item.ref.external_key,
            existing_document_id=existing_id,
        )
        return _outcome(
            item.ref,
            route,
            "skipped_duplicate_content",
            None,
            (
                f"identical bytes already in library ({len(item.payload)} bytes); "
                "conflicting document ownership not adopted"
            ),
        )

    async def _prove_created_required_head(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        document: DocumentResource,
        historical_attempts: tuple[MaterializationAttempt, ...],
    ) -> OrchestrationOutcome | None:
        """A create admission is not proof of READY publication or copy closure."""
        new_id = str(document.id)
        self._state.record_document_location(
            kind=item.ref.kind,
            item_id=item.ref.item_id,
            external_key=item.ref.external_key,
            ironrag_document_id=new_id,
            ironrag_library_id=str(route.library_id),
        )
        handle = document.admission
        if (
            handle is None
            or handle.document_id != UUID(new_id)
            or handle.result_revision_id is None
            or handle.mutation_item_id is None
        ):
            raise IronRagError("new required document returned no complete payload admission")
        status = await self._ironrag.wait_for_operation(handle.operation_id)
        head = await self._ironrag.get_document(new_id)
        if (
            status.id != handle.operation_id
            or status.status is not OperationStatusValue.READY
            or head is None
            or head.readable_revision_id != handle.result_revision_id
            or head.readable_mutation_item_id != handle.mutation_item_id
            or head.visibility_intent_id != handle.visibility_intent_id
            or handle.result_checksum != item.payload_checksum
        ):
            return _outcome(
                item.ref,
                route,
                "skipped_new",
                new_id,
                "new document is not proven READY at the current public head",
                deferred=True,
            )
        _validated_document_id(
            head, library_id=route.library_id, external_key=item.ref.external_key
        )
        if not await self._required_copy_closure(item, route, new_id, historical_attempts):
            return _outcome(
                item.ref,
                route,
                "skipped_new",
                new_id,
                "target membership changed during creation; reconcile next sweep",
                deferred=True,
            )
        return None

    async def _create(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        policy: PushPolicy,
        cache_key: tuple[UUID, str],
        parent_external_key: str | None,
        *,
        historical_attempts: tuple[MaterializationAttempt, ...] = (),
        visibility_episode: str | None = None,
    ) -> OrchestrationOutcome:
        if policy.on_new is UpsertAction.SKIP:
            return _outcome(item.ref, route, "skipped_new", None, "policy on_new=skip")
        try:
            document = await self._ironrag.create_document(
                route.library_id,
                external_key=item.ref.external_key,
                file_bytes=item.payload,
                file_name=item.file_name,
                mime_type=item.mime_type,
                title=item.title,
                document_hint=item.document_hint,
                parent_external_key=parent_external_key,
            )
        except IronRagDuplicateContentError as exc:
            return await self._handle_create_duplicate(item, route, policy, cache_key, exc)

        new_id = _validated_document_id(
            document,
            library_id=route.library_id,
            external_key=item.ref.external_key,
        )
        if item.ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED:
            deferred = await self._prove_created_required_head(
                item, route, document, historical_attempts
            )
            if deferred is not None:
                return deferred
        self._state.upsert(
            kind=item.ref.kind,
            item_id=item.ref.item_id,
            change_token=item.ref.change_token,
            external_key=item.ref.external_key,
            ironrag_document_id=new_id,
            ironrag_library_id=str(route.library_id),
            expected_visibility_episode=visibility_episode,
            expected_materialization_attempts=historical_attempts
            if visibility_episode is not None
            else None,
            expected_copy_membership=self._required_copy_membership(
                item, route, new_id, historical_attempts
            )
            if visibility_episode is not None
            else None,
        )
        self._sweep_pushed[cache_key] = _SweepPushMemo(item.ref.change_token, new_id)
        return _outcome(
            item.ref,
            route,
            "created",
            new_id,
            f"created {len(item.payload)} bytes ({item.mime_type})",
        )

    def _prepare_revision_attempt(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        document_id: str,
        visibility_attempt: MaterializationAttempt | None,
    ) -> tuple[MaterializationAttempt, ReplacementIdentity]:
        """Bind one complete immutable body before any send, including ordinary replay."""
        replacement = (
            visibility_attempt.receipt.replacement
            if visibility_attempt is not None and visibility_attempt.receipt is not None
            else ReplacementIdentity(
                checksum=item.payload_checksum,
                byte_size=len(item.payload),
                mime_type=item.mime_type,
                file_name=item.file_name,
                title=item.title,
                document_hint=item.document_hint,
            )
        )
        identity = PayloadIdentity(
            document_id=UUID(document_id),
            operation="replace",
            replacement=replacement,
            # An accepted intent owns its payload namespace for its entire
            # lifetime, including unknown sends. A changed caller namespace
            # cannot rebind that immutable attempt or recapture its floor.
            caller_key=visibility_attempt.payload_identity.caller_key
            if visibility_attempt is not None and visibility_attempt.payload_identity is not None
            else item.idempotency_key,
            visibility_intent_id=visibility_attempt.receipt.visibility_intent_id
            if visibility_attempt is not None and visibility_attempt.receipt is not None
            else None,
        )
        if visibility_attempt is None:
            observation = self._state.observe_materialization(
                adapter=self._adapter.name,
                kind=item.ref.kind,
                item_id=item.ref.item_id,
                external_key=item.ref.external_key,
                library_id=str(route.library_id),
                target_document_id=document_id,
                materialization_token=item.ref.change_token,
                replacement=replacement,
                payload_identity=identity,
            )
            visibility_attempt = self._state.prepare_materialization_attempt(
                observation,
                document_id=document_id,
                library_id=str(route.library_id),
                intent_required=False,
            )
        return self._state.bind_payload_identity(visibility_attempt, identity), replacement

    async def _submit_revision(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        cache_key: tuple[UUID, str],
        parent_external_key: str | None,
        attempt: MaterializationAttempt,
        replacement: ReplacementIdentity,
        bound: bool,
        historical_attempts: tuple[MaterializationAttempt, ...],
    ) -> tuple[OperationHandle, MaterializationAttempt] | OrchestrationOutcome:
        """Submit the durable payload key and persist acceptance before polling."""
        document_id = attempt.document_id
        # ``replace`` is canonical: the server reuses checksum-identical chunks,
        # so re-materializing a source rewrites only the bytes that moved. A
        # source that renders a stable prefix (YouTrack) gets append-like cost
        # without ``append``, which cannot express comment edits or deletes.
        try:
            handle = await self._ironrag.create_revision(
                document_id,
                mode="replace",
                file_bytes=item.payload,
                file_name=replacement.file_name,
                mime_type=replacement.mime_type,
                idempotency_key=attempt.payload_key,
                visibility_intent_id=(
                    attempt.receipt.visibility_intent_id if attempt.receipt is not None else None
                ),
                title=replacement.title,
                document_hint=replacement.document_hint,
                provenance=RevisionProvenance(
                    language_code=replacement.language_code,
                    source_uri=replacement.source_uri,
                ),
            )
            if bound:
                self._validate_bound_payload_receipt(attempt, handle)
            attempt = self._state.record_payload_receipt(attempt, handle)
        except IronRagNotFoundError:
            # The location registry pointed at a doc IronRAG no longer has
            # (manual delete, library reset). Invalidate exactly that location and fall back
            # to create so the next sweep is consistent.
            log.warning(
                "orchestrator.revise_404_invalidates_location",
                external_key=item.ref.external_key,
                stale_document_id=document_id,
            )
            self._state.remove_location(
                kind=item.ref.kind,
                item_id=item.ref.item_id,
                ironrag_library_id=str(route.library_id),
                ironrag_document_id=document_id,
            )
            policy = self._policies.for_kind(item.ref.kind)
            return await self._create(
                item,
                route,
                policy,
                cache_key,
                parent_external_key,
                historical_attempts=historical_attempts,
                visibility_episode=attempt.episode_id if bound else None,
            )
        except IronRagVisibilityConflictError as exc:
            if not bound or exc.problem.code != "visibility_intent_superseded":
                raise
            self._state.transition_materialization_attempt(attempt, phase="intent_superseded")
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                document_id,
                "visibility intent superseded; refetch required",
                deferred=True,
            )
        except IronRagConflictError as exc:
            if exc.problem.code != "conflicting_mutation":
                raise
            # A mutation is already in flight for this document (typed 409,
            # not a regex match on the error message).
            log.info(
                "orchestrator.revise_deferred",
                external_key=item.ref.external_key,
                document_id=document_id,
                reason="conflicting_mutation",
            )
            self._sweep_pushed[cache_key] = _SweepPushMemo(
                item.ref.change_token,
                document_id,
            )
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                document_id,
                "document has a pending mutation; retry next sweep",
                deferred=True,
            )
        return handle, attempt

    async def _poll_revision(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        cache_key: tuple[UUID, str],
        attempt: MaterializationAttempt,
        handle: OperationHandle,
    ) -> OrchestrationOutcome | None:
        """Only a proven terminal result can rotate the durable payload key."""
        document_id = attempt.document_id
        try:
            status = await self._ironrag.wait_for_operation(handle.operation_id)
        except IronRagMutationTimeoutError as exc:
            log.info(
                "orchestrator.revise_deferred",
                external_key=item.ref.external_key,
                document_id=document_id,
                reason="operation_poll_timeout",
                error=str(exc),
            )
            self._sweep_pushed[cache_key] = _SweepPushMemo(
                item.ref.change_token,
                document_id,
            )
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                document_id,
                "revision operation did not reach a terminal state; retry next sweep",
                deferred=True,
            )
        except IronRagOperationFailedError as exc:
            if exc.status.id != handle.operation_id:
                raise IronRagError("revision poll returned mismatched operation") from exc
            if exc.status.status is not OperationStatusValue.FAILED:
                raise IronRagError(
                    "revision failure did not carry a terminal failed status"
                ) from exc
            # Our own submission reached a proven terminal failure. Roll the
            # payload key once so the next sweep submits a fresh key rather than
            # replaying this terminal record; the failure itself still surfaces.
            self._state.transition_materialization_attempt(attempt, phase="payload_terminal")
            if (
                self._dependent_kinds
                and item.ref.kind in self._adapter.primary_kinds
                and item.ref.materialization_enforcement is SourceMaterializationEnforcement.POLICY
                and not isinstance(exc, IronRagContentUnsupportedError)
            ):
                raise OrchestrationFailedError(exc) from exc
            raise

        if status.id != handle.operation_id:
            raise IronRagError("revision poll returned mismatched operation")
        if status.status in (
            OperationStatusValue.SUPERSEDED,
            OperationStatusValue.CANCELED,
        ):
            if status.status is OperationStatusValue.CANCELED:
                # Our own submission was withdrawn: roll the payload key so a
                # later sweep can submit the current desired source afresh.
                self._state.transition_materialization_attempt(attempt, phase="payload_terminal")
            # A SUPERSEDED revision was won by another owner. Without a server
            # receipt naming the current head owner we cannot prove whether a
            # matching READY winner already satisfies this desired source, so we
            # fail closed: retain, never delete, never overwrite the foreign
            # winner, and defer. A changed desired payload already yields a new
            # durable complete-body episode/key, so only the same desired body a
            # foreign winner superseded reach this deferral. This is a known
            # boundary, not a silently assumed reconciliation.
            log.info(
                "orchestrator.revise_deferred",
                external_key=item.ref.external_key,
                document_id=document_id,
                reason=status.status.value,
            )
            self._sweep_pushed[cache_key] = _SweepPushMemo(
                item.ref.change_token,
                document_id,
            )
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                document_id,
                f"revision operation {status.status.value}; retry next sweep",
                deferred=True,
            )

        if status.status is not OperationStatusValue.READY:
            raise IronRagError("revision poll returned mismatched or non-READY operation")
        return None

    async def _prove_revised_required_head(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        document_id: str,
        handle: OperationHandle,
        historical_attempts: tuple[MaterializationAttempt, ...],
    ) -> OrchestrationOutcome | None:
        """Match READY admission to the public owner and every retired copy."""
        head = await self._ironrag.get_document(document_id)
        if head is None or (
            head.visibility_intent_id != handle.visibility_intent_id
            or head.readable_revision_id != handle.result_revision_id
            or head.readable_mutation_item_id != handle.mutation_item_id
        ):
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                document_id,
                "READY receipt does not own the current public head",
                deferred=True,
            )
        _validated_document_id(
            head, library_id=route.library_id, external_key=item.ref.external_key
        )
        if not await self._required_copy_closure(item, route, document_id, historical_attempts):
            return _outcome(
                item.ref,
                route,
                "skipped_changed",
                document_id,
                "target membership changed during publication; reconcile next sweep",
                deferred=True,
            )
        return None

    async def _revise(
        self,
        item: SourceItem,
        route: ResolvedRoute,
        document_id: str,
        cache_key: tuple[UUID, str],
        parent_external_key: str | None,
        *,
        visibility_attempt: MaterializationAttempt | None = None,
        historical_attempts: tuple[MaterializationAttempt, ...] = (),
    ) -> OrchestrationOutcome:
        bound = visibility_attempt is not None
        visibility_attempt, replacement = self._prepare_revision_attempt(
            item, route, document_id, visibility_attempt
        )
        replayed_payload = visibility_attempt.phase == "payload_submitting"
        if visibility_attempt.phase in {"intent_accepted", "payload_terminal"}:
            visibility_attempt = self._state.transition_materialization_attempt(
                visibility_attempt, phase="payload_submitting"
            )
        submission = await self._submit_revision(
            item,
            route,
            cache_key,
            parent_external_key,
            visibility_attempt,
            replacement,
            bound,
            historical_attempts,
        )
        if isinstance(submission, OrchestrationOutcome):
            return submission
        handle, visibility_attempt = submission
        deferred = await self._poll_revision(item, route, cache_key, visibility_attempt, handle)
        if deferred is not None:
            return deferred
        if bound:
            deferred = await self._prove_revised_required_head(
                item, route, document_id, handle, historical_attempts
            )
            if deferred is not None:
                return deferred
        previous_cursor = self._state.get(item.ref.kind, item.ref.item_id)
        self._state.upsert(
            kind=item.ref.kind,
            item_id=item.ref.item_id,
            change_token=item.ref.change_token,
            external_key=item.ref.external_key,
            ironrag_document_id=document_id,
            ironrag_library_id=str(route.library_id),
            expected_visibility_episode=visibility_attempt.episode_id,
            expected_materialization_attempts=(visibility_attempt, *historical_attempts),
            expected_copy_membership=self._required_copy_membership(
                item, route, document_id, historical_attempts
            )
            if bound
            else None,
        )
        self._sweep_pushed[cache_key] = _SweepPushMemo(
            item.ref.change_token,
            document_id,
        )
        return _outcome(
            item.ref,
            route,
            (
                "noop_unchanged"
                if replayed_payload
                and previous_cursor is not None
                and previous_cursor.change_token == item.ref.change_token
                and previous_cursor.current_library_id == str(route.library_id)
                else "replaced"
            ),
            document_id,
            f"replaced {len(item.payload)} bytes ({item.mime_type})",
        )

    @staticmethod
    def _validate_bound_payload_receipt(
        attempt: MaterializationAttempt,
        handle: OperationHandle,
    ) -> None:
        assert attempt.receipt is not None
        if (
            handle.visibility_intent_id != attempt.receipt.visibility_intent_id
            or handle.document_id != UUID(attempt.document_id)
            or handle.result_revision_id is None
            or handle.mutation_item_id is None
            or handle.result_checksum != attempt.receipt.replacement.checksum
            or not handle.request_fingerprint
        ):
            raise IronRagError(
                "replacement admission returned incomplete or mismatched visibility binding"
            )

    async def reap_orphan(
        self,
        ref: SourceItemRef,
        library_id: UUID,
        ironrag_document_id: str,
        policy: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> OrchestrationOutcome:
        self._validate_ref_identity(ref)
        async with self._identity_lock(ref):
            if _missing_requires_delete(policy, enforcement):
                if not self._target_evidence_is_canonical(
                    ref,
                    expected_cursor=expected_cursor,
                    expected_location=expected_location,
                ):
                    return self._deferred_reap_outcome(
                        ref,
                        library_id,
                        "persisted reap evidence disagrees with canonical external key",
                    )
                if not self._state.target_state_matches(
                    kind=ref.kind,
                    item_id=ref.item_id,
                    ironrag_library_id=str(library_id),
                    expected_cursor=expected_cursor,
                    expected_location=expected_location,
                ):
                    return self._deferred_reap_outcome(
                        ref,
                        library_id,
                        "state changed after orphan list; retry next sweep",
                    )
            return await self._reap_orphan_locked(
                ref,
                library_id,
                ironrag_document_id,
                policy,
                enforcement=enforcement,
            )

    async def _reap_orphan_locked(
        self,
        ref: SourceItemRef,
        library_id: UUID,
        ironrag_document_id: str,
        policy: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
    ) -> OrchestrationOutcome:
        """Delete one listed target while its source identity lock is held."""
        if not _missing_requires_delete(policy, enforcement):
            return OrchestrationOutcome(
                ref=ref,
                action="skipped_missing",
                workspace_id=None,
                library_id=library_id,
                rule_description=None,
                ironrag_document_id=ironrag_document_id,
                detail=f"policy on_missing=ignore for kind={ref.kind}",
            )
        idempotency_key = f"{self._adapter.name}:reap:{ref.kind}:{ref.item_id}:{uuid4()}"
        handle = await self._ironrag.delete_document(
            ironrag_document_id, idempotency_key=idempotency_key
        )
        if handle is not None:
            # Delete is async like revisions; polled serially per orphan.
            # The reaper runs after item enumeration and is not on the
            # per-item hot path, so N sequential poll-to-terminal calls for
            # N orphans is an accepted cost, not batched (no batch delete
            # endpoint in the SDK's scope -- see plan S7.2).
            status = await self._ironrag.wait_for_operation(handle.operation_id)
            if status.id != handle.operation_id or status.status is not OperationStatusValue.READY:
                raise IronRagError(
                    f"delete operation {handle.operation_id} ended "
                    f"{status.status.value}; target absence is not established"
                )
        self._state.remove_location(
            kind=ref.kind,
            item_id=ref.item_id,
            ironrag_library_id=str(library_id),
            ironrag_document_id=ironrag_document_id,
        )
        cursor = self._state.get(ref.kind, ref.item_id)
        if cursor is not None and cursor.current_library_id == str(library_id):
            self._state.delete_cursor(ref.kind, ref.item_id)
        # A successful exact-key deletion invalidates any same-sweep push
        # memo for that target. Otherwise a retry can report a noop for a
        # document this very path just removed.
        self._sweep_pushed.pop((library_id, ref.external_key), None)
        return OrchestrationOutcome(
            ref=ref,
            action="deleted",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=ironrag_document_id,
            detail="reaped orphan",
        )

    @staticmethod
    def _target_evidence_is_canonical(
        ref: SourceItemRef,
        *,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> bool:
        return not (
            (expected_cursor is not None and expected_cursor.external_key != ref.external_key)
            or (
                expected_location is not None and expected_location.external_key != ref.external_key
            )
        )

    @staticmethod
    def _deferred_reap_outcome(
        ref: SourceItemRef,
        library_id: UUID,
        detail: str,
    ) -> OrchestrationOutcome:
        return OrchestrationOutcome(
            ref=ref,
            action="fetch_deferred",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=None,
            detail=detail,
            deferred=True,
        )

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
        """Converge registry state after a successful exact scan found no doc.

        The complete-census reaper normally calls :meth:`reap_orphan` for a
        returned document. A migrated cursor can nominate a historical
        library where the exact key is already absent, leaving no document id
        to reap. This sibling boundary applies the same enforcement gate and
        clears only the library whose scan established absence.
        """
        self._validate_ref_identity(ref)
        async with self._identity_lock(ref):
            return self._confirm_orphan_absent_locked(
                ref,
                library_id,
                policy,
                enforcement=enforcement,
                expected_cursor=expected_cursor,
                expected_location=expected_location,
            )

    def _confirm_orphan_absent_locked(
        self,
        ref: SourceItemRef,
        library_id: UUID,
        policy: PushPolicy,
        *,
        enforcement: SourceDeleteEnforcement,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> OrchestrationOutcome:
        if not _missing_requires_delete(policy, enforcement):
            return OrchestrationOutcome(
                ref=ref,
                action="skipped_missing",
                workspace_id=None,
                library_id=library_id,
                rule_description=None,
                ironrag_document_id=None,
                detail=f"policy on_missing=ignore for kind={ref.kind}",
            )
        if not self._target_evidence_is_canonical(
            ref,
            expected_cursor=expected_cursor,
            expected_location=expected_location,
        ):
            return self._deferred_reap_outcome(
                ref,
                library_id,
                "persisted absence evidence disagrees with canonical external key",
            )
        applied = self._state.confirm_library_absence(
            kind=ref.kind,
            item_id=ref.item_id,
            ironrag_library_id=str(library_id),
            expected_cursor=expected_cursor,
            expected_location=expected_location,
        )
        if not applied:
            return self._deferred_reap_outcome(
                ref,
                library_id,
                "state changed after exact absence scan; retry next sweep",
            )
        self._sweep_pushed.pop((library_id, ref.external_key), None)
        return OrchestrationOutcome(
            ref=ref,
            action="source_absent",
            workspace_id=None,
            library_id=library_id,
            rule_description=None,
            ironrag_document_id=None,
            detail="exact reaper scan confirmed orphan target absent",
        )

    async def delete_by_ref(self, ref: SourceItemRef) -> OrchestrationOutcome:
        """Apply a webhook's authoritative delete event with or without a route.

        Used by webhook handlers that receive a vendor delete event:
        translate to a SourceItemRef and call this. The normal ``on_missing``
        policy still decides retain vs delete, but deletion uses the same
        all-relevant-library path as a fetch-driven decision.
        """
        self._validate_ref_identity(ref)
        async with self._identity_lock(ref):
            return await self._delete_by_ref_locked(ref)

    async def _delete_by_ref_locked(self, ref: SourceItemRef) -> OrchestrationOutcome:
        """Apply a validated delete event while holding its identity lock."""
        try:
            route = self._router.resolve(ref)
        except RoutingError as exc:
            route = None
            route_detail = f"; current route unavailable: {exc}"
        else:
            route_detail = ""
        policy = self._policies.for_kind(ref.kind)
        return await self._apply_fetch_delete(
            ref,
            route,
            policy,
            SourceFetchDelete(
                SourceDeleteEnforcement.POLICY,
                f"source delete event{route_detail}",
                cacheable=False,
            ),
        )


def _outcome(
    ref: SourceItemRef,
    route: ResolvedRoute,
    action: OutcomeAction,
    document_id: str | None,
    detail: str,
    *,
    deferred: bool = False,
) -> OrchestrationOutcome:
    return OrchestrationOutcome(
        ref=ref,
        action=action,
        workspace_id=route.workspace_id,
        library_id=route.library_id,
        rule_description=route.rule_description,
        ironrag_document_id=document_id,
        detail=detail,
        deferred=deferred,
    )
