"""Contract every connector implements.

A connector wires a single :class:`SourceAdapter` instance into the
framework. The adapter is the only piece of code that knows the vendor
API; everything else (routing, policy, state, sync loop, HTTP server) is
shared infrastructure.

The adapter exchanges three shapes with the framework:

* :class:`SourceItemRef` — lightweight summary used during the diff stage.
  Just enough to decide whether the item needs to be (re)fetched. Must
  carry a ``change_token`` (typically the source's ``updated_at`` or an
  ETag) so the framework can short-circuit unchanged items without
  downloading their payload.
* :data:`SourceFetchResult` — one closed source decision returned by
  :meth:`SourceAdapter.fetch`: materialize bytes, authoritatively delete, or
  defer without changing indexed state.
* :class:`SourceItem` — the fully materialized payload carried by
  :class:`SourceFetchMaterialized`. The framework hands this to the IronRAG
  client.

The ``kind`` field on both shapes is the framework's per-policy bucket
key. A single adapter can emit several kinds (pages, attachments,
images) and the operator can apply different push/delete policies to
each via ``routing.yaml``. The adapter is built by ``build_app`` through the
plugin factory (``adapter_factory(settings, policies)``) after routing is
loaded; an adapter whose projection depends on a per-kind policy field, such
as ``include_comments``, keeps that live table and resolves it at enumeration
and fetch time.
"""

from __future__ import annotations

import hashlib
from collections.abc import AsyncIterator, Iterable
from dataclasses import dataclass, field
from enum import StrEnum
from typing import Any, Protocol, runtime_checkable


class SourceMaterializationEnforcement(StrEnum):
    """Whether a safe source projection may retain older materializations."""

    POLICY = "policy"
    REQUIRED = "required"


@dataclass(frozen=True)
class SourceItemRef:
    """Lightweight handle used during a sweep's diff stage."""

    item_id: str
    """Stable, source-side identity (page id, file path, document key)."""

    kind: str
    """Bucket for per-kind policy. Free-form (e.g. ``page``, ``attachment``)."""

    external_key: str
    """IronRAG identity surface, e.g. ``my-source:page:42``.

    Must be deterministic from ``(kind, item_id)`` for the lifetime of the
    item, so renames and slug changes still resolve to the same IronRAG
    document.
    """

    change_token: str | None = None
    """Opaque token that advances iff the item's content may have changed.

    Typical sources: ``updated_at`` ISO timestamp, ETag, revision counter,
    content hash. If ``None`` the framework cannot diff and will always
    re-fetch.
    """

    routing_facts: dict[str, Any] = field(default_factory=dict)
    """Adapter-emitted facts the Router can match on (shelf, book, tag…)."""

    raw: dict[str, Any] = field(default_factory=dict)
    """Adapter-private snapshot, opaque to the framework.

    Adapters retain only the fields required to validate a later fetch; this
    is not a general-purpose copy of the vendor response or a logging surface.
    """

    materialization_enforcement: SourceMaterializationEnforcement = (
        SourceMaterializationEnforcement.POLICY
    )
    """Per-kind reconciliation contract captured during enumeration.

    ``REQUIRED`` tells the orchestrator that a token can represent removals
    from a composite document. Even an unchanged-token fast path must then
    prove public denial for every exact-key historical-route copy.
    """

    dependents_token: str | None = None
    """Opaque token that advances iff the item's dependent set may have changed.

    ``change_token`` covers the item's own content; a page's attachments can
    change without it moving. An adapter that emits dependent kinds folds an
    independent census of them (attachment ids and versions listed once per
    sweep) into this token. ``None`` means the adapter cannot bound dependent
    drift, so the parent is never proven unchanged and is fetched every sweep.
    Adapters without dependent kinds leave it unset.
    """


