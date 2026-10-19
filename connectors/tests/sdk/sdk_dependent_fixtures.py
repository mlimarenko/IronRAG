"""Shared doubles for the durable dependent lifecycle (orchestrator + sync).

Not a ``test_`` module: it holds no tests, so it neither shifts the collection
floor nor duplicates fixtures between the two SDK test files that need them.
"""

from __future__ import annotations

from collections.abc import AsyncIterator
from dataclasses import dataclass, replace
from typing import NotRequired, TypedDict
from uuid import UUID

from ironrag_connector.ironrag import DocumentResource, RevisionProvenance
from ironrag_connector.routing import ResolvedLibraryTarget, Router, RoutingConfig
from ironrag_connector.source import (
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
    dependents_token_from,
)
from ironrag_connector.sources.echo.adapter import EchoAdapter, EchoPage
from ironrag_connector.state import StateStore

WS = UUID("00000000-0000-0000-0000-000000000099")
LIB = UUID(int=0)
LIB2 = UUID(int=2)


class RevisionRequest(TypedDict):
    """Complete revision admission request accepted by the stateful test servers."""

    mode: str
    idempotency_key: str
    markdown: NotRequired[str | None]
    appended_text: NotRequired[str | None]
    file_bytes: NotRequired[bytes | None]
    file_name: NotRequired[str | None]
    mime_type: NotRequired[str | None]
    visibility_intent_id: NotRequired[UUID | str | None]
    title: NotRequired[str | None]
    document_hint: NotRequired[str | None]
    provenance: NotRequired[RevisionProvenance]
    base_revision_id: NotRequired[UUID | str | None]


def revision_request_identity(
    document_id: UUID | str, request: RevisionRequest
) -> tuple[object, ...]:
    """Replay identity includes every payload field, normalizing only UUID representations."""
    intent = request.get("visibility_intent_id")
    base_revision = request.get("base_revision_id")
    provenance = request.get("provenance", RevisionProvenance())
    return (
        "revision",
        str(document_id),
        request["mode"],
        request.get("markdown"),
        request.get("appended_text"),
        request.get("file_bytes"),
        request.get("file_name"),
        request.get("mime_type"),
        request.get("title"),
        request.get("document_hint"),
        provenance.language_code,
        provenance.source_uri,
        str(intent) if intent else None,
        str(base_revision) if base_revision else None,
    )


@dataclass(frozen=True)
class DependentSpec:
    """One dependent a page materializes: identity, token and bytes."""

    kind: str
    item_id: str
    change_token: str
    payload: bytes = b"\x89PNG\r\n\x1a\n synthetic dependent bytes"
    mime_type: str = "image/png"


def dependents_fingerprint(specs: tuple[DependentSpec, ...]) -> str:
    """What an adapter's dependent census index would fold into the ref."""
    return dependents_token_from(
        f"{spec.kind}:{spec.item_id}:{spec.change_token}" for spec in specs
    )


class DependentPageAdapter(EchoAdapter):
    """Echo pages with configurable dependents and a bounded dependents token.

    ``bounded=False`` mimics an adapter that cannot bound dependent drift and
    leaves ``dependents_token`` unset. ``deferred_pages`` makes ``fetch``
    return :class:`SourceFetchDeferred` for those page ids, and
    ``census_failure`` is raised after enumeration to withdraw the census.
    """

    kinds = ("page", "image", "attachment")

    def __init__(
        self,
        pages: dict[str, EchoPage],
        dependents: dict[str, tuple[DependentSpec, ...]] | None = None,
        *,
        bounded: bool = True,
    ) -> None:
        super().__init__(pages)
        self.dependents: dict[str, tuple[DependentSpec, ...]] = dict(dependents or {})
        self.bounded = bounded
        self.deferred_pages: set[str] = set()
        self.census_failure: Exception | None = None
        self.fetch_calls: list[str] = []

    def set_dependents(self, page_id: str, *specs: DependentSpec) -> None:
        self.dependents[page_id] = tuple(specs)

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind in self.kinds:
            return SourceDeleteEnforcement.POLICY
        raise ValueError(f"unsupported {self.name} item kind: {kind}")

    def dependents_token(self, page_id: str) -> str | None:
        if not self.bounded:
            return None
        return dependents_fingerprint(self.dependents.get(page_id, ()))

    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        async for ref in super().iter_items():
            yield replace(ref, dependents_token=self.dependents_token(ref.item_id))
        if self.census_failure is not None:
            raise self.census_failure

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        self.fetch_calls.append(ref.item_id)
        if ref.item_id in self.deferred_pages:
            return SourceFetchDeferred("synthetic dependent read failure")
        page_result = await super().fetch(ref)
        if not isinstance(page_result, SourceFetchMaterialized):
            return page_result
        materialized = tuple(
            self.materialize(spec) for spec in self.dependents.get(ref.item_id, ())
        )
        return SourceFetchMaterialized(replace(page_result.item, dependents=materialized))

    def materialize(self, spec: DependentSpec) -> SourceItem:
        return SourceItem(
            ref=SourceItemRef(
                item_id=spec.item_id,
                kind=spec.kind,
                external_key=self.external_key(spec.kind, spec.item_id),
                change_token=spec.change_token,
            ),
            payload=spec.payload,
            mime_type=spec.mime_type,
            file_name=f"{spec.item_id}.bin",
            title=spec.item_id,
        )


async def first_ref(adapter: EchoAdapter) -> SourceItemRef:
    async for ref in adapter.iter_items():
        return ref
    raise AssertionError("adapter enumerated nothing")


def echo_page(item_id: str, updated_at: str) -> EchoPage:
    return EchoPage(item_id=item_id, title=f"Page {item_id}", body="hello", updated_at=updated_at)


def dependent_ref(kind: str, item_id: str, change_token: str) -> SourceItemRef:
    return SourceItemRef(
        item_id=item_id,
        kind=kind,
        external_key=f"echo:{kind}:{item_id}",
        change_token=change_token,
    )


def seed_document(
    state: StateStore,
    documents: dict[tuple[UUID, str], DocumentResource],
    ref: SourceItemRef,
    *,
    change_token: str,
    document_id: UUID,
    library_id: UUID = LIB,
) -> None:
    """One materialized copy known to both IronRAG and the cursor registry."""
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token=change_token,
        external_key=ref.external_key,
        ironrag_document_id=str(document_id),
        ironrag_library_id=str(library_id),
    )
    documents[(library_id, ref.external_key)] = DocumentResource(
        id=document_id,
        library_id=library_id,
        external_key=ref.external_key,
        status="ready",
    )


def router_to(library_id: UUID) -> Router:
    library_ref = f"tests/library-{library_id.hex}"
    return Router(
        RoutingConfig.model_validate({"default": {"library": library_ref}}),
        resolved_targets={
            library_ref: ResolvedLibraryTarget(
                library_ref=library_ref,
                workspace_id=WS,
                library_id=library_id,
            )
        },
    )