@dataclass(frozen=True)
class SourceItem:
    """Full payload ready to be pushed into IronRAG."""

    ref: SourceItemRef
    payload: bytes
    mime_type: str
    file_name: str
    title: str | None = None
    document_hint: str | None = None
    """Free-form user-facing label IronRAG surfaces to MCP agents in citations.

    This is not required to be a URL. If omitted, IronRAG falls back to
    ``source_uri`` or ``document.title``.
    """

    idempotency_key: str | None = None
    """Optional logical namespace for durable replacement replay.

    The state store persists opaque transport keys for complete payload identities
    before submission. Ordinary metadata or namespace changes start a new episode;
    unchanged requests replay the stored key after an unknown transport outcome.
    An accepted REQUIRED intent retains its first bound namespace and payload key
    despite later namespace changes, without accepting a new intent or floor.
    Creation identifies documents by external key, not by this field; this field
    does not promise content deduplication across parents.
    """

    dependents: tuple[SourceItem, ...] = ()
    """Items derived from this one (a page's attachments and inline images).

    Returned inline so the orchestrator can push them in the same
    transaction as their parent. Dependents are themselves valid
    SourceItems and obey their own ``kind``'s policy.

    This tuple is the parent's complete dependent census: a dependent the
    source still exposes that is missing here is treated as gone and is
    released from the parent's durable membership. An adapter that could not
    read one dependent must return :class:`SourceFetchDeferred` for the whole
    parent instead of a shorter tuple. Only a deliberate exclusion (an
    operator size ceiling, bytes the source does not serve) may omit an item.
    """

    @property
    def payload_checksum(self) -> str:
        """Canonical checksum of actual bytes, independent of source tokens."""
        return "sha256:" + hashlib.sha256(self.payload).hexdigest()


@dataclass(frozen=True)
class SourceFetchMaterialized:
    """The source item was read completely and is ready to ingest.

    ``POLICY`` applies the configured ``on_changed`` and ``on_missing``
    choices. ``REQUIRED`` means the materialized bytes are the complete safe
    projection: retaining an older revision or a copy in a historical route
    could retain content that the source no longer exposes. The orchestrator
    therefore accepts independent visibility denial for every exact-key copy,
    replaces the current-route document, and proves its READY publication and
    historical-copy public unreadability before recording the new source token.
    Historical blobs may remain in privileged immutable archives; retirement
    neither deletes a foreign mutation winner nor replaces its old-route bytes.

    ``on_new`` remains authoritative when no copy exists anywhere: choosing
    not to create a new document cannot retain stale bytes.
    """

    item: SourceItem
    enforcement: SourceMaterializationEnforcement = SourceMaterializationEnforcement.POLICY


class SourceDeleteEnforcement(StrEnum):
    """How a source-authoritative absence interacts with ``on_missing``."""

    POLICY = "policy"
    REQUIRED = "required"


class SourceExclusion(StrEnum):
    """Why a required delete happened, when ``reason`` is not machine-readable.

    ``NONE`` is the ordinary case: absence, post-enumeration unreadability, or
    any other cause that needs no dedicated report counter.
    ``LIMITED_VISIBILITY`` marks a delete forced by a limited or unknown
    visibility projection, which the sync report counts separately instead of
    letting it disappear into ``reaped`` or ``skipped_empty``.
    """

    NONE = "none"
    LIMITED_VISIBILITY = "limited_visibility"


@dataclass(frozen=True)
class SourceFetchDelete:
    """The source authoritatively requires the indexed document to be absent.

    ``POLICY`` means normal source disappearance, so ``on_missing`` decides
    whether an existing document is retained. ``REQUIRED`` means retaining the
    old document would be false or unsafe (for example, text changed into an
    unindexable binary blob), so deletion overrides ``on_missing=ignore``.

    ``cacheable`` is valid only when the decision is tied to the ref's
    ``change_token``. The orchestrator records it only after the required target
    state has been established successfully.

    ``exclusion`` names why a REQUIRED delete happened, separately from
    ``reason``, so the framework can count causes it must surface without
    parsing prose.
    """

    enforcement: SourceDeleteEnforcement
    reason: str
    cacheable: bool = False
    exclusion: SourceExclusion = SourceExclusion.NONE


@dataclass(frozen=True)
class SourceFetchDeferred:
    """The source cannot make an authoritative decision in this sweep."""

    reason: str


type SourceFetchResult = SourceFetchMaterialized | SourceFetchDelete | SourceFetchDeferred


class IncompleteCensusError(RuntimeError):
    """``iter_items`` cannot prove that every live primary item was yielded.

    The census contract is: an ``iter_items`` generator that finishes normally
    has enumerated every live primary item, so a previously materialized
    document whose item was not seen is an orphan the reaper may delete. An
    adapter that loses that proof part-way (a repository it could not read, a
    listing page it could not parse) keeps yielding what it *can* enumerate,
    then raises this error instead of finishing normally. The framework pushes
    the enumerated items and skips the reaper for that sweep: absence from an
    incomplete census is not evidence of deletion.

    Deliberate exclusion by operator configuration (include/exclude rules, a
    size ceiling, a repository or branch list) is not incompleteness. The
    census is complete over the admitted scope, and a document whose item left
    that scope is handled by the kind's ``on_missing`` policy.

    ``scopes`` names the units the adapter could not enumerate (a repository, a
    branch, a listing page) for the operator log; ``reason`` is the summary.
    """

    def __init__(self, reason: str, *, scopes: tuple[str, ...] = ()) -> None:
        super().__init__(reason)
        self.reason = reason
        self.scopes = scopes


def dependents_token_from(entries: Iterable[str]) -> str:
    """Fold one sweep's dependent census entries into a ``dependents_token``.

    Order-insensitive so listing order cannot masquerade as change.
    """
    canonical = "\n".join(sorted(entries))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()[:32]


def dependent_kinds(adapter: SourceAdapter) -> tuple[str, ...]:
    """Kinds reached only through a parent's fetch, in declaration order."""
    return tuple(kind for kind in adapter.kinds if kind not in adapter.primary_kinds)


@runtime_checkable
class SourceAdapter(Protocol):
    """Vendor-specific code lives here. Everything else is framework."""

    name: str
    """Short slug, e.g. ``my-source``. Used as the external-key prefix."""

    kinds: tuple[str, ...]
    """Every ``kind`` value the adapter can emit (primary + dependent)."""

    primary_kinds: tuple[str, ...]
    """Kinds the adapter enumerates from ``iter_items()`` directly.

    A primary kind is proven by the census: an item not seen during a
    complete sweep is an orphan. Every other declared kind is a dependent
    kind (see :func:`dependent_kinds`), reached only through a parent's
    fetch and proven by that parent: the framework records each parent's
    complete dependent set durably and deletes a dependent copy once no live
    parent claims it. A kind reached only through a parent must not be
    declared primary, or the reaper would delete it for not appearing in
    ``iter_items``.
    """

    def iter_items(self) -> AsyncIterator[SourceItemRef]:
        """Yield every primary item visible in the source.

        Finishing normally is the adapter's proof that every live primary
        item was yielded; only then may the reaper delete a previously
        materialized document that was not seen. An adapter that cannot give
        that proof yields what it did enumerate and raises
        :class:`IncompleteCensusError`. Any other exception is a failed
        enumeration and is likewise not proof of absence. Items excluded by
        operator configuration are outside the census scope, not missing
        from it.

        Dependents (a page's attachments, a doc's images) should NOT be
        yielded here — they ride along with their parent via
        :attr:`SourceItem.dependents`. The primary census reaper scans primary
        kinds only; dependent kinds are reaped against the durable parent
        membership once every enumerated parent has a recorded census. An
        adapter with dependent kinds sets :attr:`SourceItemRef.dependents_token`
        on each parent ref so an unchanged parent whose dependents changed is
        fetched and re-proven.
        """
        raise NotImplementedError

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        """Return one explicit, authoritative decision for ``ref``.

        Adapters must not encode vendor failures or ambiguity as deletion.
        Transient failures return :class:`SourceFetchDeferred`; exceptions are
        reserved for errors the sync report should count as failures.
        """
        ...

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        """Return the complete-census deletion contract for ``kind``.

        ``REQUIRED`` means an item absent from a successfully completed
        primary census must be removed even when ``on_missing=ignore``. Every
        adapter implements this explicitly so a newly added kind cannot
        silently inherit unsafe orphan behaviour.
        """
        ...

    def external_key(self, kind: str, item_id: str) -> str:
        """Compose the canonical IronRAG external_key for an item.

        Used by the reaper (which only has the external key from IronRAG)
        to round-trip back to a ``(kind, item_id)`` pair via
        :meth:`parse_external_key`.
        """
        ...

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        """Inverse of :meth:`external_key`. Returns ``(kind, item_id)`` or
        ``None`` if the key was not minted by this adapter."""
        ...

    async def close(self) -> None:
        """Release any HTTP clients or file handles the adapter holds."""
        ...
