from __future__ import annotations

import asyncio
import sqlite3
from collections.abc import AsyncIterator, Callable
from dataclasses import replace
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Unpack
from uuid import UUID

import httpx
import pytest
from sdk_dependent_fixtures import (
    DependentPageAdapter,
    DependentSpec,
    RevisionRequest,
    dependent_ref,
    echo_page,
    first_ref,
    revision_request_identity,
    seed_document,
)

from ironrag_connector.config import BaseConnectorSettings
from ironrag_connector.ironrag import (
    DocumentResource,
    IronRagClient,
    IronRagConflictError,
    IronRagDuplicateContentError,
    IronRagError,
    IronRagMutationTimeoutError,
    IronRagNotFoundError,
    IronRagOperationFailedError,
    IronRagVisibilityConflictError,
    OperationHandle,
    OperationProgress,
    OperationStatus,
    OperationStatusValue,
    ProblemDetails,
    ReplacementIdentity,
    RevisionProvenance,
    VisibilityIntentReceipt,
    VisibilityIntentRequest,
)
from ironrag_connector.orchestrator import OrchestrationFailedError, Orchestrator
from ironrag_connector.policy import (
    DeleteAction,
    DuplicateContentAction,
    PushPolicy,
    UpdateAction,
    UpsertAction,
)
from ironrag_connector.routing import (
    PolicyOverrides,
    ResolvedLibraryTarget,
    Router,
    RoutingConfig,
)
from ironrag_connector.source import (
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
    SourceMaterializationEnforcement,
)
from ironrag_connector.sources.bookstack.mapping import (
    build_external_key as build_bookstack_external_key,
)
from ironrag_connector.sources.bookstack.mapping import (
    parse_external_key as parse_bookstack_external_key,
)
from ironrag_connector.sources.confluence.mapping import (
    build_external_key as build_confluence_external_key,
)
from ironrag_connector.sources.confluence.mapping import (
    parse_external_key as parse_confluence_external_key,
)
from ironrag_connector.sources.echo.adapter import EchoAdapter, EchoPage
from ironrag_connector.sources.gitrepos.mapping import (
    build_external_key as build_gitrepos_external_key,
)
from ironrag_connector.sources.gitrepos.mapping import file_item_id
from ironrag_connector.sources.gitrepos.mapping import (
    parse_external_key as parse_gitrepos_external_key,
)
from ironrag_connector.state import MemberTarget, StateStore
from ironrag_connector.sync import SyncManager

WS = UUID("00000000-0000-0000-0000-000000000099")
LIB = UUID("00000000-0000-0000-0000-000000000000")
LIB2 = UUID("00000000-0000-0000-0000-000000000002")
LIB3 = UUID("00000000-0000-0000-0000-000000000003")
LIB4 = UUID("00000000-0000-0000-0000-000000000004")
LIB5 = UUID("00000000-0000-0000-0000-000000000005")


def _doc_id(n: int) -> UUID:
    """Deterministic synthetic document/operation id for test fixtures."""
    return UUID(int=n)


DOC_PRE = _doc_id(1)
DOC_OLD = _doc_id(2)
DOC_IMAGE_PRE = _doc_id(3)


def _document(doc_id: UUID, library_id: UUID, external_key: str) -> DocumentResource:
    return DocumentResource(
        id=doc_id, library_id=library_id, external_key=external_key, status="ready"
    )


def _duplicate_content_error(existing_id: UUID) -> IronRagDuplicateContentError:
    problem = ProblemDetails(
        type="urn:ironrag:error:duplicate_content",
        title="Duplicate Content",
        status=409,
        detail="synthetic duplicate content",
        code="duplicate_content",
        existing_document_id=existing_id,
    )
    return IronRagDuplicateContentError(problem)


def _conflict_error() -> IronRagConflictError:
    problem = ProblemDetails(
        type="urn:ironrag:error:conflicting_mutation",
        title="Conflicting Mutation",
        status=409,
        detail="synthetic conflicting mutation",
        code="conflicting_mutation",
    )
    return IronRagConflictError(problem)


def _not_found_error() -> IronRagNotFoundError:
    problem = ProblemDetails(
        type="urn:ironrag:error:not_found",
        title="Not Found",
        status=404,
        detail="synthetic not found",
        code="not_found",
    )
    return IronRagNotFoundError(problem)


def _ready_operation(op_id: UUID) -> OperationStatus:
    return OperationStatus(
        id=op_id,
        workspace_id=WS,
        library_id=LIB,
        operation_kind="revise_document",
        status=OperationStatusValue.READY,
        created_at=datetime.now(tz=UTC),
        progress=OperationProgress(),
    )


class FakeIronRag:
    """In-memory double for :class:`IronRagClient` matching its redesigned
    interface: synchronous ``create_document``, async ``create_revision``/
    ``delete_document`` (submit -> poll via ``wait_for_operation``), and
    ``find_document``/``get_document``/``list_documents`` reads."""

    def __init__(self) -> None:
        self.documents: dict[tuple[UUID, str], DocumentResource] = {}
        self.creates: list[dict[str, Any]] = []
        self.revisions: list[dict[str, Any]] = []
        self.deletes: list[str] = []
        self.duplicate_for_key: str | None = None
        self.revision_conflicts: set[str] = set()
        self.find_calls = 0
        self._next_doc_id = 100
        self._next_op_id = 1000
        self._duplicate_existing_id: UUID | None = None
        self._operations: dict[UUID, OperationStatus] = {}
        self.visibility_intents: dict[str, VisibilityIntentReceipt] = {}
        self._visibility_bodies: dict[str, tuple[UUID, VisibilityIntentRequest]] = {}
        self.visibility_requests: list[VisibilityIntentRequest] = []
        self._payload_handles: dict[tuple[str, str], OperationHandle] = {}
        self._payload_bodies: dict[str, tuple[Any, ...]] = {}
        self._published_operations: set[UUID] = set()

    async def ensure_visibility_contract(self) -> None:
        return None

    async def create_visibility_intent(
        self,
        document_id: UUID | str,
        *,
        request: VisibilityIntentRequest,
    ) -> VisibilityIntentReceipt:
        self.visibility_requests.append(request)
        body = (UUID(str(document_id)), request)
        if request.idempotency_key in self.visibility_intents:
            receipt = self.visibility_intents[request.idempotency_key]
            if self._visibility_bodies[request.idempotency_key] != body:
                raise IronRagConflictError(
                    ProblemDetails(
                        type="urn:ironrag:error:idempotency_conflict",
                        title="Idempotency conflict",
                        status=409,
                        code="idempotency_conflict",
                        detail="complete visibility identity differs",
                    )
                )
            return receipt
        item_id = self._new_op_id()
        receipt = VisibilityIntentReceipt(
            visibility_intent_id=item_id,
            mutation_item_id=item_id,
            mutation_id=self._new_op_id(),
            async_operation_id=self._new_op_id(),
            document_id=UUID(str(document_id)),
            revision_floor=len(self.revisions) + 1,
            source_identity=request.source_identity,
            materialization_token=request.materialization_token,
            replacement=request.replacement,
            request_fingerprint="opaque-server-visibility-fingerprint",
        )
        self.visibility_intents[request.idempotency_key] = receipt
        self._visibility_bodies[request.idempotency_key] = body
        for key, doc in self.documents.items():
            if str(doc.id) == str(document_id):
                self.documents[key] = doc.model_copy(
                    update={
                        "visibility_intent_id": item_id,
                        "readable_revision_id": None,
                        "readable_mutation_item_id": None,
                    }
                )
        return receipt

    def _new_doc_id(self) -> UUID:
        doc_id = _doc_id(self._next_doc_id)
        self._next_doc_id += 1
        return doc_id

    def _new_op_id(self) -> UUID:
        op_id = _doc_id(self._next_op_id)
        self._next_op_id += 1
        return op_id

    async def find_document(self, library_id: UUID, external_key: str) -> DocumentResource | None:
        self.find_calls += 1
        return self.documents.get((library_id, external_key))

    async def get_document(self, document_id: UUID | str) -> DocumentResource | None:
        for doc in self.documents.values():
            if str(doc.id) == str(document_id):
                return doc
        return None

    async def create_document(
        self,
        library_id: UUID,
        *,
        external_key: str,
        file_bytes: bytes | None = None,
        file_name: str | None = None,
        mime_type: str | None = None,
        title: str | None = None,
        document_hint: str | None = None,
        parent_external_key: str | None = None,
    ) -> DocumentResource:
        self.creates.append(
            {
                "library_id": library_id,
                "external_key": external_key,
                "size": len(file_bytes) if file_bytes is not None else 0,
                "mime_type": mime_type,
                "document_hint": document_hint,
                "parent_external_key": parent_external_key,
            }
        )
        if self.duplicate_for_key == external_key:
            if self._duplicate_existing_id is None:
                self._duplicate_existing_id = self._new_doc_id()
            raise _duplicate_content_error(self._duplicate_existing_id)
        doc = DocumentResource(
            id=self._new_doc_id(),
            library_id=library_id,
            external_key=external_key,
            status="ready",
            file_name=file_name,
            document_hint=document_hint,
        )
        import hashlib

        operation_id = self._new_op_id()
        admission = OperationHandle(
            operation_id=operation_id,
            document_id=doc.id,
            result_revision_id=self._new_op_id(),
            mutation_item_id=self._new_op_id(),
            result_checksum="sha256:" + hashlib.sha256(file_bytes or b"").hexdigest(),
        )
        self._operations[operation_id] = _ready_operation(operation_id)
        doc = doc.model_copy(
            update={
                "admission": admission,
                "readable_revision_id": admission.result_revision_id,
                "readable_mutation_item_id": admission.mutation_item_id,
            }
        )
        self.documents[(library_id, external_key)] = doc
        return doc

    async def create_revision(
        self,
        document_id: UUID | str,
        **request: Unpack[RevisionRequest],
    ) -> OperationHandle:
        doc_id_str = str(document_id)
        body = revision_request_identity(document_id, request)
        idempotency_key = request["idempotency_key"]
        file_bytes = request.get("file_bytes")
        visibility_intent_id = request.get("visibility_intent_id")
        if (
            idempotency_key in self._payload_bodies
            and self._payload_bodies[idempotency_key] != body
        ):
            raise IronRagConflictError(
                ProblemDetails(
                    type="urn:ironrag:error:idempotency_conflict",
                    title="Idempotency conflict",
                    status=409,
                    code="idempotency_conflict",
                    detail="complete request identity differs",
                )
            )
        if doc_id_str in self.revision_conflicts:
            raise _conflict_error()
        if not any(str(doc.id) == doc_id_str for doc in self.documents.values()):
            raise _not_found_error()
        size = (
            len(file_bytes)
            if file_bytes is not None
            else len(request.get("appended_text") or request.get("markdown") or "")
        )
        self.revisions.append(
            {
                "document_id": doc_id_str,
                "mode": request["mode"],
                "size": size,
                "idempotency_key": idempotency_key,
                "visibility_intent_id": visibility_intent_id,
            }
        )
        replay_key = (doc_id_str, idempotency_key)
        if replay_key in self._payload_handles:
            return self._payload_handles[replay_key]
        op_id = self._new_op_id()
        self._operations[op_id] = _ready_operation(op_id)
        import hashlib

        handle = OperationHandle(
            operation_id=op_id,
            document_id=UUID(doc_id_str),
            visibility_intent_id=visibility_intent_id,
            mutation_item_id=self._new_op_id(),
            result_revision_id=self._new_op_id(),
            result_checksum="sha256:" + hashlib.sha256(file_bytes or b"").hexdigest(),
            request_fingerprint="opaque-server-payload-fingerprint",
        )
        self._payload_handles[replay_key] = handle
        self._payload_bodies[idempotency_key] = body
        return handle

    async def delete_document(
        self, document_id: UUID | str, *, idempotency_key: str
    ) -> OperationHandle | None:
        found_key = None
        for key, doc in self.documents.items():
            if str(doc.id) == str(document_id):
                found_key = key
                break
        if found_key is None:
            return None
        del self.documents[found_key]
        self.deletes.append(str(document_id))
        op_id = self._new_op_id()
        self._operations[op_id] = _ready_operation(op_id)
        return OperationHandle(operation_id=op_id)

    async def wait_for_operation(
        self,
        operation_id: UUID | str,
        *,
        poll_interval: float | None = None,
        budget: float | None = None,
    ) -> OperationStatus:
        status = self._operations[UUID(str(operation_id))]
        if (
            status.status is OperationStatusValue.READY
            and status.id not in self._published_operations
        ):
            self._published_operations.add(status.id)
            for (document_id, _), handle in self._payload_handles.items():
                if handle.operation_id != status.id:
                    continue
                for key, doc in self.documents.items():
                    if (
                        str(doc.id) == document_id
                        and doc.visibility_intent_id == handle.visibility_intent_id
                    ):
                        self.documents[key] = doc.model_copy(
                            update={
                                "readable_revision_id": handle.result_revision_id,
                                "readable_mutation_item_id": handle.mutation_item_id,
                            }
                        )
        return status

    async def list_documents(
        self,
        library_id: UUID,
        *,
        search: str | None = None,
        external_key: str | None = None,
        status: Any = (),
        include_deleted: bool = False,
        limit: int = 200,
    ) -> AsyncIterator[DocumentResource]:
        for (lib, key), doc in list(self.documents.items()):
            if lib != library_id:
                continue
            if external_key and key != external_key:
                continue
            yield doc


class CancelledRevisionPoll(FakeIronRag):
    def __init__(self, *, block_delete: bool = False) -> None:
        super().__init__()
        self.revision_operation_id: UUID | None = None
        self.revision_waiting = asyncio.Event()
        self.delete_waiting = asyncio.Event()
        self.allow_delete = asyncio.Event()
        self.block_delete = block_delete

    async def create_revision(
        self,
        document_id: UUID | str,
        **kwargs: Any,
    ) -> OperationHandle:
        handle = await super().create_revision(document_id, **kwargs)
        self.revision_operation_id = handle.operation_id
        return handle

    async def wait_for_operation(
        self,
        operation_id: UUID | str,
        *,
        poll_interval: float | None = None,
        budget: float | None = None,
    ) -> OperationStatus:
        if UUID(str(operation_id)) == self.revision_operation_id:
            self.revision_waiting.set()
            await asyncio.Event().wait()
            raise AssertionError("unreachable")
        if self.block_delete and self.revision_operation_id is not None:
            self.delete_waiting.set()
            await self.allow_delete.wait()
        return await super().wait_for_operation(
            operation_id,
            poll_interval=poll_interval,
            budget=budget,
        )


class PayloadTrackingIronRag(FakeIronRag):
    """Track the bytes behind a fake document for stale-content assertions."""

    def __init__(self) -> None:
        super().__init__()
        self.payloads: dict[tuple[UUID, str], bytes] = {}

    async def create_document(
        self,
        library_id: UUID,
        *,
        external_key: str,
        file_bytes: bytes | None = None,
        file_name: str | None = None,
        mime_type: str | None = None,
        title: str | None = None,
        document_hint: str | None = None,
        parent_external_key: str | None = None,
    ) -> DocumentResource:
        document = await super().create_document(
            library_id,
            external_key=external_key,
            file_bytes=file_bytes,
            file_name=file_name,
            mime_type=mime_type,
            title=title,
            document_hint=document_hint,
            parent_external_key=parent_external_key,
        )
        self.payloads[(library_id, external_key)] = file_bytes or b""
        return document

    async def create_revision(
        self,
        document_id: UUID | str,
        **request: Unpack[RevisionRequest],
    ) -> OperationHandle:
        document_key = next(
            key for key, document in self.documents.items() if str(document.id) == str(document_id)
        )
        handle = await super().create_revision(document_id, **request)
        self.payloads[document_key] = request.get("file_bytes") or b""
        return handle

    async def delete_document(
        self,
        document_id: UUID | str,
        *,
        idempotency_key: str,
    ) -> OperationHandle | None:
        document_key = next(
            (
                key
                for key, document in self.documents.items()
                if str(document.id) == str(document_id)
            ),
            None,
        )
        handle = await super().delete_document(
            document_id,
            idempotency_key=idempotency_key,
        )
        if document_key is not None:
            self.payloads.pop(document_key, None)
        return handle


def _library_ref(library_id: UUID) -> str:
    return f"tests/library-{library_id.hex}"


def _router_to(library_id: UUID) -> Router:
    library_ref = _library_ref(library_id)
    config = RoutingConfig.model_validate({"default": {"library": library_ref}})
    return Router(
        config,
        resolved_targets={
            library_ref: ResolvedLibraryTarget(
                library_ref=library_ref,
                workspace_id=WS,
                library_id=library_id,
            )
        },
    )


def _router() -> Router:
    return _router_to(LIB)


def _unroutable_router() -> Router:
    """Routing snapshot after the only matching rule has been removed."""
    return Router(RoutingConfig.model_validate({}), resolved_targets={})


def _router_with_configured_secondary(
    default_library_id: UUID,
    secondary_library_id: UUID,
) -> Router:
    default_ref = _library_ref(default_library_id)
    secondary_ref = _library_ref(secondary_library_id)
    config = RoutingConfig.model_validate(
        {
            "default": {"library": default_ref},
            "rules": [
                {
                    "description": "configured secondary",
                    "match": {"destination": "secondary"},
                    "target": {"library": secondary_ref},
                }
            ],
        }
    )
    return Router(
        config,
        resolved_targets={
            default_ref: ResolvedLibraryTarget(
                library_ref=default_ref,
                workspace_id=WS,
                library_id=default_library_id,
            ),
            secondary_ref: ResolvedLibraryTarget(
                library_ref=secondary_ref,
                workspace_id=WS,
                library_id=secondary_library_id,
            ),
        },
    )


def _policies(default: PushPolicy | None = None) -> PolicyOverrides:
    return PolicyOverrides(default=default or PushPolicy(), by_kind={})


def _state(tmp_path: Path) -> StateStore:
    return StateStore(tmp_path / "s.sqlite")


def _create_legacy_cursor_db(
    db_path: Path,
    *,
    kind: str,
    item_id: str,
    change_token: str,
    external_key: str,
    ironrag_document_id: UUID,
) -> None:
    conn = sqlite3.connect(db_path)
    try:
        conn.execute(
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
            )
            """
        )
        conn.execute(
            "INSERT INTO cursor VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                kind,
                item_id,
                change_token,
                external_key,
                str(ironrag_document_id),
                str(LIB),
                "2026-01-01T00:00:00+00:00",
            ),
        )
        conn.commit()
    finally:
        conn.close()


def _orchestrator(
    adapter: EchoAdapter,
    ironrag: FakeIronRag,
    state: StateStore,
    *,
    router: Router | None = None,
    policy: PushPolicy | None = None,
) -> Orchestrator:
    return Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router or _router(),
        state=state,
        policies=_policies(policy),
    )


def _page_ref(change_token: str) -> SourceItemRef:
    return SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token=change_token,
    )


def _seed_existing_page(
    state: StateStore,
    ironrag: FakeIronRag,
    ref: SourceItemRef,
    *,
    change_token: str,
    document_id: UUID = DOC_PRE,
    remember_location: bool = True,
) -> None:
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token=change_token,
        external_key=ref.external_key,
        ironrag_document_id=str(document_id) if remember_location else None,
        ironrag_library_id=str(LIB),
    )
    ironrag.documents[(LIB, ref.external_key)] = _document(
        document_id,
        LIB,
        ref.external_key,
    )


class PageWithImageAdapter(EchoAdapter):
    kinds = ("page", "image")

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        page_result = await super().fetch(ref)
        if not isinstance(page_result, SourceFetchMaterialized):
            return page_result
        page = page_result.item
        image = SourceItem(
            ref=SourceItemRef(
                item_id="img-1",
                kind="image",
                external_key=self.external_key("image", "img-1"),
                change_token="i1",
            ),
            payload=b"\x89PNG\r\n\x1a\n synthetic image bytes",
            mime_type="image/png",
            file_name="img-1.png",
            title="Synthetic image",
        )
        return SourceFetchMaterialized(
            SourceItem(
                ref=page.ref,
                payload=page.payload,
                mime_type=page.mime_type,
                file_name=page.file_name,
                title=page.title,
                dependents=(image,),
            )
        )


@pytest.mark.asyncio
async def test_create_new_item(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t1")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "created"
    created_doc = ironrag.documents[(LIB, "echo:page:1")]
    assert out.ironrag_document_id == str(created_doc.id)
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "t1"


@pytest.mark.asyncio
@pytest.mark.parametrize("mismatch", ["library", "external-key"])
async def test_create_rejects_mismatched_returned_document_identity(
    tmp_path: Path,
    mismatch: str,
) -> None:
    class MismatchedCreateResponse(FakeIronRag):
        async def create_document(
            self,
            library_id: UUID,
            *,
            external_key: str,
            **kwargs: Any,
        ) -> DocumentResource:
            returned_library = LIB2 if mismatch == "library" else library_id
            returned_key = "echo:page:foreign" if mismatch == "external-key" else external_key
            return _document(DOC_PRE, returned_library, returned_key)

    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t1")})
    ref = await _first(adapter.iter_items())
    state = _state(tmp_path)
    ironrag = MismatchedCreateResponse()
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(IronRagError, match="library/external-key identity"):
        await orchestrator.push_ref(ref)

    assert state.get(ref.kind, ref.item_id) is None
    assert state.locations_for(ref.kind, ref.item_id) == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []


@pytest.mark.asyncio
async def test_document_hint_forwards_on_create(tmp_path: Path) -> None:
    adapter = EchoAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    item = SourceItem(
        ref=SourceItemRef(
            item_id="hinted",
            kind="page",
            external_key="echo:page:hinted",
            change_token="t1",
        ),
        payload=b"hello",
        mime_type="text/markdown",
        file_name="hinted.md",
        title="Hinted",
        document_hint="https://docs.example/hinted",
    )
    route = _router().resolve(item.ref)

    out = await orchestrator.push_item(item, route, PushPolicy())

    assert out.action == "created"
    assert ironrag.creates[0]["document_hint"] == "https://docs.example/hinted"


@pytest.mark.asyncio
async def test_unchanged_short_circuits_to_noop(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t1")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "noop_unchanged"
    assert ironrag.creates == []
    assert ironrag.revisions == []


@pytest.mark.asyncio
async def test_changed_item_replaces(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello2", updated_at="t2")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "replaced"
    assert ironrag.revisions
    assert ironrag.revisions[0]["document_id"] == str(DOC_PRE)
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "t2"


@pytest.mark.asyncio
async def test_conflicting_replace_is_deferred_without_advancing_cursor(
    tmp_path: Path,
) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello2", updated_at="t2")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    ironrag.revision_conflicts.add(str(DOC_PRE))
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)

    assert out.action == "skipped_changed"
    assert out.ironrag_document_id == str(DOC_PRE)
    assert out.deferred is True
    assert ironrag.creates == []
    assert ironrag.revisions == []
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "t1"


@pytest.mark.asyncio
async def test_on_changed_skip_does_not_replace(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t2")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    policy = PushPolicy(on_changed=UpdateAction.SKIP)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(policy),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "skipped_changed"
    assert ironrag.revisions == []


@pytest.mark.asyncio
async def test_on_new_skip_does_not_create(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t1")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    policy = PushPolicy(on_new=UpsertAction.SKIP)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(policy),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "skipped_new"
    assert ironrag.creates == []


@pytest.mark.asyncio
async def test_duplicate_content_skip(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t1")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    other_key = "echo:page:other"
    ironrag.documents[(LIB, other_key)] = _document(DOC_OLD, LIB, other_key)
    ironrag._duplicate_existing_id = DOC_OLD
    ironrag.duplicate_for_key = "echo:page:1"
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(
            on_duplicate_content=DuplicateContentAction.SKIP,
            on_missing=DeleteAction.DELETE,
        ),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "skipped_duplicate_content"
    assert out.ironrag_document_id is None
    assert state.locations_for(ref.kind, ref.item_id) == []

    # A pre-fix cursor could have adopted this cross-key duplicate. Migration
    # retains legacy ownership deterministically, so deletion must still prove
    # the requested external key rather than trusting that stale document id.
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token=ref.change_token,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB),
    )
    deleted = await orchestrator.delete_by_ref(ref)

    assert deleted.action == "source_absent"
    assert ironrag.documents[(LIB, other_key)].id == DOC_OLD
    assert ironrag.deletes == []
    assert state.locations_for(ref.kind, ref.item_id) == []


@pytest.mark.asyncio
async def test_duplicate_content_fail_raises(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t1")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.duplicate_for_key = "echo:page:1"
    policy = PushPolicy(on_duplicate_content=DuplicateContentAction.FAIL)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(policy),
    )
    ref = await _first(adapter.iter_items())
    with pytest.raises(IronRagError, match="duplicate content"):
        await orchestrator.push_ref(ref)


@pytest.mark.asyncio
async def test_cursor_wins_over_server_find(tmp_path: Path) -> None:
    """Cursor with a known document_id must short-circuit the server lookup.

    Simulates a deployment where a server-side find is unreliable:
    ``BlindIronRag.find_document`` always returns None. Without the
    cursor-wins fix the orchestrator would re-create and trip a
    unique-violation server-side.
    """
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t2")})
    state = _state(tmp_path)
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )

    class BlindIronRag(FakeIronRag):
        async def find_document(
            self, library_id: UUID, external_key: str
        ) -> DocumentResource | None:
            return None  # server-side find surrogate: always misses

    ironrag = BlindIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "replaced"
    assert ironrag.creates == [], "must not create -- cursor knew doc_id"
    assert ironrag.revisions
    assert ironrag.revisions[0]["document_id"] == str(DOC_PRE)


@pytest.mark.asyncio
async def test_replace_timeout_is_deferred_without_advancing_cursor(tmp_path: Path) -> None:
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t2")})
    state = _state(tmp_path)
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )

    class TimeoutIronRag(FakeIronRag):
        async def wait_for_operation(
            self,
            operation_id: UUID | str,
            *,
            poll_interval: float | None = None,
            budget: float | None = None,
        ) -> OperationStatus:
            raise IronRagMutationTimeoutError("synthetic mutation timeout")

    ironrag = TimeoutIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)

    assert out.action == "skipped_changed"
    assert out.ironrag_document_id == str(DOC_PRE)
    assert out.deferred is True
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "t1", "deferred revision must retry the new version later"


@pytest.mark.asyncio
async def test_revise_404_falls_back_to_create(tmp_path: Path) -> None:
    """A cursor/find pointed at a doc IronRAG no longer has (manual delete,
    library reset). The typed 404 must invalidate the cursor and fall back
    to create so the next sweep is consistent."""
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hi", updated_at="t2")})
    state = _state(tmp_path)
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )

    class MissingDocumentIronRag(FakeIronRag):
        async def create_revision(self, *args: Any, **kwargs: Any) -> OperationHandle:
            raise _not_found_error()

    ironrag = MissingDocumentIronRag()
    # No pre-seeded document for "echo:page:1" -- `find_document` (used by
    # `push_item`'s existing-document lookup) also finds nothing, but the
    # cursor still claims a document id, exercising the create_revision-404
    # path via the cursor branch instead.
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)

    assert out.action == "created"
    created_doc = ironrag.documents[(LIB, "echo:page:1")]
    assert out.ironrag_document_id == str(created_doc.id)


@pytest.mark.asyncio
async def test_route_move_creates_new_target_and_reaps_old_target(tmp_path: Path) -> None:
    """After routing config moves a kind to a new library, the orchestrator
    creates the document under the new target AND reaps the stale document
    from the previously-routed library. The reaper gets that library from a
    pre-sweep snapshot of the durable location registry, so advancing the
    source cursor to the new route cannot lose ownership of the old copy."""
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t1")})
    state = _state(tmp_path)
    external_key = "echo:page:1"
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key=external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB),
    )
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, external_key)] = _document(DOC_OLD, LIB, external_key)
    router = _router_to(LIB2)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=_policies(),
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=_policies(),
        concurrency=1,
        interval_seconds=60,
    )

    report = await manager.run_once(reason="test")

    assert report.created == 1
    assert report.reaped == 1
    assert report.errors == 0
    assert (LIB, external_key) not in ironrag.documents, "old-library doc must be reaped"
    assert (LIB2, external_key) in ironrag.documents
    row = state.get("page", "1")
    assert row is not None
    assert row.current_library_id == str(LIB2)


@pytest.mark.asyncio
async def test_unmoved_kind_is_not_reaped_from_libraries_it_never_used(
    tmp_path: Path,
) -> None:
    """A kind whose items were never routed to some other library must not
    have that unrelated library swept in -- the pre-sweep snapshot only
    widens the reap scope to libraries a *cursor row for this kind* actually
    names, not every library the connector has ever touched."""
    adapter = EchoAdapter({})
    state = _state(tmp_path)
    external_key = "echo:page:stable"
    state.upsert(
        kind="page",
        item_id="stable",
        change_token="t1",
        external_key=external_key,
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, external_key)] = _document(DOC_PRE, LIB, external_key)
    # A document that happens to live in LIB2, a library this kind's cursor
    # never references -- must survive the sweep untouched.
    ironrag.documents[(LIB2, "echo:page:unrelated")] = _document(
        DOC_OLD, LIB2, "echo:page:unrelated"
    )
    router = _router()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=_policies(),
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=_policies(),
        concurrency=1,
        interval_seconds=60,
    )

    report = await manager.run_once(reason="test")

    assert report.reaped == 1, "the stable doc itself has no source item this sweep"
    assert (LIB, external_key) not in ironrag.documents, "stable doc has no source item -- reaped"
    assert (LIB2, "echo:page:unrelated") in ironrag.documents, "unrelated library left untouched"


@pytest.mark.asyncio
async def test_reap_respects_ignore_policy(tmp_path: Path) -> None:
    adapter = PageWithImageAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = SourceItemRef(item_id="1", kind="image", external_key="echo:image:1")
    out = await orchestrator.reap_orphan(
        ref,
        LIB,
        str(DOC_PRE),
        PushPolicy(on_missing=DeleteAction.IGNORE),
        enforcement=SourceDeleteEnforcement.POLICY,
        expected_cursor=None,
        expected_location=None,
    )
    assert out.action == "skipped_missing"
    assert ironrag.deletes == []


@pytest.mark.asyncio
async def test_reap_orphan_deletes_and_polls_to_terminal(tmp_path: Path) -> None:
    adapter = PageWithImageAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:image:1")] = _document(DOC_PRE, LIB, "echo:image:1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = SourceItemRef(item_id="1", kind="image", external_key="echo:image:1")
    out = await orchestrator.reap_orphan(
        ref,
        LIB,
        str(DOC_PRE),
        PushPolicy(),
        enforcement=SourceDeleteEnforcement.POLICY,
        expected_cursor=None,
        expected_location=None,
    )
    assert out.action == "deleted"
    assert ironrag.deletes == [str(DOC_PRE)]
    assert (LIB, "echo:image:1") not in ironrag.documents


@pytest.mark.asyncio
async def test_reap_required_enforcement_overrides_ignore_policy(tmp_path: Path) -> None:
    adapter = EchoAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    orchestrator = _orchestrator(adapter, ironrag, state)
    ref = _page_ref("missing")

    out = await orchestrator.reap_orphan(
        ref,
        LIB,
        str(DOC_PRE),
        PushPolicy(on_missing=DeleteAction.IGNORE),
        enforcement=SourceDeleteEnforcement.REQUIRED,
        expected_cursor=None,
        expected_location=None,
    )

    assert out.action == "deleted"
    assert ironrag.deletes == [str(DOC_PRE)]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("enforcement", "on_missing", "expected_action", "state_remains"),
    [
        (
            SourceDeleteEnforcement.POLICY,
            DeleteAction.IGNORE,
            "skipped_missing",
            True,
        ),
        (
            SourceDeleteEnforcement.POLICY,
            DeleteAction.DELETE,
            "source_absent",
            False,
        ),
        (
            SourceDeleteEnforcement.REQUIRED,
            DeleteAction.IGNORE,
            "source_absent",
            False,
        ),
    ],
    ids=("policy-ignore", "policy-delete", "required-ignore"),
)
async def test_confirm_orphan_absent_applies_enforcement_without_document_id(
    tmp_path: Path,
    enforcement: SourceDeleteEnforcement,
    on_missing: DeleteAction,
    expected_action: str,
    state_remains: bool,
) -> None:
    ref = _page_ref("missing")
    state = _state(tmp_path)
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token="v1",
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    orchestrator = _orchestrator(EchoAdapter({}), FakeIronRag(), state)
    expected_cursor = state.get(ref.kind, ref.item_id)
    expected_location = state.get_location(ref.kind, ref.item_id, str(LIB))

    outcome = await orchestrator.confirm_orphan_absent(
        ref,
        LIB,
        PushPolicy(on_missing=on_missing),
        enforcement=enforcement,
        expected_cursor=expected_cursor,
        expected_location=expected_location,
    )

    assert outcome.action == expected_action
    assert (state.get(ref.kind, ref.item_id) is not None) is state_remains
    assert bool(state.locations_for(ref.kind, ref.item_id)) is state_remains


@pytest.mark.asyncio
async def test_confirm_orphan_absent_defers_when_state_changed_after_scan(
    tmp_path: Path,
) -> None:
    ref = _page_ref("missing")
    state = _state(tmp_path)
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token="v1",
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    stale_cursor = state.get(ref.kind, ref.item_id)
    stale_location = state.get_location(ref.kind, ref.item_id, str(LIB))
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token="v2",
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB),
    )
    orchestrator = _orchestrator(EchoAdapter({}), FakeIronRag(), state)

    outcome = await orchestrator.confirm_orphan_absent(
        ref,
        LIB,
        PushPolicy(on_missing=DeleteAction.DELETE),
        enforcement=SourceDeleteEnforcement.POLICY,
        expected_cursor=stale_cursor,
        expected_location=stale_location,
    )

    assert outcome.action == "fetch_deferred"
    assert outcome.deferred is True
    cursor = state.get(ref.kind, ref.item_id)
    location = state.get_location(ref.kind, ref.item_id, str(LIB))
    assert cursor is not None
    assert cursor.change_token == "v2"
    assert location is not None
    assert location.ironrag_document_id == str(DOC_OLD)


@pytest.mark.asyncio
async def test_webhook_delete_honors_ignore_then_clears_all_route_copies_after_restart(
    tmp_path: Path,
) -> None:
    ref = _page_ref("deleted-v3")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1", document_id=DOC_OLD)
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token="v2",
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB2),
    )
    ironrag.documents[(LIB2, ref.external_key)] = _document(DOC_PRE, LIB2, ref.external_key)
    state.close()
    state = StateStore(tmp_path / "s.sqlite")

    adapter = EchoAdapter({})
    ignored = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router_to(LIB2),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.IGNORE)),
    )
    ignored_outcome = await ignored.delete_by_ref(ref)
    assert ignored_outcome.action == "skipped_missing"
    assert ironrag.find_calls == 0
    assert ironrag.deletes == []

    deleting = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router_to(LIB2),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.DELETE)),
    )
    deleted_outcome = await deleting.delete_by_ref(ref)
    assert deleted_outcome.action == "deleted"
    assert {
        (library_id, key) for library_id, key in ironrag.documents if key == ref.external_key
    } == set()
    assert len(ironrag.deletes) == 2
    assert state.locations_for(ref.kind, ref.item_id) == []
    assert state.get(ref.kind, ref.item_id) is None


@pytest.mark.asyncio
async def test_unroutable_webhook_delete_uses_durable_cursor_and_pre_sweep_scope_after_restart(
    tmp_path: Path,
) -> None:
    """Removing a route must not make an already-owned copy undeletable."""
    materializing_adapter = EchoAdapter(
        {"1": EchoPage(item_id="1", title="One", body="hello", updated_at="v1")}
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = await _first(materializing_adapter.iter_items())
    materializer = _orchestrator(materializing_adapter, ironrag, state)

    created = await materializer.push_ref(ref)

    assert created.action == "created"
    # Seed the two route-history shapes the happy path cannot retain at once:
    # a cursor-only current route and a pre-sweep-only old route. LIB remains
    # in the durable location registry from the materialization above.
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token=ref.change_token,
        external_key=ref.external_key,
        ironrag_document_id=None,
        ironrag_library_id=str(LIB2),
    )
    ironrag.documents[(LIB2, ref.external_key)] = _document(DOC_OLD, LIB2, ref.external_key)
    ironrag.documents[(LIB3, ref.external_key)] = _document(DOC_PRE, LIB3, ref.external_key)
    state.close()

    restarted_state = StateStore(tmp_path / "s.sqlite")
    delete_adapter = EchoAdapter({})
    reads_before_ignore = ironrag.find_calls
    ignored = _orchestrator(
        delete_adapter,
        ironrag,
        restarted_state,
        router=_unroutable_router(),
        policy=PushPolicy(on_missing=DeleteAction.IGNORE),
    )
    ignored.reset_sweep_cache({"page": {LIB3}})

    ignored_outcome = await ignored.delete_by_ref(ref)

    assert ignored_outcome.action == "skipped_missing"
    assert ignored_outcome.workspace_id is None
    assert ignored_outcome.library_id is None
    assert ignored_outcome.rule_description is None
    assert ironrag.find_calls == reads_before_ignore
    assert ironrag.deletes == []
    assert restarted_state.get(ref.kind, ref.item_id) is not None
    assert len(restarted_state.locations_for(ref.kind, ref.item_id)) == 1

    deleting = _orchestrator(
        delete_adapter,
        ironrag,
        restarted_state,
        router=_unroutable_router(),
        policy=PushPolicy(on_missing=DeleteAction.DELETE),
    )
    deleting.reset_sweep_cache({"page": {LIB3}})
    reads_before_delete = ironrag.find_calls

    deleted_outcome = await deleting.delete_by_ref(ref)

    assert deleted_outcome.action == "deleted"
    assert deleted_outcome.workspace_id is None
    assert deleted_outcome.library_id is None
    assert deleted_outcome.rule_description is None
    assert ironrag.find_calls - reads_before_delete == 3
    assert len(ironrag.deletes) == 3
    assert not any(key == ref.external_key for _, key in ironrag.documents)
    assert restarted_state.get(ref.kind, ref.item_id) is None
    assert restarted_state.locations_for(ref.kind, ref.item_id) == []


@pytest.mark.asyncio
async def test_revision_idempotency_key_is_durable_complete_body_scoped(tmp_path: Path) -> None:
    """The revision idempotency key must derive from the payload bytes, not
    the change_token, so a re-rendered payload for the same logical version
    does not collide (409 idempotency_conflict) with a stuck prior attempt.
    Create no longer takes an idempotency key at all (plan S7.1/S7.2) -- only
    revisions and deletes do -- so this is exercised via a replace."""
    adapter = EchoAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    def _item(payload: bytes) -> SourceItem:
        return SourceItem(
            ref=SourceItemRef(
                item_id="1",
                kind="page",
                external_key="echo:page:1",
                change_token="vSAME",
            ),
            payload=payload,
            mime_type="text/html",
            file_name="1.html",
            title="One",
        )

    route = _router().resolve(_item(b"a").ref)

    await orchestrator.push_item(_item(b"<p>render A</p>"), route, PushPolicy())
    first_key = ironrag.revisions[0]["idempotency_key"]
    # Same change_token, different bytes -> different key (no false conflict).
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t-reset",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    orchestrator.reset_sweep_cache()
    await orchestrator.push_item(_item(b"<p>render B (different)</p>"), route, PushPolicy())
    second_key = ironrag.revisions[1]["idempotency_key"]

    assert "vSAME" not in first_key, "key must not embed change_token"
    assert state.materialization_attempts_for_document(str(DOC_PRE))[0].payload_key == first_key
    assert first_key != second_key, "different payload must yield a different key"


@pytest.mark.asyncio
async def test_noop_persists_discovered_document_location(tmp_path: Path) -> None:
    """A seed cursor that knows change_token but has no location must be
    upgraded with the discovered copy on the first sweep, so later sweeps
    short-circuit with zero find calls."""
    adapter = EchoAdapter({"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t1")})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    # Seed cursor: change_token/current route known, location absent.
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=None,
        ironrag_library_id=str(LIB),
    )
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)
    assert out.action == "noop_unchanged"
    assert out.ironrag_document_id == str(DOC_PRE)
    assert ironrag.find_calls == 1
    location = state.get_location("page", "1", str(LIB))
    assert location is not None
    assert location.ironrag_document_id == str(DOC_PRE)

    # Second sweep: the registry now knows the copy, so no further find calls.
    out2 = await orchestrator.push_ref(ref)
    assert out2.action == "noop_unchanged"
    assert ironrag.find_calls == 1, "location registry must short-circuit the second sweep"


@pytest.mark.asyncio
async def test_dependent_uploaded_with_parent_external_key(tmp_path: Path) -> None:
    """A page with an image dependent must create the dependent declaring its
    source page as parent (parent_external_key == parent.ref.external_key),
    while the primary page creates with parent_external_key None. This is the
    single orchestrator injection point that gives every connector correct
    parentage without an adapter change."""

    adapter = PageWithImageAdapter(
        {"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t1")}
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(adapter.iter_items())
    out = await orchestrator.push_ref(ref)

    assert out.action == "created"
    assert len(out.dependent_outcomes) == 1
    assert out.dependent_outcomes[0].action == "created"

    by_key = {c["external_key"]: c for c in ironrag.creates}
    primary = by_key["echo:page:1"]
    dependent = by_key["echo:image:img-1"]
    assert primary["parent_external_key"] is None, "primary stays role=primary"
    assert dependent["parent_external_key"] == "echo:page:1", (
        "dependent must declare its source page as parent"
    )


@pytest.mark.asyncio
async def test_dependent_waits_when_primary_replace_is_deferred(tmp_path: Path) -> None:
    class TimeoutReviseIronRag(FakeIronRag):
        async def wait_for_operation(
            self,
            operation_id: UUID | str,
            *,
            poll_interval: float | None = None,
            budget: float | None = None,
        ) -> OperationStatus:
            raise IronRagMutationTimeoutError("synthetic mutation timeout")

    adapter = PageWithImageAdapter(
        {"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t2")}
    )
    state = _state(tmp_path)
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    ironrag = TimeoutReviseIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    out = await orchestrator.push_ref(await _first(adapter.iter_items()))

    assert out.action == "skipped_changed"
    assert out.deferred is True
    assert out.dependent_outcomes == ()
    assert ironrag.creates == []
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "t1"


@pytest.mark.asyncio
async def test_dependent_deferral_restores_parent_cursor_for_retry(
    tmp_path: Path,
) -> None:
    """The dependent image already has an IronRAG document (so its write is
    a revise, not a create -- only revise supports poll-timeout deferral in
    the redesigned client, since create is now a single synchronous call
    with no polling phase to time out on). Its first revision poll times
    out; the orchestrator must defer the whole item (restoring the parent's
    cursor for retry) rather than partially advancing state, and the second
    sweep must succeed once the transient failure clears."""

    class OneTimeoutImageReviseIronRag(FakeIronRag):
        def __init__(self) -> None:
            super().__init__()
            self.image_op_polls = 0
            self._image_op_ids: set[UUID] = set()

        async def create_revision(self, document_id: Any, **kwargs: Any) -> OperationHandle:
            handle = await super().create_revision(document_id, **kwargs)
            if str(document_id) == str(DOC_IMAGE_PRE):
                self._image_op_ids.add(handle.operation_id)
            return handle

        async def wait_for_operation(
            self,
            operation_id: UUID | str,
            *,
            poll_interval: float | None = None,
            budget: float | None = None,
        ) -> OperationStatus:
            op_id = UUID(str(operation_id))
            if op_id in self._image_op_ids:
                self.image_op_polls += 1
                if self.image_op_polls == 1:
                    raise IronRagMutationTimeoutError("synthetic mutation timeout")
            return await super().wait_for_operation(operation_id)

    adapter = PageWithImageAdapter(
        {"1": EchoPage(item_id="1", title="One", body="hello", updated_at="t2")}
    )
    state = _state(tmp_path)
    state.upsert(
        kind="page",
        item_id="1",
        change_token="t1",
        external_key="echo:page:1",
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    state.upsert(
        kind="image",
        item_id="img-1",
        change_token="i0",
        external_key="echo:image:img-1",
        ironrag_document_id=str(DOC_IMAGE_PRE),
        ironrag_library_id=str(LIB),
    )
    ironrag = OneTimeoutImageReviseIronRag()
    ironrag.documents[(LIB, "echo:page:1")] = _document(DOC_PRE, LIB, "echo:page:1")
    ironrag.documents[(LIB, "echo:image:img-1")] = _document(DOC_IMAGE_PRE, LIB, "echo:image:img-1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(adapter.iter_items())

    first = await orchestrator.push_ref(ref)

    assert first.action == "replaced"
    assert first.deferred is True
    assert len(first.dependent_outcomes) == 1
    assert first.dependent_outcomes[0].action == "skipped_changed"
    assert first.dependent_outcomes[0].deferred is True
    assert ironrag.image_op_polls == 1
    parent_row = state.get("page", "1")
    assert parent_row is not None
    assert parent_row.change_token == "t1"
    image_row = state.get("image", "img-1")
    assert image_row is not None
    assert image_row.change_token == "i0"

    orchestrator.reset_sweep_cache()
    second = await orchestrator.push_ref(ref)

    assert second.action == "replaced"
    assert second.deferred is False
    assert len(second.dependent_outcomes) == 1
    assert second.dependent_outcomes[0].action == "replaced"
    assert ironrag.image_op_polls == 2
    page_row_after = state.get("page", "1")
    assert page_row_after is not None
    assert page_row_after.change_token == "t2"
    image_row_after = state.get("image", "img-1")
    assert image_row_after is not None
    assert image_row_after.change_token == "i1"


async def _first(it: Any) -> Any:
    async for x in it:
        return x
    raise AssertionError("empty iter")


class CountingAuthoritativeDeleteAdapter(EchoAdapter):
    """An adapter whose items exist upstream but carry nothing to ingest.

    Confluence pages without a body and without attachments behave exactly like
    this: `iter_items` keeps listing them because they exist, and `fetch`
    correctly declines to make a document out of them.
    """

    def __init__(self, pages: dict[str, EchoPage]) -> None:
        super().__init__(pages)
        self.fetch_calls = 0

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        self.fetch_calls += 1
        return SourceFetchDelete(
            SourceDeleteEnforcement.REQUIRED,
            "item has no ingestable content",
            cacheable=True,
        )


class FixedFetchResultAdapter(EchoAdapter):
    """Return one explicit source decision for orchestration contract tests."""

    def __init__(self, result: SourceFetchResult) -> None:
        super().__init__({})
        self.result = result
        self.fetch_calls = 0

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        self.fetch_calls += 1
        return self.result


class OneRefFetchResultAdapter(FixedFetchResultAdapter):
    def __init__(self, ref: SourceItemRef, result: SourceFetchResult) -> None:
        super().__init__(result)
        self.ref = ref

    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        yield self.ref


@pytest.mark.asyncio
async def test_push_ref_rejects_noncanonical_external_key_before_any_side_effect(
    tmp_path: Path,
) -> None:
    ref = SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:someone-else",
        change_token="v1",
    )
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=ref,
                payload=b"must not be written",
                mime_type="text/markdown",
                file_name="1.md",
            )
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    with pytest.raises(ValueError, match="non-canonical source ref"):
        await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == 0
    assert ironrag.find_calls == 0
    assert ironrag.creates == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []
    assert state.get("page", "1") is None


@pytest.mark.asyncio
async def test_push_ref_rejects_undeclared_primary_kind_before_any_side_effect(
    tmp_path: Path,
) -> None:
    ref = SourceItemRef(
        item_id="1",
        kind="future",
        external_key="echo:future:1",
        change_token="v1",
    )
    adapter = FixedFetchResultAdapter(SourceFetchDeferred("must not be fetched"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(ValueError, match="undeclared source kind"):
        await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == 0
    assert ironrag.find_calls == 0
    assert ironrag.creates == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []
    assert state.get("future", "1") is None


@pytest.mark.asyncio
async def test_push_ref_rejects_undeclared_dependent_kind_before_primary_mutation(
    tmp_path: Path,
) -> None:
    requested = _page_ref("v1")
    undeclared_dependent = SourceItem(
        ref=SourceItemRef(
            item_id="dependent-1",
            kind="future",
            external_key="echo:future:dependent-1",
        ),
        payload=b"must not be written",
        mime_type="application/octet-stream",
        file_name="dependent.bin",
    )
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=requested,
                payload=b"primary must not be written either",
                mime_type="text/markdown",
                file_name="1.md",
                dependents=(undeclared_dependent,),
            )
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(ValueError, match="undeclared source kind"):
        await orchestrator.push_ref(requested)

    assert ironrag.find_calls == 0
    assert ironrag.creates == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []
    assert state.get("page", "1") is None
    assert state.get("future", "dependent-1") is None


@pytest.mark.asyncio
async def test_push_ref_rejects_adapter_key_that_does_not_parse_round_trip(
    tmp_path: Path,
) -> None:
    class NonRoundTrippingAdapter(FixedFetchResultAdapter):
        def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
            return ("page", "different")

    ref = SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token="v1",
    )
    adapter = NonRoundTrippingAdapter(SourceFetchDeferred("must not be fetched"))
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=_state(tmp_path),
        policies=_policies(),
    )

    with pytest.raises(ValueError, match="does not round-trip"):
        await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == 0
    assert ironrag.find_calls == 0
    assert ironrag.deletes == []


@pytest.mark.asyncio
async def test_push_ref_rejects_materialized_item_for_another_canonical_identity(
    tmp_path: Path,
) -> None:
    requested = _page_ref("v1")
    returned_ref = SourceItemRef(
        item_id="2",
        kind="page",
        external_key="echo:page:2",
        change_token="v1",
        routing_facts={"destination": "secondary"},
    )
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=returned_ref,
                payload=b"belongs to page 2",
                mime_type="text/markdown",
                file_name="2.md",
            )
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=_router_with_configured_secondary(LIB, LIB2),
    )

    with pytest.raises(ValueError, match="materialized source identity"):
        await orchestrator.push_ref(requested)

    assert ironrag.find_calls == 0
    assert ironrag.creates == []
    assert state.get("page", "1") is None
    assert state.get("page", "2") is None


@pytest.mark.parametrize(
    ("returned_change_token", "returned_routing_facts"),
    [
        pytest.param("v2", {}, id="change-token-drift"),
        pytest.param("v1", {"destination": "secondary"}, id="routing-facts-drift"),
    ],
)
@pytest.mark.asyncio
async def test_push_ref_rejects_materialized_snapshot_drift_before_target_read(
    tmp_path: Path,
    returned_change_token: str,
    returned_routing_facts: dict[str, str],
) -> None:
    requested = _page_ref("v1")
    returned_ref = SourceItemRef(
        item_id=requested.item_id,
        kind=requested.kind,
        external_key=requested.external_key,
        change_token=returned_change_token,
        routing_facts=returned_routing_facts,
        raw={"fresh source payload": True},
    )
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=returned_ref,
                payload=b"must not be written through a stale route",
                mime_type="text/markdown",
                file_name="1.md",
            )
        )
    )
    state = _state(tmp_path)
    # A matching cursor without a durable location used to trigger an exact
    # IronRAG read before fetch validation. Keep the target empty so fetch is
    # reached while proving that no target read crosses the stale boundary.
    state.upsert(
        kind=requested.kind,
        item_id=requested.item_id,
        change_token=requested.change_token,
        external_key=requested.external_key,
        ironrag_document_id=None,
        ironrag_library_id=str(LIB),
    )
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=_router_with_configured_secondary(LIB, LIB2),
    )

    with pytest.raises(ValueError, match="materialized source snapshot"):
        await orchestrator.push_ref(requested)

    assert adapter.fetch_calls == 1
    assert ironrag.find_calls == 0
    assert ironrag.creates == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []
    row = state.get(requested.kind, requested.item_id)
    assert row is not None
    assert row.change_token == "v1"


@pytest.mark.asyncio
async def test_push_ref_allows_materialized_raw_refresh_for_same_route_snapshot(
    tmp_path: Path,
) -> None:
    requested = _page_ref("v1")
    returned_ref = SourceItemRef(
        item_id=requested.item_id,
        kind=requested.kind,
        external_key=requested.external_key,
        change_token=requested.change_token,
        routing_facts=requested.routing_facts,
        raw={"fresh source payload": True},
    )
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=returned_ref,
                payload=b"safe refreshed payload",
                mime_type="text/markdown",
                file_name="1.md",
            )
        )
    )
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, _state(tmp_path))

    outcome = await orchestrator.push_ref(requested)

    assert outcome.action == "created"
    assert ironrag.creates[0]["library_id"] == LIB


@pytest.mark.asyncio
async def test_push_ref_rejects_invalid_dependent_before_primary_mutation(tmp_path: Path) -> None:
    requested = _page_ref("v1")
    invalid_dependent = SourceItem(
        ref=SourceItemRef(
            item_id="dependent-page",
            kind="page",
            external_key="echo:page:someone-else",
        ),
        payload=b"must not be written",
        mime_type="image/png",
        file_name="image-1.png",
    )
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=requested,
                payload=b"primary must not be written either",
                mime_type="text/markdown",
                file_name="1.md",
                dependents=(invalid_dependent,),
            )
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(ValueError, match="non-canonical source ref"):
        await orchestrator.push_ref(requested)

    assert ironrag.find_calls == 0
    assert ironrag.creates == []
    assert state.get("page", "1") is None
    assert state.get("page", "dependent-page") is None


@pytest.mark.asyncio
async def test_deferred_fetch_does_not_mutate_or_enter_negative_cache(tmp_path: Path) -> None:
    ref = _page_ref("v2")
    adapter = FixedFetchResultAdapter(SourceFetchDeferred("upstream read timed out"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    first = await orchestrator.push_ref(ref)
    second = await orchestrator.push_ref(ref)

    assert first.action == "fetch_deferred"
    assert first.deferred is True
    assert second.action == "fetch_deferred"
    assert adapter.fetch_calls == 2, "deferred reads must be retried, never negative-cached"
    assert state.get_empty_fetch("page", "1") is None
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "v1"
    assert ironrag.deletes == []


@pytest.mark.asyncio
async def test_deferred_fetch_is_counted_and_protects_existing_doc_from_reaper(
    tmp_path: Path,
) -> None:
    ref = _page_ref("v2")
    adapter = OneRefFetchResultAdapter(
        ref,
        SourceFetchDeferred("upstream snapshot is unstable"),
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    router = _router()
    policies = _policies(PushPolicy(on_missing=DeleteAction.DELETE))
    orchestrator = Orchestrator(
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

    report = await manager.run_once(reason="test")

    assert report.deferred == 1
    assert report.errors == 0
    assert report.reaped == 0
    assert (LIB, ref.external_key) in ironrag.documents
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "v1"
    assert state.get_empty_fetch("page", "1") is None


@pytest.mark.asyncio
async def test_policy_fetch_delete_removes_race_without_negative_caching(
    tmp_path: Path,
) -> None:
    ref = _page_ref("v2")
    adapter = FixedFetchResultAdapter(
        SourceFetchDelete(
            SourceDeleteEnforcement.POLICY,
            "item disappeared after enumeration",
            cacheable=False,
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(
        state,
        ironrag,
        ref,
        change_token="v1",
        remember_location=False,
    )
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.DELETE)),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "deleted"
    assert ironrag.deletes == [str(DOC_PRE)]
    assert state.get("page", "1") is None
    assert state.get_empty_fetch("page", "1") is None


@pytest.mark.asyncio
async def test_same_token_reappearance_after_fetch_race_is_materialized(
    tmp_path: Path,
) -> None:
    pages = {"1": EchoPage(item_id="1", title="One", body="old", updated_at="v1")}
    adapter = EchoAdapter(pages)
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)
    initial_ref = await _first(adapter.iter_items())
    assert (await orchestrator.push_ref(initial_ref)).action == "created"

    pages["1"] = EchoPage(item_id="1", title="One", body="new", updated_at="v2")
    raced_ref = await _first(adapter.iter_items())
    del pages["1"]
    disappeared = await orchestrator.push_ref(raced_ref)
    assert disappeared.action == "deleted"
    assert state.get_empty_fetch("page", "1") is None

    pages["1"] = EchoPage(item_id="1", title="One", body="new", updated_at="v2")
    reappeared_ref = await _first(adapter.iter_items())
    assert reappeared_ref.change_token == raced_ref.change_token
    reappeared = await orchestrator.push_ref(reappeared_ref)

    assert reappeared.action == "created"
    assert (LIB, reappeared_ref.external_key) in ironrag.documents


@pytest.mark.asyncio
async def test_policy_fetch_delete_honors_ignore_without_caching_retained_document(
    tmp_path: Path,
) -> None:
    ref = _page_ref("v2")
    adapter = FixedFetchResultAdapter(
        SourceFetchDelete(
            SourceDeleteEnforcement.POLICY,
            "item disappeared after enumeration",
            cacheable=False,
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.IGNORE)),
    )

    first = await orchestrator.push_ref(ref)
    second = await orchestrator.push_ref(ref)

    assert first.action == "skipped_missing"
    assert second.action == "skipped_missing"
    assert adapter.fetch_calls == 2
    assert ironrag.deletes == []
    assert state.get_empty_fetch("page", "1") is None
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "v1"


@pytest.mark.asyncio
async def test_text_to_binary_required_delete_overrides_ignore_policy(tmp_path: Path) -> None:
    ref = _page_ref("binary-v2")
    adapter = FixedFetchResultAdapter(
        SourceFetchDelete(
            SourceDeleteEnforcement.REQUIRED,
            "content is no longer ingestable",
            cacheable=True,
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="text-v1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.IGNORE)),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "deleted"
    assert ironrag.deletes == [str(DOC_PRE)]
    assert state.get("page", "1") is None


@pytest.mark.asyncio
async def test_required_delete_clears_every_known_and_configured_library_after_route_move(
    tmp_path: Path,
) -> None:
    """A route move must not make an old copy undiscoverable.

    The first write moves the cursor from LIB to LIB2 while
    ``on_missing=ignore`` deliberately retains the old document. LIB is then
    absent from the current routing snapshot. A subsequent REQUIRED source
    decision must still delete that owned old copy, the current copy, an
    otherwise-untracked duplicate in another currently configured library,
    and a copy visible only through the stable pre-sweep library snapshot.
    """
    external_key = "echo:page:1"
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(
        state,
        ironrag,
        _page_ref("v1"),
        change_token="v1",
        document_id=DOC_OLD,
    )

    moved_adapter = EchoAdapter(
        {"1": EchoPage(item_id="1", title="One", body="moved", updated_at="v2")}
    )
    moved_orchestrator = Orchestrator(
        adapter=moved_adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router_to(LIB2),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.IGNORE)),
    )
    moved_ref = await _first(moved_adapter.iter_items())
    moved = await moved_orchestrator.push_ref(moved_ref)
    assert moved.action == "created"
    assert (LIB, external_key) in ironrag.documents
    assert (LIB2, external_key) in ironrag.documents
    assert {location.ironrag_library_id for location in state.locations_for("page", "1")} == {
        str(LIB),
        str(LIB2),
    }
    state.close()
    state = StateStore(tmp_path / "s.sqlite")

    # This copy models state that predates durable ownership tracking. It is
    # discoverable only because LIB3 is still present somewhere in the active
    # routing snapshot, even though this ref resolves to LIB2.
    ironrag.documents[(LIB3, external_key)] = _document(DOC_PRE, LIB3, external_key)
    ironrag.documents[(LIB4, external_key)] = _document(DOC_IMAGE_PRE, LIB4, external_key)
    delete_ref = _page_ref("restricted-v3")
    delete_adapter = FixedFetchResultAdapter(
        SourceFetchDelete(
            SourceDeleteEnforcement.REQUIRED,
            "source item is no longer safe to index",
            cacheable=True,
        )
    )
    delete_orchestrator = Orchestrator(
        adapter=delete_adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router_with_configured_secondary(LIB2, LIB3),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.IGNORE)),
    )
    delete_orchestrator.reset_sweep_cache({"page": {LIB4}})

    outcome = await delete_orchestrator.push_ref(delete_ref)

    assert outcome.action == "deleted"
    assert {
        (library_id, key) for library_id, key in ironrag.documents if key == external_key
    } == set()
    assert len(ironrag.deletes) == 4
    assert state.get("page", "1") is None
    assert state.locations_for("page", "1") == []
    cached_absence = state.get_empty_fetch("page", "1")
    assert cached_absence is not None
    assert cached_absence.change_token == "restricted-v3"

    # A routing reload can widen the connector's authoritative scope without
    # changing the source token. The cached absence is valid only for the old
    # scope, so the decision must be fetched and applied again for LIB5.
    ironrag.documents[(LIB5, external_key)] = _document(DOC_PRE, LIB5, external_key)
    widened_orchestrator = Orchestrator(
        adapter=delete_adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router_with_configured_secondary(LIB2, LIB5),
        state=state,
        policies=_policies(PushPolicy(on_missing=DeleteAction.IGNORE)),
    )
    widened = await widened_orchestrator.push_ref(delete_ref)
    assert widened.action == "deleted"
    assert (LIB5, external_key) not in ironrag.documents
    assert delete_adapter.fetch_calls == 2


@pytest.mark.asyncio
async def test_failed_fetch_driven_delete_does_not_advance_or_cache(tmp_path: Path) -> None:
    class FailingDeleteIronRag(FakeIronRag):
        async def wait_for_operation(
            self,
            operation_id: UUID | str,
            *,
            poll_interval: float | None = None,
            budget: float | None = None,
        ) -> OperationStatus:
            raise IronRagError("synthetic delete failure after submission")

    ref = _page_ref("v2")
    adapter = FixedFetchResultAdapter(
        SourceFetchDelete(
            SourceDeleteEnforcement.REQUIRED,
            "content is no longer ingestable",
            cacheable=True,
        )
    )
    state = _state(tmp_path)
    ironrag = FailingDeleteIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    with pytest.raises(IronRagError, match="synthetic delete failure after submission"):
        await orchestrator.push_ref(ref)

    assert ironrag.deletes == [str(DOC_PRE)], "the failure occurs after delete submission"
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == "v1"
    assert state.get_location("page", "1", str(LIB)) is not None
    assert state.get_empty_fetch("page", "1") is None


@pytest.mark.asyncio
async def test_an_item_with_nothing_to_fetch_is_not_refetched_until_it_changes(
    tmp_path: Path,
) -> None:
    """A negative fetch is a result, and it is bound to a change_token.

    Without remembering it, every sweep re-asks the vendor about an item that
    cannot become a document. On the stand that was 103 Confluence pages
    re-fetched every 30 minutes — roughly nine thousand pointless upstream
    requests a day, indistinguishable in the report from pages that were
    actually deleted.
    """
    adapter = CountingAuthoritativeDeleteAdapter(
        {"1": EchoPage(item_id="1", title="Empty", body="", updated_at="v1")}
    )
    state = _state(tmp_path)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=FakeIronRag(),  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    ref = await _first(adapter.iter_items())
    first = await orchestrator.push_ref(ref)
    assert first.action == "source_absent"
    assert adapter.fetch_calls == 1

    # Second sweep, same change_token: the answer is already known.
    second = await orchestrator.push_ref(ref)
    assert second.action == "source_absent"
    assert adapter.fetch_calls == 1, (
        "the adapter was asked again about an item whose emptiness was already "
        "established for this change_token"
    )

    # The item was edited upstream: now it deserves another look.
    changed = SourceItemRef(
        item_id=ref.item_id,
        kind=ref.kind,
        external_key=ref.external_key,
        change_token="v2",
        routing_facts=ref.routing_facts,
        raw=ref.raw,
    )
    third = await orchestrator.push_ref(changed)
    assert third.action == "source_absent"
    assert adapter.fetch_calls == 2, "a changed item must be fetched again"


@pytest.mark.asyncio
async def test_cached_intrinsic_absence_rechecks_exact_target_each_sweep(
    tmp_path: Path,
) -> None:
    """A restored binary document cannot hide forever behind its stable SHA."""
    ref = _page_ref("stable-binary-sha")
    decision = SourceFetchDelete(
        SourceDeleteEnforcement.REQUIRED,
        "content hash identifies an intrinsic binary blob",
        cacheable=True,
    )
    adapter = OneRefFetchResultAdapter(ref, decision)
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="text-before-binary")
    router = _router()
    policies = _policies(PushPolicy(on_missing=DeleteAction.IGNORE))
    orchestrator = Orchestrator(
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

    first = await manager.run_once(reason="binary-cache-initial")
    assert first.reaped == 1
    assert adapter.fetch_calls == 1
    assert (LIB, ref.external_key) not in ironrag.documents
    assert state.get_empty_fetch(ref.kind, ref.item_id) is not None

    restored_id = _doc_id(777)
    ironrag.documents[(LIB, ref.external_key)] = _document(
        restored_id,
        LIB,
        ref.external_key,
    )
    second = await manager.run_once(reason="binary-cache-restored-target")

    assert second.reaped == 1
    assert adapter.fetch_calls == 2
    assert str(restored_id) in ironrag.deletes
    assert (LIB, ref.external_key) not in ironrag.documents


@pytest.mark.asyncio
async def test_cached_absence_scan_failure_preserves_target_and_cache(tmp_path: Path) -> None:
    class ToggleMalformedFindIronRag(FakeIronRag):
        fail_find = False

        async def find_document(
            self,
            library_id: UUID,
            external_key: str,
        ) -> DocumentResource | None:
            if self.fail_find:
                raise IronRagError("malformed exact-target collection")
            return await super().find_document(library_id, external_key)

    ref = _page_ref("stable-binary-sha")
    adapter = FixedFetchResultAdapter(
        SourceFetchDelete(
            SourceDeleteEnforcement.REQUIRED,
            "content hash identifies an intrinsic binary blob",
            cacheable=True,
        )
    )
    state = _state(tmp_path)
    ironrag = ToggleMalformedFindIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="text-before-binary")
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_missing=DeleteAction.IGNORE),
    )

    assert (await orchestrator.push_ref(ref)).action == "deleted"
    cached_before = state.get_empty_fetch(ref.kind, ref.item_id)
    assert cached_before is not None
    restored_id = _doc_id(778)
    ironrag.documents[(LIB, ref.external_key)] = _document(
        restored_id,
        LIB,
        ref.external_key,
    )
    ironrag.fail_find = True

    with pytest.raises(IronRagError, match="malformed exact-target collection"):
        await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == 1
    assert ironrag.documents[(LIB, ref.external_key)].id == restored_id
    assert state.get_empty_fetch(ref.kind, ref.item_id) == cached_before


@pytest.mark.asyncio
async def test_an_item_that_stops_being_empty_is_ingested(tmp_path: Path) -> None:
    """The remembered emptiness must not outlive the reason for it."""
    pages = {"1": EchoPage(item_id="1", title="Empty", body="", updated_at="v1")}
    empty_adapter = CountingAuthoritativeDeleteAdapter(pages)
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = Orchestrator(
        adapter=empty_adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    ref = await _first(empty_adapter.iter_items())
    assert (await orchestrator.push_ref(ref)).action == "source_absent"

    # Someone filled the page in; the change_token moves with the edit.
    filled = EchoAdapter({"1": EchoPage(item_id="1", title="Filled", body="text", updated_at="v2")})
    orchestrator = Orchestrator(
        adapter=filled,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )
    new_ref = await _first(filled.iter_items())
    outcome = await orchestrator.push_ref(new_ref)
    assert outcome.action == "created"
    assert state.get("page", "1") is not None


def _materialized_page(
    ref: SourceItemRef,
    payload: bytes,
    *,
    enforcement: SourceMaterializationEnforcement,
) -> SourceFetchMaterialized:
    return SourceFetchMaterialized(
        SourceItem(
            ref=ref,
            payload=payload,
            mime_type="text/markdown",
            file_name="1.md",
        ),
        enforcement=enforcement,
    )


def _required_page_ref(change_token: str) -> SourceItemRef:
    return SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token=change_token,
        materialization_enforcement=SourceMaterializationEnforcement.REQUIRED,
    )


class MappedPolicyAdapter(FixedFetchResultAdapter):
    def __init__(
        self,
        *,
        name: str,
        kind: str,
        build_external_key: Callable[[str, str], str],
        parse_external_key: Callable[[str], tuple[str, str] | None],
        result: SourceFetchResult,
    ) -> None:
        super().__init__(result)
        self.name = name
        self.kinds = (kind,)
        self.primary_kinds = (kind,)
        self._build_external_key = build_external_key
        self._parse_external_key = parse_external_key

    def external_key(self, kind: str, item_id: str) -> str:
        return self._build_external_key(kind, item_id)

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return self._parse_external_key(external_key)

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind in self.kinds:
            return SourceDeleteEnforcement.POLICY
        raise ValueError(f"unsupported {self.name} item kind: {kind}")


_ECHO_MAPPING = EchoAdapter({})
_POLICY_MAPPING_CASES = (
    (
        "bookstack",
        "page",
        "42",
        build_bookstack_external_key,
        parse_bookstack_external_key,
    ),
    (
        "confluence",
        "page",
        "42",
        build_confluence_external_key,
        parse_confluence_external_key,
    ),
    (
        "gitrepos",
        "file",
        file_item_id("repo", "main", "README.md"),
        build_gitrepos_external_key,
        parse_gitrepos_external_key,
    ),
    (
        "echo",
        "page",
        "42",
        _ECHO_MAPPING.external_key,
        _ECHO_MAPPING.parse_external_key,
    ),
)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("adapter_name", "kind", "item_id", "build_external_key", "parse_external_key"),
    _POLICY_MAPPING_CASES,
    ids=tuple(case[0] for case in _POLICY_MAPPING_CASES),
)
@pytest.mark.parametrize("operation", ["equal-token", "changed-token", "delete"])
async def test_legacy_duplicate_document_id_is_never_adopted_or_mutated(
    tmp_path: Path,
    adapter_name: str,
    kind: str,
    item_id: str,
    build_external_key: Callable[[str, str], str],
    parse_external_key: Callable[[str], tuple[str, str] | None],
    operation: str,
) -> None:
    external_key = build_external_key(kind, item_id)
    source_token = "v2"
    legacy_token = source_token if operation == "equal-token" else "v1"
    db_path = tmp_path / "s.sqlite"
    _create_legacy_cursor_db(
        db_path,
        kind=kind,
        item_id=item_id,
        change_token=legacy_token,
        external_key=external_key,
        ironrag_document_id=DOC_PRE,
    )
    state = StateStore(db_path)
    assert state.get_location(kind, item_id, str(LIB)) is None
    ref = SourceItemRef(
        item_id=item_id,
        kind=kind,
        external_key=external_key,
        change_token=source_token,
    )
    adapter = MappedPolicyAdapter(
        name=adapter_name,
        kind=kind,
        build_external_key=build_external_key,
        parse_external_key=parse_external_key,
        result=_materialized_page(
            ref,
            b"canonical bytes",
            enforcement=SourceMaterializationEnforcement.POLICY,
        ),
    )
    ironrag = FakeIronRag()
    foreign_key = f"foreign:{adapter_name}:{item_id}"
    ironrag.documents[(LIB, foreign_key)] = _document(DOC_PRE, LIB, foreign_key)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=_policies(),
    )

    if operation == "delete":
        outcome = await orchestrator.delete_by_ref(ref)
        assert outcome.action == "source_absent"
        assert state.get(kind, item_id) is None
    else:
        outcome = await orchestrator.push_ref(ref)
        assert outcome.action == "created"
        location = state.get_location(kind, item_id, str(LIB))
        assert location is not None
        assert location.ironrag_document_id != str(DOC_PRE)

    assert ironrag.documents[(LIB, foreign_key)].id == DOC_PRE
    assert ironrag.revisions == []
    assert ironrag.deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "enforcement",
    [
        SourceMaterializationEnforcement.POLICY,
        SourceMaterializationEnforcement.REQUIRED,
    ],
)
async def test_exact_find_rejects_cross_library_document_before_trusting_id(
    tmp_path: Path,
    enforcement: SourceMaterializationEnforcement,
) -> None:
    ref = SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token="v1",
        materialization_enforcement=enforcement,
    )
    adapter = FixedFetchResultAdapter(_materialized_page(ref, b"safe", enforcement=enforcement))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.documents[(LIB, ref.external_key)] = _document(DOC_PRE, LIB2, ref.external_key)
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(IronRagError, match="library/external-key identity"):
        await orchestrator.push_ref(ref)

    assert state.get(ref.kind, ref.item_id) is None
    assert state.locations_for(ref.kind, ref.item_id) == []
    assert ironrag.revisions == []


@pytest.mark.asyncio
async def test_required_discovery_error_retains_applied_state_for_retry(
    tmp_path: Path,
) -> None:
    class MalformedCollectionIronRag(FakeIronRag):
        async def find_document(
            self,
            library_id: UUID,
            external_key: str,
        ) -> DocumentResource | None:
            raise IronRagError("IronRAG returned a malformed document collection page")

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = MalformedCollectionIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(IronRagError, match="malformed document collection"):
        await orchestrator.push_ref(ref)

    cursor = state.get(ref.kind, ref.item_id)
    location = state.get_location(ref.kind, ref.item_id, str(LIB))
    assert cursor is not None
    assert cursor.change_token == "v1"
    assert location is not None
    assert location.ironrag_document_id == str(DOC_PRE)
    assert state.get_empty_fetch(ref.kind, ref.item_id) is None
    assert (LIB, ref.external_key) in ironrag.documents


@pytest.mark.asyncio
async def test_required_intentionally_unrouted_ref_deletes_all_exact_copies(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("v2-no-longer-routed")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    for library_id, document_id in ((LIB, DOC_PRE), (LIB2, DOC_OLD)):
        state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token="v1",
            external_key=ref.external_key,
            ironrag_document_id=str(document_id),
            ironrag_library_id=str(library_id),
        )
        ironrag.documents[(library_id, ref.external_key)] = _document(
            document_id,
            library_id,
            ref.external_key,
        )
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"must not be fetched without a route",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=_unroutable_router(),
        policy=PushPolicy(on_missing=DeleteAction.IGNORE),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "deleted"
    assert outcome.library_id is None
    assert adapter.fetch_calls == 0
    assert set(ironrag.deletes) == {str(DOC_PRE), str(DOC_OLD)}
    assert state.get(ref.kind, ref.item_id) is None
    assert state.locations_for(ref.kind, ref.item_id) == []


@pytest.mark.asyncio
async def test_policy_intentionally_unrouted_ref_remains_nondestructive(
    tmp_path: Path,
) -> None:
    ref = _page_ref("v2-no-longer-routed")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = EchoAdapter({})
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=_unroutable_router(),
        policy=PushPolicy(on_missing=DeleteAction.IGNORE),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "unrouted"
    assert ironrag.deletes == []
    assert (LIB, ref.external_key) in ironrag.documents
    assert state.get(ref.kind, ref.item_id) is not None


@pytest.mark.asyncio
async def test_required_catalog_resolution_error_remains_nondestructive(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"must not be fetched through a broken catalog",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    unresolved_ref = _library_ref(LIB)
    unresolved_router = Router(
        RoutingConfig.model_validate({"default": {"library": unresolved_ref}})
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=unresolved_router,
        policy=PushPolicy(on_missing=DeleteAction.IGNORE),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "unrouted"
    assert adapter.fetch_calls == 0
    assert ironrag.deletes == []
    assert (LIB, ref.external_key) in ironrag.documents
    assert state.get(ref.kind, ref.item_id) is not None


@pytest.mark.asyncio
async def test_required_materialization_replaces_stale_bytes_despite_skip_policy(
    tmp_path: Path,
) -> None:
    """A child restricted before enumeration must not freeze its old bytes.

    The captured v2 ref and fetched payload are already the sanitized source
    projection, so there is no prior raw child to compare during fetch. The
    typed REQUIRED decision is what overrides ``on_changed=skip``. Once v2 is
    applied, a third sweep refetches and reconciles the same bound receipt.
    """
    old_ref = _required_page_ref("v1")
    sanitized_ref = _required_page_ref("v2-restricted-before-census")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            old_ref,
            b"public parent\n\nstale sensitive comment marker",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    state = _state(tmp_path)
    ironrag = PayloadTrackingIronRag()
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(
            on_changed=UpdateAction.SKIP,
            on_missing=DeleteAction.IGNORE,
        ),
    )

    created = await orchestrator.push_ref(old_ref)
    original_document_id = created.ironrag_document_id
    assert b"stale sensitive comment marker" in ironrag.payloads[(LIB, old_ref.external_key)]

    orchestrator.reset_sweep_cache()
    adapter.result = _materialized_page(
        sanitized_ref,
        b"public parent\n\nrestricted comment omitted",
        enforcement=SourceMaterializationEnforcement.REQUIRED,
    )
    sanitized = await orchestrator.push_ref(sanitized_ref)

    assert sanitized.action == "replaced"
    assert sanitized.ironrag_document_id == original_document_id
    assert b"stale sensitive comment marker" not in ironrag.payloads[(LIB, old_ref.external_key)]
    row = state.get("page", "1")
    assert row is not None
    assert row.change_token == sanitized_ref.change_token

    orchestrator.reset_sweep_cache()
    unchanged = await orchestrator.push_ref(sanitized_ref)
    assert unchanged.action == "noop_unchanged"
    assert adapter.fetch_calls == 3


@pytest.mark.asyncio
async def test_required_materialization_still_honors_on_new_skip_when_target_absent(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("new-v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe new document",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_new=UpsertAction.SKIP),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "skipped_new"
    assert ironrag.creates == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []
    assert state.get(ref.kind, ref.item_id) is None


@pytest.mark.asyncio
async def test_required_materialization_denies_every_historical_exact_key_copy(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("v2-comment-deleted-before-census")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    for library_id, document_id in (
        (LIB2, DOC_OLD),
        (LIB, DOC_PRE),
    ):
        state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token="v1",
            external_key=ref.external_key,
            ironrag_document_id=str(document_id),
            ironrag_library_id=str(library_id),
        )
        ironrag.documents[(library_id, ref.external_key)] = _document(
            document_id,
            library_id,
            ref.external_key,
        )
    # Exact-key copies can also predate the durable location registry. Cover
    # both a configured historical scope and the sweep's inherited scope.
    ironrag.documents[(LIB3, ref.external_key)] = _document(DOC_IMAGE_PRE, LIB3, ref.external_key)
    inherited_id = _doc_id(4)
    ironrag.documents[(LIB4, ref.external_key)] = _document(inherited_id, LIB4, ref.external_key)
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"deleted comment omitted",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=_router_with_configured_secondary(LIB, LIB3),
        policy=PushPolicy(
            on_changed=UpdateAction.SKIP,
            on_missing=DeleteAction.IGNORE,
        ),
    )
    orchestrator.reset_sweep_cache({"page": {LIB4}})

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "replaced"
    assert outcome.ironrag_document_id == str(DOC_PRE)
    assert ironrag.deletes == []
    assert len(state.locations_for(ref.kind, ref.item_id)) == 4
    for library in (LIB2, LIB3, LIB4):
        retired = ironrag.documents[(library, ref.external_key)]
        assert retired.visibility_intent_id is not None
        assert retired.readable_revision_id is None
        attempts = state.materialization_attempts_for_document(str(retired.id))
        assert len(attempts) == 1
        assert attempts[0].receipt is not None
    assert len(ironrag.revisions) == 1
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == ref.change_token


@pytest.mark.asyncio
async def test_required_materialization_denies_history_and_refetches_equal_token(
    tmp_path: Path,
) -> None:
    """An already-advanced cursor cannot hide a migrated historical copy."""
    ref = _required_page_ref("already-applied-v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    for library_id, document_id in ((LIB2, DOC_OLD), (LIB, DOC_PRE)):
        state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token=ref.change_token,
            external_key=ref.external_key,
            ironrag_document_id=str(document_id),
            ironrag_library_id=str(library_id),
        )
        ironrag.documents[(library_id, ref.external_key)] = _document(
            document_id,
            library_id,
            ref.external_key,
        )
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"complete safe projection",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_changed=UpdateAction.SKIP, on_missing=DeleteAction.IGNORE),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "replaced"
    assert outcome.ironrag_document_id == str(DOC_PRE)
    assert adapter.fetch_calls == 1
    assert ironrag.deletes == []
    assert ironrag.documents[(LIB2, ref.external_key)].visibility_intent_id is not None
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == ref.change_token


@pytest.mark.asyncio
async def test_required_equal_token_denies_history_when_current_copy_is_missing(
    tmp_path: Path,
) -> None:
    """A missing current copy must not bypass cleanup on the equal-token path."""
    ref = _required_page_ref("already-applied-v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token=ref.change_token,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB2),
    )
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token=ref.change_token,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    ironrag.documents[(LIB2, ref.external_key)] = _document(
        DOC_OLD,
        LIB2,
        ref.external_key,
    )
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe current projection",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_changed=UpdateAction.SKIP, on_missing=DeleteAction.IGNORE),
    )

    outcome = await orchestrator.push_ref(ref)

    assert outcome.action == "created"
    assert adapter.fetch_calls == 1
    assert ironrag.deletes == []
    assert ironrag.documents[(LIB2, ref.external_key)].visibility_intent_id is not None
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == ref.change_token


@pytest.mark.asyncio
async def test_required_materialization_retries_after_historical_intent_failure(
    tmp_path: Path,
) -> None:
    class FailFirstIntent(FakeIronRag):
        def __init__(self) -> None:
            super().__init__()
            self.fail_next_poll = True

        async def create_visibility_intent(
            self, document_id: UUID | str, *, request: VisibilityIntentRequest
        ) -> VisibilityIntentReceipt:
            if self.fail_next_poll:
                self.fail_next_poll = False
                raise IronRagError("synthetic historical intent failure")
            return await super().create_visibility_intent(document_id, request=request)

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FailFirstIntent()
    for library_id, document_id in ((LIB2, DOC_OLD), (LIB, DOC_PRE)):
        state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token="v1",
            external_key=ref.external_key,
            ironrag_document_id=str(document_id),
            ironrag_library_id=str(library_id),
        )
        ironrag.documents[(library_id, ref.external_key)] = _document(
            document_id,
            library_id,
            ref.external_key,
        )
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_changed=UpdateAction.SKIP, on_missing=DeleteAction.IGNORE),
    )

    with pytest.raises(IronRagError, match="historical intent failure"):
        await orchestrator.push_ref(ref)

    row_after_failure = state.get(ref.kind, ref.item_id)
    assert row_after_failure is not None
    assert row_after_failure.change_token == "v1"
    assert ironrag.revisions == []

    orchestrator.reset_sweep_cache()
    retried = await orchestrator.push_ref(ref)
    assert retried.action == "replaced"
    row_after_retry = state.get(ref.kind, ref.item_id)
    assert row_after_retry is not None
    assert row_after_retry.change_token == "v2"


@pytest.mark.asyncio
async def test_required_retirement_persists_rediscovered_history_until_intent_succeeds(
    tmp_path: Path,
) -> None:
    class FailFirstIntentSubmission(FakeIronRag):
        def __init__(self) -> None:
            super().__init__()
            self.fail_next_delete = True

        async def create_visibility_intent(
            self,
            document_id: UUID | str,
            *,
            request: VisibilityIntentRequest,
        ) -> VisibilityIntentReceipt:
            if self.fail_next_delete:
                self.fail_next_delete = False
                raise IronRagError("synthetic historical intent submission failure")
            return await super().create_visibility_intent(document_id, request=request)

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FailFirstIntentSubmission()
    # The durable row has an obsolete legacy id, while exact-key discovery
    # finds the replacement that must remain in retry scope across a restart.
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token="v1",
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB2),
    )
    state.upsert(
        kind=ref.kind,
        item_id=ref.item_id,
        change_token="v1",
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_PRE),
        ironrag_library_id=str(LIB),
    )
    rediscovered_id = _doc_id(8)
    ironrag.documents[(LIB2, ref.external_key)] = _document(
        rediscovered_id,
        LIB2,
        ref.external_key,
    )
    ironrag.documents[(LIB, ref.external_key)] = _document(
        DOC_PRE,
        LIB,
        ref.external_key,
    )
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(IronRagError, match="historical intent submission failure"):
        await orchestrator.push_ref(ref)

    orchestrator.reset_sweep_cache()
    retried = await orchestrator.push_ref(ref)

    assert retried.action == "replaced"
    assert ironrag.deletes == []
    retired = ironrag.documents[(LIB2, ref.external_key)]
    assert retired.id == rediscovered_id
    assert retired.readable_revision_id is None
    assert state.get_location(ref.kind, ref.item_id, str(LIB2)).ironrag_document_id == str(
        rediscovered_id
    )
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == ref.change_token


@pytest.mark.asyncio
async def test_required_materialization_retry_after_current_apply_failure_retains_target(
    tmp_path: Path,
) -> None:
    class FailFirstRevisionPoll(FakeIronRag):
        def __init__(self) -> None:
            super().__init__()
            self.fail_next_poll = True

        async def wait_for_operation(
            self,
            operation_id: UUID | str,
            *,
            poll_interval: float | None = None,
            budget: float | None = None,
        ) -> OperationStatus:
            if self.fail_next_poll:
                self.fail_next_poll = False
                raise IronRagError("synthetic current apply failure")
            return await super().wait_for_operation(
                operation_id,
                poll_interval=poll_interval,
                budget=budget,
            )

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FailFirstRevisionPoll()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_changed=UpdateAction.SKIP),
    )

    with pytest.raises(IronRagError, match="current apply failure"):
        await orchestrator.push_ref(ref)

    # A transport failure leaves the target in place; the old readable
    # revision is suppressed by the admission policy, so no delete is needed.
    assert ironrag.deletes == []
    assert (LIB, ref.external_key) in ironrag.documents

    orchestrator.reset_sweep_cache()
    retried = await orchestrator.push_ref(ref)
    assert retried.action == "replaced"
    assert retried.ironrag_document_id == str(DOC_PRE)
    row_after_retry = state.get(ref.kind, ref.item_id)
    assert row_after_retry is not None
    assert row_after_retry.change_token == "v2"


async def _cancel_required_push(tmp_path: Path, *, repeated: bool) -> None:
    """Cancels a REQUIRED apply mid-flight and asserts the target survives.

    ``repeated`` also hammers the task with further cancellations to prove the
    retained outcome is not contingent on a single cancel landing cleanly.
    """
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = CancelledRevisionPoll()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    task = asyncio.create_task(orchestrator.push_ref(ref))
    await ironrag.revision_waiting.wait()

    task.cancel()
    if repeated:
        for _ in range(2_000):
            task.cancel()
            await asyncio.sleep(0)
            if task.done():
                break
        assert task.done()

    with pytest.raises(asyncio.CancelledError):
        await task

    assert ironrag.deletes == [], "cancellation must not delete the accepted target"
    assert (LIB, ref.external_key) in ironrag.documents


@pytest.mark.asyncio
async def test_required_materialization_cancellation_retains_target(tmp_path: Path) -> None:
    await _cancel_required_push(tmp_path, repeated=False)


@pytest.mark.asyncio
async def test_required_materialization_repeated_cancellation_retains_target(
    tmp_path: Path,
) -> None:
    await _cancel_required_push(tmp_path, repeated=True)


@pytest.mark.asyncio
async def test_required_materialization_deferred_apply_retains_target(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    ironrag.revision_conflicts.add(str(DOC_PRE))
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)

    deferred = await orchestrator.push_ref(ref)

    assert deferred.deferred is True
    assert deferred.ironrag_document_id == str(DOC_PRE)
    assert ironrag.deletes == [], "a deferred REQUIRED apply must not delete the target"
    assert (LIB, ref.external_key) in ironrag.documents
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == "v1", "the deferred replacement must retry next sweep"

    ironrag.revision_conflicts.clear()
    orchestrator.reset_sweep_cache()
    retried = await orchestrator.push_ref(ref)
    assert retried.action == "replaced"
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == "v2"


class AcceptedThenTimeoutIronRag(FakeIronRag):
    """The server accepted the replacement operation, but the connector's poll
    budget expires while it is still nonterminal (accepted/running)."""

    def __init__(self) -> None:
        super().__init__()
        self.revision_operation_id: UUID | None = None

    async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> OperationHandle:
        handle = await super().create_revision(document_id, **kwargs)
        self.revision_operation_id = handle.operation_id
        self._operations[handle.operation_id] = OperationStatus(
            id=handle.operation_id,
            workspace_id=WS,
            library_id=LIB,
            operation_kind="revise_document",
            status=OperationStatusValue.ACCEPTED,
            created_at=datetime.now(tz=UTC),
            progress=OperationProgress(),
        )
        return handle

    async def wait_for_operation(
        self,
        operation_id: UUID | str,
        *,
        poll_interval: float | None = None,
        budget: float | None = None,
    ) -> OperationStatus:
        if (
            self.revision_operation_id is not None
            and UUID(str(operation_id)) == self.revision_operation_id
        ):
            raise IronRagMutationTimeoutError("synthetic poll budget exceeded")
        return await super().wait_for_operation(
            operation_id,
            poll_interval=poll_interval,
            budget=budget,
        )


class FailFirstTerminalIronRag(FakeIronRag):
    """The first poll reports a proven terminal failure of our own submission."""

    def __init__(self) -> None:
        super().__init__()
        self.fail_next_terminal = True

    async def wait_for_operation(
        self,
        operation_id: UUID | str,
        *,
        poll_interval: float | None = None,
        budget: float | None = None,
    ) -> OperationStatus:
        if self.fail_next_terminal:
            self.fail_next_terminal = False
            raise IronRagOperationFailedError(
                OperationStatus(
                    id=UUID(str(operation_id)),
                    workspace_id=WS,
                    library_id=LIB,
                    operation_kind="revise_document",
                    status=OperationStatusValue.FAILED,
                    created_at=datetime.now(tz=UTC),
                    progress=OperationProgress(),
                )
            )
        return await super().wait_for_operation(
            operation_id,
            poll_interval=poll_interval,
            budget=budget,
        )


@pytest.mark.asyncio
async def test_required_terminal_failure_retries_with_fresh_nonce(tmp_path: Path) -> None:
    """A proven terminal failure of our own submission rolls the durable retry
    nonce once, so the next sweep submits a fresh idempotency key instead of
    replaying the terminal record forever. The target itself is retained."""
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FailFirstTerminalIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)

    with pytest.raises(IronRagOperationFailedError):
        await orchestrator.push_ref(ref)

    assert ironrag.deletes == [], "a terminal failure must not delete the accepted target"
    assert (LIB, ref.external_key) in ironrag.documents
    first_key = ironrag.revisions[0]["idempotency_key"]

    orchestrator.reset_sweep_cache()
    retried = await orchestrator.push_ref(ref)

    assert retried.action == "replaced"
    second_key = ironrag.revisions[1]["idempotency_key"]
    assert second_key != first_key, (
        "a proven terminal failure must roll a fresh idempotency key once"
    )
    row = state.get(ref.kind, ref.item_id)
    assert row is not None
    assert row.change_token == "v2"


@pytest.mark.asyncio
async def test_required_poll_timeout_retains_accepted_target(tmp_path: Path) -> None:
    """A REQUIRED replacement whose accepted operation outlives the connector's
    poll budget must be retained, not deleted.

    Deleting the current target cancels the accepted replacement job and
    destroys the document. The old readable revision is instead suppressed by
    the independently accepted visibility intent, so the
    connector keeps the accepted target and defers to a later sweep.
    """
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = AcceptedThenTimeoutIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe v2",
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(ref)

    assert out.deferred is True
    assert ironrag.deletes == [], (
        "a nonterminal accepted REQUIRED replacement must not be deleted; "
        "deletion cancels the accepted job and destroys the target"
    )
    assert (LIB, ref.external_key) in ironrag.documents, (
        "the accepted target document must be retained until the new revision is terminal"
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("lost_phase", [None, "intent", "payload"])
async def test_visibility_contract_submits_server_normalized_replacement_metadata(
    tmp_path: Path,
    lost_phase: str | None,
) -> None:
    class NormalizingIronRag(FakeIronRag):
        payload_metadata: dict[str, Any]
        lost = False

        async def create_visibility_intent(
            self, document_id: UUID | str, *, request: VisibilityIntentRequest
        ) -> VisibilityIntentReceipt:
            receipt = await super().create_visibility_intent(document_id, request=request)
            normalized = receipt.model_copy(
                update={
                    "replacement": receipt.replacement.model_copy(
                        update={
                            "mime_type": "text/markdown",
                            "file_name": "1.md",
                            "title": "Page",
                            "document_hint": "Hint",
                        }
                    )
                }
            )
            self.visibility_intents[request.idempotency_key] = normalized
            if lost_phase == "intent" and not self.lost:
                self.lost = True
                raise IronRagError("lost normalized receipt")
            return normalized

        async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> OperationHandle:
            self.payload_metadata = kwargs
            handle = await super().create_revision(document_id, **kwargs)
            if lost_phase == "payload" and not self.lost:
                self.lost = True
                raise IronRagError("lost bound payload receipt")
            return handle

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = NormalizingIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        SourceFetchMaterialized(
            SourceItem(
                ref=ref,
                payload=b"safe replacement",
                mime_type=" TEXT/MARKDOWN ",
                file_name=" 1.md ",
                title=" Page ",
                document_hint=" Hint ",
            ),
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )
    )

    if lost_phase is not None:
        orchestrator = _orchestrator(adapter, ironrag, state)
        with pytest.raises(IronRagError, match="lost"):
            await orchestrator.push_ref(ref)
        saved_receipt = next(iter(ironrag.visibility_intents.values()))
        state.close()
        state = _state(tmp_path)
    result = await _orchestrator(adapter, ironrag, state).push_ref(ref)

    assert result.action == "replaced"
    assert not result.deferred
    request = ironrag.visibility_requests[0]
    assert request.replacement.mime_type == " TEXT/MARKDOWN "
    receipt = ironrag.visibility_intents[request.idempotency_key]
    assert ironrag.payload_metadata["visibility_intent_id"] == receipt.visibility_intent_id
    assert ironrag.payload_metadata["file_name"] == "1.md"
    assert ironrag.payload_metadata["mime_type"] == "text/markdown"
    assert ironrag.payload_metadata["title"] == "Page"
    assert ironrag.payload_metadata["document_hint"] == "Hint"
    assert ironrag.payload_metadata["provenance"].language_code is None
    assert ironrag.payload_metadata["provenance"].source_uri is None
    if lost_phase is not None:
        assert receipt == saved_receipt
        assert len(ironrag.visibility_intents) == 1
        if lost_phase == "intent":
            assert ironrag.visibility_requests[0] == ironrag.visibility_requests[1]


@pytest.mark.asyncio
@pytest.mark.parametrize("terminal", list(OperationStatusValue))
async def test_direct_reaper_foreign_operation_preserves_durable_target_evidence(
    tmp_path: Path, terminal: OperationStatusValue
) -> None:
    class ForeignDeletePoll(FakeIronRag):
        foreign = True
        accepted = _doc_id(9201)

        async def delete_document(
            self, document_id: UUID | str, *, idempotency_key: str
        ) -> OperationHandle:
            assert str(document_id) == str(DOC_PRE)
            return OperationHandle(operation_id=self.accepted)

        async def wait_for_operation(
            self,
            operation_id: UUID | str,
            *,
            poll_interval: float | None = None,
            budget: float | None = None,
        ) -> OperationStatus:
            assert UUID(str(operation_id)) == self.accepted
            return _ready_operation(_doc_id(9202) if self.foreign else self.accepted).model_copy(
                update={"status": terminal if self.foreign else OperationStatusValue.READY}
            )

    state = _state(tmp_path)
    ironrag = ForeignDeletePoll()
    ref = _page_ref("v1")
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = EchoAdapter({})
    orchestrator = _orchestrator(adapter, ironrag, state)
    cursor = state.get(ref.kind, ref.item_id)
    locations = state.locations_for(ref.kind, ref.item_id)
    location = state.get_location(ref.kind, ref.item_id, str(LIB))
    document_id = str(DOC_PRE)
    policy = PushPolicy()
    with pytest.raises(IronRagError, match="target absence is not established"):
        await orchestrator.reap_orphan(
            ref,
            LIB,
            document_id,
            policy,
            enforcement=SourceDeleteEnforcement.REQUIRED,
            expected_cursor=cursor,
            expected_location=location,
        )
    assert state.get(ref.kind, ref.item_id) == cursor
    assert state.locations_for(ref.kind, ref.item_id) == locations
    state.close()
    reopened = _state(tmp_path)
    assert reopened.get(ref.kind, ref.item_id) == cursor
    assert reopened.locations_for(ref.kind, ref.item_id) == locations
    ironrag.foreign = False
    result = await _orchestrator(adapter, ironrag, reopened).reap_orphan(
        ref,
        LIB,
        str(DOC_PRE),
        PushPolicy(),
        enforcement=SourceDeleteEnforcement.REQUIRED,
        expected_cursor=cursor,
        expected_location=location,
    )
    assert result.action == "deleted"
    assert reopened.get(ref.kind, ref.item_id) is None
    assert reopened.locations_for(ref.kind, ref.item_id) == []
    reopened.close()


@pytest.mark.asyncio
@pytest.mark.parametrize("lose_payload_receipt", [False, True])
async def test_bound_nonnull_language_and_uri_reopen_uses_accepted_metadata(
    tmp_path: Path, lose_payload_receipt: bool
) -> None:
    class MetadataReceiptIronRag(FakeIronRag):
        payloads: list[dict[str, Any]]
        lost = False

        def __init__(self) -> None:
            super().__init__()
            self.payloads = []

        async def create_visibility_intent(
            self, document_id: UUID | str, *, request: VisibilityIntentRequest
        ) -> VisibilityIntentReceipt:
            receipt = await super().create_visibility_intent(document_id, request=request)
            normalized = receipt.model_copy(
                update={
                    "replacement": receipt.replacement.model_copy(
                        update={"language_code": "ru", "source_uri": "https://example.invalid/safe"}
                    )
                }
            )
            self.visibility_intents[request.idempotency_key] = normalized
            return normalized

        async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> OperationHandle:
            self.payloads.append(kwargs)
            handle = await super().create_revision(document_id, **kwargs)
            if lose_payload_receipt and not self.lost:
                self.lost = True
                raise IronRagError("lost metadata payload receipt")
            return handle

    state = _state(tmp_path)
    ironrag = MetadataReceiptIronRag()
    ref = _required_page_ref("v2")
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    item = SourceItem(
        ref=ref, payload=b"safe replacement", mime_type="text/markdown", file_name="1.md"
    )
    # Exercise the bound-payload owner with a real persisted accepted request.
    # The nullable adapter's fields are not used to reconstruct the receipt.
    observation = state.observe_materialization(
        adapter="echo",
        kind=ref.kind,
        item_id=ref.item_id,
        external_key=ref.external_key,
        library_id=str(LIB),
        target_document_id=str(DOC_PRE),
        materialization_token=ref.change_token,
        replacement=ReplacementIdentity(
            checksum=item.payload_checksum,
            byte_size=len(item.payload),
            mime_type=item.mime_type,
            file_name=item.file_name,
            language_code=" ru ",
            source_uri=" https://example.invalid/safe ",
        ),
    )
    attempt = state.prepare_materialization_attempt(
        observation, document_id=str(DOC_PRE), library_id=str(LIB)
    )
    attempt = state.transition_materialization_attempt(attempt, phase="intent_submitting")
    assert attempt.request is not None
    receipt = await ironrag.create_visibility_intent(DOC_PRE, request=attempt.request)
    state.transition_materialization_attempt(attempt, phase="intent_accepted", receipt=receipt)
    state.close()
    reopened = _state(tmp_path)
    attempt = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert attempt.receipt == receipt
    route = _router().resolve(ref)
    adapter = EchoAdapter({})
    orchestrator = _orchestrator(adapter, ironrag, reopened)
    if lose_payload_receipt:
        document_id = str(DOC_PRE)
        with pytest.raises(IronRagError, match="lost metadata payload receipt"):
            await orchestrator._revise(
                item, route, document_id, (LIB, ref.external_key), None, visibility_attempt=attempt
            )
        saved = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
        reopened.close()
        reopened = _state(tmp_path)
        attempt = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
        assert attempt.payload_key == saved.payload_key
        assert attempt.payload_identity == saved.payload_identity
        assert attempt.receipt == receipt
    result = await _orchestrator(adapter, ironrag, reopened)._revise(
        item, route, str(DOC_PRE), (LIB, ref.external_key), None, visibility_attempt=attempt
    )
    assert result.action == "replaced"
    assert not result.deferred
    for sent in ironrag.payloads:
        assert sent["provenance"].language_code == "ru"
        assert sent["provenance"].source_uri == "https://example.invalid/safe"
        assert sent["visibility_intent_id"] == receipt.visibility_intent_id
        assert sent["idempotency_key"] == attempt.payload_key
    after = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert after.receipt == receipt
    assert after.receipt.revision_floor == receipt.revision_floor
    assert len(ironrag.visibility_intents) == 1
    reopened.close()


@pytest.mark.asyncio
async def test_visibility_contract_required_busy_has_accepted_denial(tmp_path: Path) -> None:
    class VisibilityProbe(FakeIronRag):
        protected = False

        async def create_visibility_intent(
            self, *args: Any, **kwargs: Any
        ) -> VisibilityIntentReceipt:
            receipt = await super().create_visibility_intent(*args, **kwargs)
            self.protected = True
            return receipt

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = VisibilityProbe()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    ironrag.revision_conflicts.add(str(DOC_PRE))
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe v2", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    out = await _orchestrator(adapter, ironrag, state).push_ref(ref)
    assert out.deferred
    assert ironrag.protected, "REQUIRED busy deferral retained old A without an accepted denial"
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    head = ironrag.documents[(LIB, ref.external_key)]
    assert head.readable_revision_id is None
    assert head.visibility_intent_id is not None
    assert ironrag.revisions == []
    attempts = state.materialization_attempts_for_document(str(DOC_PRE))
    assert attempts[0].receipt is not None


@pytest.mark.asyncio
@pytest.mark.parametrize("lost_phase", ["intent", "payload"])
async def test_visibility_contract_lost_receipt_reopens_exact_episode(
    tmp_path: Path, lost_phase: str
) -> None:
    class LoseOnce(FakeIronRag):
        lost = False

        async def create_visibility_intent(
            self, *args: Any, **kwargs: Any
        ) -> VisibilityIntentReceipt:
            receipt = await super().create_visibility_intent(*args, **kwargs)
            if lost_phase == "intent" and not self.lost:
                self.lost = True
                raise IronRagError("lost visibility receipt")
            return receipt

        async def create_revision(self, *args: Any, **kwargs: Any) -> OperationHandle:
            handle = await super().create_revision(*args, **kwargs)
            if lost_phase == "payload" and not self.lost:
                self.lost = True
                raise IronRagError("lost payload receipt")
            return handle

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = LoseOnce()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    with pytest.raises(IronRagError, match="lost"):
        await orchestrator.push_ref(ref)
    before = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert (before.receipt is None) == (lost_phase == "intent")
    assert before.payload_receipt is None
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    original_receipt = next(iter(ironrag.visibility_intents.values()))
    state.close()
    reopened = _state(tmp_path)
    recovered = await _orchestrator(adapter, ironrag, reopened).push_ref(ref)
    assert recovered.action == "replaced"
    after = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert after.request == before.request
    assert after.payload_key == before.payload_key
    assert after.receipt == original_receipt
    assert after.receipt.revision_floor == original_receipt.revision_floor
    assert after.payload_receipt is not None
    assert len(ironrag.visibility_intents) == 1
    if lost_phase == "payload":
        assert ironrag.revisions[0]["idempotency_key"] == ironrag.revisions[1]["idempotency_key"]
    reopened.close()


@pytest.mark.asyncio
async def test_visibility_contract_a_b_a_and_old_replay_do_not_move_head(tmp_path: Path) -> None:
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    first = _required_page_ref("z")
    _seed_existing_page(state, ironrag, first, change_token="initial")
    adapter = FixedFetchResultAdapter(
        _materialized_page(first, b"A", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    for token, payload in (("z", b"A"), ("a", b"B"), ("z", b"A")):
        ref = _required_page_ref(token)
        adapter.result = _materialized_page(
            ref, payload, enforcement=SourceMaterializationEnforcement.REQUIRED
        )
        assert (await orchestrator.push_ref(ref)).action == "replaced"
    attempts = state.materialization_attempts_for_document(str(DOC_PRE))
    assert len({attempt.episode_id for attempt in attempts}) == 3
    assert len({attempt.request.idempotency_key for attempt in attempts}) == 3
    current = ironrag.documents[(LIB, first.external_key)].visibility_intent_id
    old_request = ironrag.visibility_requests[0]
    replay = await ironrag.create_visibility_intent(DOC_PRE, request=old_request)
    assert replay == attempts[0].receipt
    assert ironrag.documents[(LIB, first.external_key)].visibility_intent_id == current
    assert (await orchestrator.push_ref(first)).action == "noop_unchanged"
    assert len(ironrag.visibility_intents) == 3


@pytest.mark.asyncio
async def test_visibility_contract_superseded_needs_changed_observation(tmp_path: Path) -> None:
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    assert (await orchestrator.push_ref(ref)).action == "replaced"
    head = ironrag.documents[(LIB, ref.external_key)]
    ironrag.documents[(LIB, ref.external_key)] = head.model_copy(
        update={"visibility_intent_id": _doc_id(9999)}
    )
    assert (await orchestrator.push_ref(ref)).deferred
    assert (await orchestrator.push_ref(ref)).deferred
    assert len(ironrag.visibility_intents) == 1
    changed = _required_page_ref("v3")
    adapter.result = _materialized_page(
        changed, b"safe changed", enforcement=SourceMaterializationEnforcement.REQUIRED
    )
    assert (await orchestrator.push_ref(changed)).action == "replaced"
    assert len(ironrag.visibility_intents) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "field,value",
    [
        ("result_revision_id", None),
        ("mutation_item_id", None),
        ("visibility_intent_id", _doc_id(9999)),
        ("result_checksum", "sha256:" + "0" * 64),
        ("request_fingerprint", None),
    ],
)
async def test_visibility_contract_rejects_payload_receipt_mismatch(
    tmp_path: Path, field: str, value: object
) -> None:
    class WrongReceipt(FakeIronRag):
        async def create_revision(self, *args: Any, **kwargs: Any) -> OperationHandle:
            handle = await super().create_revision(*args, **kwargs)
            return handle.model_copy(update={field: value})

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = WrongReceipt()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    with pytest.raises(IronRagError, match="binding"):
        await orchestrator.push_ref(ref)
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    assert state.materialization_attempts_for_document(str(DOC_PRE))[0].receipt is not None
    assert ironrag.deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "wrong_field",
    ["readable_revision_id", "readable_mutation_item_id", "visibility_intent_id", "operation_id"],
)
async def test_visibility_contract_ready_is_not_current_head_proof(
    tmp_path: Path, wrong_field: str
) -> None:
    class WrongReady(FakeIronRag):
        async def wait_for_operation(self, *args: Any, **kwargs: Any) -> OperationStatus:
            status = await super().wait_for_operation(*args, **kwargs)
            if wrong_field == "operation_id":
                return status.model_copy(update={"id": _doc_id(9999)})
            key = (LIB, "echo:page:1")
            self.documents[key] = self.documents[key].model_copy(
                update={wrong_field: _doc_id(9999)}
            )
            return status

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = WrongReady()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    if wrong_field == "operation_id":
        with pytest.raises(IronRagError, match="operation"):
            await orchestrator.push_ref(ref)
    else:
        assert (await orchestrator.push_ref(ref)).deferred
    assert state.get(ref.kind, ref.item_id).change_token == "v1"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "code",
    ["visibility_binding_conflict", "visibility_intent_required", "visibility_intent_superseded"],
)
async def test_visibility_contract_typed_revision_conflicts_are_not_busy(
    tmp_path: Path, code: str
) -> None:
    class TypedConflict(FakeIronRag):
        async def create_revision(self, *args: Any, **kwargs: Any) -> OperationHandle:
            raise IronRagVisibilityConflictError(
                ProblemDetails(
                    type="urn:ironrag:error:" + code,
                    title="Visibility conflict",
                    status=409,
                    detail="not a text-matched busy mutation",
                    code=code,
                )
            )

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = TypedConflict()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    if code == "visibility_intent_superseded":
        assert (await orchestrator.push_ref(ref)).deferred
        assert (await orchestrator.push_ref(ref)).deferred
        assert len(ironrag.visibility_intents) == 1
    else:
        with pytest.raises(IronRagVisibilityConflictError) as raised:
            await orchestrator.push_ref(ref)
        assert raised.value.problem.code == code
    assert state.get(ref.kind, ref.item_id).change_token == "v1"


@pytest.mark.asyncio
@pytest.mark.parametrize("version", [{}, {"contentVisibilityContract": "wrong"}])
async def test_visibility_contract_sync_refuses_before_source_enumeration(
    tmp_path: Path, version: dict[str, str]
) -> None:
    class EnumerationProbe(EchoAdapter):
        enumerated = False

        async def iter_items(self) -> AsyncIterator[SourceItemRef]:
            self.enumerated = True
            async for ref in super().iter_items():
                yield ref

    adapter = EnumerationProbe({})
    paths: list[str] = []

    def handle(request: httpx.Request) -> httpx.Response:
        paths.append(request.url.path)
        return httpx.Response(200, json=version)

    ironrag = IronRagClient(
        BaseConnectorSettings(
            ironrag_base_url="http://ironrag.invalid", ironrag_api_token="test-token"
        ),
        client=httpx.AsyncClient(
            base_url="http://ironrag.invalid", transport=httpx.MockTransport(handle)
        ),
    )
    state = _state(tmp_path)
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    router = _router()
    orchestrator = Orchestrator(
        adapter=adapter, ironrag=ironrag, router=router, state=state, policies=policies
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=policies,
        concurrency=1,
        interval_seconds=60,
    )
    with pytest.raises(IronRagError, match="contentVisibilityContract"):
        await manager.run_once(reason="capability-proof")
    assert not adapter.enumerated
    assert paths == ["/v1/version"]
    await ironrag.aclose()


@pytest.mark.asyncio
async def test_visibility_contract_historical_foreign_winner_finishes_without_delete(
    tmp_path: Path,
) -> None:
    class ForeignCompletion(FakeIronRag):
        foreign_succeeded = False

        async def wait_for_operation(self, *args: Any, **kwargs: Any) -> OperationStatus:
            self.foreign_succeeded = True
            # Foreign B's own job succeeded but is not bound to the current
            # historical intent; its completion must not republish old bytes.
            retired = self.documents[(LIB2, "echo:page:1")]
            assert retired.visibility_intent_id is not None
            assert retired.readable_revision_id is None
            return await super().wait_for_operation(*args, **kwargs)

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = ForeignCompletion()
    for library, doc_id in ((LIB2, DOC_OLD), (LIB, DOC_PRE)):
        state.upsert(
            kind=ref.kind,
            item_id=ref.item_id,
            change_token="v1",
            external_key=ref.external_key,
            ironrag_document_id=str(doc_id),
            ironrag_library_id=str(library),
        )
        ironrag.documents[(library, ref.external_key)] = _document(
            doc_id, library, ref.external_key
        )
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    assert (await _orchestrator(adapter, ironrag, state).push_ref(ref)).action == "replaced"
    assert ironrag.foreign_succeeded
    assert ironrag.deletes == []
    assert [revision["document_id"] for revision in ironrag.revisions] == [str(DOC_PRE)]
    state.close()
    reopened = _state(tmp_path)
    attempts = reopened.materialization_attempts_for_document(str(DOC_OLD))
    assert attempts[0].receipt is not None
    assert attempts[0].phase == "intent_accepted"
    assert reopened.get_location(ref.kind, ref.item_id, str(LIB2)) is not None
    reopened.close()


@pytest.mark.asyncio
async def test_visibility_contract_changed_complete_metadata_starts_new_episode(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("equal-token")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="initial")
    original = _materialized_page(
        ref, b"same bytes", enforcement=SourceMaterializationEnforcement.REQUIRED
    )
    adapter = FixedFetchResultAdapter(original)
    orchestrator = _orchestrator(adapter, ironrag, state)
    assert (await orchestrator.push_ref(ref)).action == "replaced"
    adapter.result = replace(
        original, item=replace(original.item, title="Changed title", document_hint="Changed hint")
    )
    assert (await orchestrator.push_ref(ref)).action == "replaced"
    attempts = state.materialization_attempts_for_document(str(DOC_PRE))
    assert len(attempts) == 2
    assert attempts[0].request.replacement.checksum == attempts[1].request.replacement.checksum
    assert attempts[0].request.idempotency_key != attempts[1].request.idempotency_key
    assert attempts[1].request.replacement.title == "Changed title"


@pytest.mark.asyncio
async def test_visibility_contract_historical_membership_change_withholds_applied_token(
    tmp_path: Path,
) -> None:
    class IntroduceCopy(FakeIronRag):
        introduced = False

        async def wait_for_operation(self, *args: Any, **kwargs: Any) -> OperationStatus:
            status = await super().wait_for_operation(*args, **kwargs)
            if not self.introduced:
                self.introduced = True
                self.documents[(LIB3, "echo:page:1")] = _document(
                    _doc_id(9998), LIB3, "echo:page:1"
                )
            return status

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = IntroduceCopy()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(
        adapter, ironrag, state, router=_router_with_configured_secondary(LIB, LIB3)
    )
    assert (await orchestrator.push_ref(ref)).deferred
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    assert state.get_location(ref.kind, ref.item_id, str(LIB3)) is not None
    assert (await orchestrator.push_ref(ref)).action == "replaced"
    assert ironrag.documents[(LIB3, ref.external_key)].visibility_intent_id is not None
    assert ironrag.documents[(LIB3, ref.external_key)].readable_revision_id is None
    assert ironrag.deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize("terminal", [OperationStatusValue.FAILED, OperationStatusValue.CANCELED])
async def test_visibility_contract_terminal_payload_retry_keeps_intent_floor(
    tmp_path: Path, terminal: OperationStatusValue
) -> None:
    class TerminalOnce(FakeIronRag):
        first = True

        async def wait_for_operation(
            self, operation_id: UUID | str, **kwargs: Any
        ) -> OperationStatus:
            if self.first:
                self.first = False
                status = self._operations[UUID(str(operation_id))].model_copy(
                    update={"status": terminal}
                )
                if terminal is OperationStatusValue.FAILED:
                    raise IronRagOperationFailedError(status)
                return status
            return await super().wait_for_operation(operation_id, **kwargs)

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = TerminalOnce()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    if terminal is OperationStatusValue.FAILED:
        with pytest.raises(IronRagOperationFailedError):
            await orchestrator.push_ref(ref)
    else:
        assert (await orchestrator.push_ref(ref)).deferred
    before = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert before.phase == "payload_terminal"
    assert before.payload_receipt is None
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    assert (await orchestrator.push_ref(ref)).action == "replaced"
    after = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert after.receipt == before.receipt
    assert len(ironrag.visibility_intents) == 1
    assert ironrag.revisions[0]["idempotency_key"] != ironrag.revisions[1]["idempotency_key"]


@pytest.mark.asyncio
async def test_required_creation_empty_history_fences_concurrent_membership(tmp_path: Path) -> None:
    ref = _required_page_ref("new")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    entered, resume = asyncio.Event(), asyncio.Event()

    class PauseClosure(Orchestrator):
        async def _required_copy_closure(self, *args: Any, **kwargs: Any) -> bool:
            result = await super()._required_copy_closure(*args, **kwargs)
            assert result
            assert args[3] == ()
            entered.set()
            await resume.wait()
            return result

    orchestrator = PauseClosure(
        adapter=adapter,
        ironrag=ironrag,
        router=_router(),
        state=state,
        policies=PolicyOverrides(default=PushPolicy(), by_kind={}),
    )
    pending = asyncio.create_task(orchestrator.push_ref(ref))
    await entered.wait()
    second = _state(tmp_path)
    second.record_document_location(
        kind=ref.kind,
        item_id=ref.item_id,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB2),
    )
    second.close()
    resume.set()
    with pytest.raises(ValueError, match="membership changed"):
        await pending
    assert state.get(ref.kind, ref.item_id) is None
    assert state.get_location(ref.kind, ref.item_id, str(LIB2)) is not None
    assert ironrag.deletes == []
    assert ironrag.visibility_requests == []


@pytest.mark.asyncio
@pytest.mark.parametrize("unknown_send", [False, True])
async def test_required_changed_caller_namespace_replays_bound_attempt(
    tmp_path: Path,
    unknown_send: bool,
) -> None:
    class LoseReceiptOnce(FakeIronRag):
        first = unknown_send

        async def create_revision(self, *args: Any, **kwargs: Any) -> OperationHandle:
            handle = await super().create_revision(*args, **kwargs)
            if self.first:
                self.first = False
                raise httpx.ReadError("accepted payload receipt lost")
            return handle

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = LoseReceiptOnce()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    original = _materialized_page(
        ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED
    )
    adapter = FixedFetchResultAdapter(
        replace(original, item=replace(original.item, idempotency_key="K1"))
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    if unknown_send:
        with pytest.raises(httpx.ReadError):
            await orchestrator.push_ref(ref)
    else:
        assert (await orchestrator.push_ref(ref)).action == "replaced"
    before = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    state.close()
    reopened = _state(tmp_path)
    adapter.result = replace(original, item=replace(original.item, idempotency_key="K2"))
    assert (await _orchestrator(adapter, ironrag, reopened).push_ref(ref)).action == (
        "replaced" if unknown_send else "noop_unchanged"
    )
    after = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert after.receipt == before.receipt
    assert after.payload_key == before.payload_key
    assert after.payload_identity == before.payload_identity
    assert ironrag.revisions[0]["idempotency_key"] == ironrag.revisions[1]["idempotency_key"]
    assert len(ironrag.visibility_intents) == 1
    assert ironrag.deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize("required", [False, True])
@pytest.mark.parametrize(
    "terminal",
    [
        OperationStatusValue.FAILED,
        OperationStatusValue.CANCELED,
        OperationStatusValue.SUPERSEDED,
        OperationStatusValue.READY,
    ],
)
async def test_foreign_operation_disposition_preserves_materialization_attempt(
    tmp_path: Path,
    required: bool,
    terminal: OperationStatusValue,
) -> None:
    class ForeignAcknowledgement(FakeIronRag):
        foreign = True

        async def wait_for_operation(
            self, operation_id: UUID | str, **kwargs: Any
        ) -> OperationStatus:
            if self.foreign:
                status = _ready_operation(_doc_id(9999)).model_copy(update={"status": terminal})
                if terminal is OperationStatusValue.FAILED:
                    raise IronRagOperationFailedError(status)
                return status
            return await super().wait_for_operation(operation_id, **kwargs)

    ref = replace(
        _required_page_ref("v2"),
        materialization_enforcement=SourceMaterializationEnforcement.REQUIRED
        if required
        else SourceMaterializationEnforcement.POLICY,
    )
    state = _state(tmp_path)
    ironrag = ForeignAcknowledgement()
    _seed_existing_page(state, ironrag, ref, change_token="old")
    adapter = FixedFetchResultAdapter(
        _materialized_page(
            ref,
            b"safe",
            enforcement=SourceMaterializationEnforcement.REQUIRED
            if required
            else SourceMaterializationEnforcement.POLICY,
        )
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    with pytest.raises(IronRagError, match="mismatched"):
        await orchestrator.push_ref(ref)
    attempt = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert attempt.phase == "payload_submitting"
    assert attempt.payload_receipt is not None
    assert state.get(ref.kind, ref.item_id).change_token == "old"
    ironrag.foreign = False
    assert (await _orchestrator(adapter, ironrag, state).push_ref(ref)).action == "replaced"
    after = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert after.payload_key == attempt.payload_key
    assert after.payload_receipt == attempt.payload_receipt


@pytest.mark.asyncio
async def test_visibility_contract_source_busy_has_no_fabricated_receipt(tmp_path: Path) -> None:
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    adapter = FixedFetchResultAdapter(SourceFetchDeferred("source busy"))
    outcome = await _orchestrator(adapter, ironrag, state).push_ref(ref)
    assert outcome.deferred
    assert ironrag.visibility_requests == []
    assert ironrag.revisions == []
    assert ironrag.deletes == []
    assert state.materialization_attempts_for_document(str(DOC_PRE)) == []
    assert state.get(ref.kind, ref.item_id).change_token == "v1"


@pytest.mark.asyncio
async def test_visibility_contract_cross_session_route_winner_retains_foreign_payload(
    tmp_path: Path,
) -> None:
    class PausePayload(FakeIronRag):
        entered = asyncio.Event()
        resume = asyncio.Event()
        first = True

        async def wait_for_operation(self, *args: Any, **kwargs: Any) -> OperationStatus:
            if self.first:
                self.first = False
                self.entered.set()
                await self.resume.wait()
            return await super().wait_for_operation(*args, **kwargs)

    state_a = _state(tmp_path)
    state_b = _state(tmp_path)
    ironrag = PausePayload()
    ref_a = _required_page_ref("A")
    _seed_existing_page(state_a, ironrag, ref_a, change_token="initial")
    ref_b = replace(ref_a, change_token="B", routing_facts={"destination": "secondary"})
    adapter_a = FixedFetchResultAdapter(
        _materialized_page(ref_a, b"safe A", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    adapter_b = FixedFetchResultAdapter(
        _materialized_page(ref_b, b"safe B", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    router = _router_with_configured_secondary(LIB, LIB2)
    first = asyncio.create_task(
        _orchestrator(adapter_a, ironrag, state_a, router=router).push_ref(ref_a)
    )
    await ironrag.entered.wait()
    winner = await _orchestrator(adapter_b, ironrag, state_b, router=router).push_ref(ref_b)
    assert winner.action == "created"
    ironrag.resume.set()
    loser = await first
    assert loser.deferred
    assert state_a.get(ref_b.kind, ref_b.item_id).current_library_id == str(LIB2)
    assert state_a.get(ref_b.kind, ref_b.item_id).change_token == "B"
    assert ironrag.deletes == []
    assert ironrag.documents[(LIB, ref_a.external_key)].readable_revision_id is None
    assert state_a.materialization_attempts_for_document(str(DOC_PRE))
    state_a.close()
    state_b.close()


@pytest.mark.asyncio
async def test_visibility_contract_superseded_copy_does_not_starve_later_denial(
    tmp_path: Path,
) -> None:
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    ironrag.revision_conflicts.add(str(DOC_PRE))
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe v2", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    assert (await orchestrator.push_ref(ref)).deferred
    foreign = ironrag.documents[(LIB, ref.external_key)].model_copy(
        update={
            "visibility_intent_id": _doc_id(9001),
            "readable_revision_id": _doc_id(9002),
            "readable_mutation_item_id": _doc_id(9003),
        }
    )
    ironrag.documents[(LIB, ref.external_key)] = foreign
    ironrag.documents[(LIB2, ref.external_key)] = _document(
        DOC_OLD, LIB2, ref.external_key
    ).model_copy(
        update={
            "readable_revision_id": _doc_id(9010),
            "readable_mutation_item_id": _doc_id(9011),
        }
    )
    state.record_document_location(
        kind=ref.kind,
        item_id=ref.item_id,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB2),
    )
    for _ in range(2):
        assert (await orchestrator.push_ref(ref)).deferred
    retired = ironrag.documents[(LIB2, ref.external_key)]
    assert retired.visibility_intent_id is not None, (
        "superseded first copy starved later readable A denial"
    )
    assert retired.readable_revision_id is None
    assert retired.readable_mutation_item_id is None
    assert ironrag.documents[(LIB, ref.external_key)] == foreign
    assert len(ironrag.visibility_intents) == 2
    assert len(state.materialization_attempts_for_document(str(DOC_PRE))) == 1
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    assert ironrag.revisions == []
    assert ironrag.deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "change", ["superseded", "payload_retry", "retirement_superseded", "membership"]
)
async def test_visibility_contract_same_episode_supersession_during_closure_blocks_commit(
    tmp_path: Path, change: str
) -> None:
    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    state.record_document_location(
        kind=ref.kind,
        item_id=ref.item_id,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB2),
    )
    ironrag.documents[(LIB2, ref.external_key)] = _document(DOC_OLD, LIB2, ref.external_key)
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    entered, resume = asyncio.Event(), asyncio.Event()

    class PauseClosure(Orchestrator):
        async def _required_copy_closure(self, *args: Any, **kwargs: Any) -> bool:
            result = await super()._required_copy_closure(*args, **kwargs)
            entered.set()
            await resume.wait()
            return result

    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    orchestrator = PauseClosure(
        adapter=adapter, ironrag=ironrag, router=_router(), state=state, policies=policies
    )
    pending = asyncio.create_task(orchestrator.push_ref(ref))
    await entered.wait()
    second = _state(tmp_path)
    target_id = DOC_OLD if change == "retirement_superseded" else DOC_PRE
    attempt = second.materialization_attempts_for_document(str(target_id))[0]
    if change == "membership":
        second.record_document_location(
            kind=ref.kind,
            item_id=ref.item_id,
            external_key=ref.external_key,
            ironrag_document_id=str(_doc_id(9020)),
            ironrag_library_id=str(LIB3),
        )
        changed = attempt
    else:
        changed = second.transition_materialization_attempt(
            attempt, phase="payload_terminal" if change == "payload_retry" else "intent_superseded"
        )
    second.close()
    resume.set()
    with pytest.raises(
        ValueError, match="membership changed" if change == "membership" else "attempt changed"
    ):
        await pending
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    assert state.materialization_attempts_for_document(str(target_id))[0] == changed
    assert changed.receipt == attempt.receipt
    assert ironrag.deletes == []
    assert len(ironrag.visibility_intents) == 2


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", ["intent", "head"])
async def test_visibility_contract_failed_copy_does_not_starve_later_denial(
    tmp_path: Path, failure: str
) -> None:
    class FailCurrent(FakeIronRag):
        async def create_visibility_intent(
            self, document_id: UUID | str, **kwargs: Any
        ) -> VisibilityIntentReceipt:
            if failure == "intent" and str(document_id) == str(DOC_PRE):
                raise IronRagError("unknown first-copy receipt")
            return await super().create_visibility_intent(document_id, **kwargs)

        async def get_document(self, document_id: UUID | str) -> DocumentResource | None:
            if failure == "head" and str(document_id) == str(DOC_PRE):
                raise IronRagError("unknown first-copy head")
            return await super().get_document(document_id)

    ref = _required_page_ref("v2")
    state = _state(tmp_path)
    ironrag = FailCurrent()
    _seed_existing_page(state, ironrag, ref, change_token="v1")
    state.record_document_location(
        kind=ref.kind,
        item_id=ref.item_id,
        external_key=ref.external_key,
        ironrag_document_id=str(DOC_OLD),
        ironrag_library_id=str(LIB2),
    )
    ironrag.documents[(LIB2, ref.external_key)] = _document(DOC_OLD, LIB2, ref.external_key)
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"safe", enforcement=SourceMaterializationEnforcement.REQUIRED)
    )
    orchestrator = _orchestrator(adapter, ironrag, state)
    with pytest.raises(IronRagError, match="unknown first-copy"):
        await orchestrator.push_ref(ref)
    assert ironrag.documents[(LIB2, ref.external_key)].visibility_intent_id is not None
    assert state.get(ref.kind, ref.item_id).change_token == "v1"
    assert ironrag.revisions == []
    assert ironrag.deletes == []


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "field,value",
    [
        ("title", "other title"),
        ("document_hint", "other hint"),
        ("file_name", "other.txt"),
        ("mime_type", "text/plain"),
        ("idempotency_key", "logical-key"),
    ],
)
async def test_policy_complete_body_change_gets_durable_key(
    tmp_path: Path, field: str, value: str
) -> None:
    from dataclasses import replace

    ref = _page_ref("A")
    state, server = _state(tmp_path), FakeIronRag()
    _seed_existing_page(state, server, ref, change_token="initial")
    first = _materialized_page(
        ref, b"same bytes", enforcement=SourceMaterializationEnforcement.POLICY
    )
    first = replace(first, item=replace(first.item, idempotency_key="caller-logical"))
    adapter = FixedFetchResultAdapter(first)
    orchestrator = _orchestrator(adapter, server, state)
    assert (await orchestrator.push_ref(ref)).action == "replaced"
    key = server.revisions[-1]["idempotency_key"]
    changed_ref = _page_ref("B")
    adapter.result = replace(first, item=replace(first.item, ref=changed_ref, **{field: value}))
    orchestrator.reset_sweep_cache()
    assert (await orchestrator.push_ref(changed_ref)).action == "replaced"
    changed_key = server.revisions[-1]["idempotency_key"]
    assert changed_key != key
    orchestrator.reset_sweep_cache()
    assert (await orchestrator.push_ref(changed_ref)).action == "noop_unchanged"
    assert server.revisions[-1]["idempotency_key"] == changed_key
    attempts = state.materialization_attempts_for_document(str(DOC_PRE))
    assert len(attempts) == 2
    assert all(
        not attempt.intent_required
        and attempt.request is None
        and attempt.receipt is None
        and attempt.payload_identity is not None
        for attempt in attempts
    )
    assert not server.visibility_requests
    assert not server.deletes


@pytest.mark.asyncio
async def test_policy_unknown_send_reopen_replays_exact_body_and_key(tmp_path: Path) -> None:
    class UnknownSend(FakeIronRag):
        unknown = True

        async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> OperationHandle:
            handle = await super().create_revision(document_id, **kwargs)
            if self.unknown:
                self.unknown = False
                raise httpx.ReadTimeout("receipt lost after admission")
            return handle

    ref = _page_ref("B")
    state, server = _state(tmp_path), UnknownSend()
    _seed_existing_page(state, server, ref, change_token="A")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"exact", enforcement=SourceMaterializationEnforcement.POLICY)
    )
    orchestrator = _orchestrator(adapter, server, state)
    with pytest.raises(httpx.ReadTimeout):
        await orchestrator.push_ref(ref)
    attempt = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert attempt.payload_receipt is None
    assert state.get(ref.kind, ref.item_id).change_token == "A"
    body = server._payload_bodies[attempt.payload_key]
    state.close()
    reopened = _state(tmp_path)
    assert (await _orchestrator(adapter, server, reopened).push_ref(ref)).action == "replaced"
    assert server.revisions[-1]["idempotency_key"] == attempt.payload_key
    assert server._payload_bodies[attempt.payload_key] == body
    assert len(server._payload_handles) == 1
    assert reopened.get(ref.kind, ref.item_id).change_token == "B"
    assert not server.visibility_requests


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "field,value",
    [
        ("title", "changed"),
        ("mime_type", "text/plain"),
        ("file_name", "changed.txt"),
        ("file_bytes", b"changed"),
        ("document_hint", "hint"),
        ("provenance", RevisionProvenance(language_code="ru")),
        ("provenance", RevisionProvenance(source_uri="https://source.invalid/page")),
        ("mode", "append"),
        ("appended_text", "appended"),
        ("markdown", "markdown"),
        ("base_revision_id", DOC_OLD),
        ("visibility_intent_id", DOC_OLD),
    ],
)
async def test_fake_revision_rejects_changed_complete_body(field: str, value: Any) -> None:
    server = FakeIronRag()
    server.documents[(LIB, "test")] = _document(DOC_PRE, LIB, "test")
    body: dict[str, Any] = {
        "mode": "replace",
        "file_bytes": b"original",
        "idempotency_key": "fixed",
    }
    handle = await server.create_revision(DOC_PRE, **body)
    assert await server.create_revision(DOC_PRE, **body) == handle
    with pytest.raises(IronRagConflictError) as caught:
        await server.create_revision(DOC_PRE, **{**body, field: value})
    assert caught.value.problem.code == "idempotency_conflict"


@pytest.mark.asyncio
@pytest.mark.parametrize("terminal", [OperationStatusValue.FAILED, OperationStatusValue.CANCELED])
async def test_policy_terminal_retry_reopen_rolls_only_payload_key(
    tmp_path: Path, terminal: OperationStatusValue
) -> None:
    class TerminalOnce(FakeIronRag):
        first = True

        async def wait_for_operation(
            self, operation_id: UUID | str, **kwargs: Any
        ) -> OperationStatus:
            if self.first:
                self.first = False
                status = self._operations[UUID(str(operation_id))].model_copy(
                    update={"status": terminal}
                )
                self._operations[status.id] = status
                if terminal is OperationStatusValue.FAILED:
                    raise IronRagOperationFailedError(status)
                return status
            return await super().wait_for_operation(operation_id, **kwargs)

    ref, state, server = _page_ref("B"), _state(tmp_path), TerminalOnce()
    _seed_existing_page(state, server, ref, change_token="A")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"exact", enforcement=SourceMaterializationEnforcement.POLICY)
    )
    if terminal is OperationStatusValue.FAILED:
        orchestrator = _orchestrator(adapter, server, state)
        with pytest.raises(IronRagOperationFailedError):
            await orchestrator.push_ref(ref)
    else:
        assert (await _orchestrator(adapter, server, state).push_ref(ref)).deferred
    before = state.materialization_attempts_for_document(str(DOC_PRE))[0]
    first_key = server.revisions[0]["idempotency_key"]
    assert before.phase == "payload_terminal"
    assert before.payload_key != first_key
    assert before.payload_receipt is None
    state.close()
    reopened = _state(tmp_path)
    assert (await _orchestrator(adapter, server, reopened).push_ref(ref)).action == "replaced"
    after = reopened.materialization_attempts_for_document(str(DOC_PRE))[0]
    assert after.payload_key == before.payload_key
    assert after.payload_identity == before.payload_identity
    assert server._payload_bodies[first_key] == server._payload_bodies[after.payload_key]
    assert after.episode_id == before.episode_id
    assert not server.visibility_requests
    assert not server.deletes


@pytest.mark.asyncio
async def test_fake_revision_replay_key_is_bound_to_target() -> None:
    server = FakeIronRag()
    server.documents[(LIB, "first")] = _document(DOC_PRE, LIB, "first")
    server.documents[(LIB, "second")] = _document(DOC_OLD, LIB, "second")
    body = {"mode": "replace", "file_bytes": b"same", "idempotency_key": "fixed"}
    await server.create_revision(DOC_PRE, **body)
    with pytest.raises(IronRagConflictError) as caught:
        await server.create_revision(DOC_OLD, **body)
    assert caught.value.problem.code == "idempotency_conflict"


@pytest.mark.asyncio
async def test_policy_typed_idempotency_conflict_surfaces_not_busy(tmp_path: Path) -> None:
    class Conflicted(FakeIronRag):
        async def create_revision(self, *args: Any, **kwargs: Any) -> OperationHandle:
            raise IronRagConflictError(
                ProblemDetails(
                    type="urn:ironrag:error:idempotency_conflict",
                    title="Idempotency conflict",
                    status=409,
                    code="idempotency_conflict",
                    detail="synthetic complete identity conflict",
                )
            )

    ref, state, server = _page_ref("B"), _state(tmp_path), Conflicted()
    _seed_existing_page(state, server, ref, change_token="A")
    adapter = FixedFetchResultAdapter(
        _materialized_page(ref, b"same", enforcement=SourceMaterializationEnforcement.POLICY)
    )
    orchestrator = _orchestrator(adapter, server, state)
    with pytest.raises(IronRagConflictError) as caught:
        await orchestrator.push_ref(ref)
    assert caught.value.problem.code == "idempotency_conflict"
    assert state.get(ref.kind, ref.item_id).change_token == "A"
    assert state.materialization_attempts_for_document(str(DOC_PRE))[0].payload_receipt is None


class RefDrivenFetchAdapter(EchoAdapter):
    """Materialize the supplied snapshot, except for explicit delete tokens."""

    def __init__(self, *, delete_tokens: frozenset[str] = frozenset()) -> None:
        super().__init__({})
        self.delete_tokens = delete_tokens

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        if ref.change_token in self.delete_tokens:
            return SourceFetchDelete(
                SourceDeleteEnforcement.POLICY,
                "synthetic post-enumeration disappearance",
                cacheable=False,
            )
        return _materialized_page(
            ref,
            f"payload {ref.change_token}".encode(),
            enforcement=ref.materialization_enforcement,
        )


class PauseFirstExactFindIronRag(FakeIronRag):
    """Pause one exact lookup so a competing same-identity push can start."""

    def __init__(self) -> None:
        super().__init__()
        self.find_entered = asyncio.Event()
        self.release_find = asyncio.Event()
        self._pause_next_find = True

    async def find_document(
        self,
        library_id: UUID,
        external_key: str,
    ) -> DocumentResource | None:
        if self._pause_next_find:
            self._pause_next_find = False
            self.find_entered.set()
            await self.release_find.wait()
        return await super().find_document(library_id, external_key)


class PauseFirstRevisionIronRag(FakeIronRag):
    """Pause one revision submission to exercise the public push-item lock."""

    def __init__(self) -> None:
        super().__init__()
        self.revision_entered = asyncio.Event()
        self.release_revision = asyncio.Event()
        self._pause_next_revision = True

    async def create_revision(
        self,
        document_id: UUID | str,
        **kwargs: Any,
    ) -> OperationHandle:
        if self._pause_next_revision:
            self._pause_next_revision = False
            self.revision_entered.set()
            await self.release_revision.wait()
        return await super().create_revision(document_id, **kwargs)


@pytest.mark.asyncio
async def test_same_sweep_push_memo_is_bound_to_source_token(tmp_path: Path) -> None:
    adapter = RefDrivenFetchAdapter()
    state = _state(tmp_path)
    ironrag = PayloadTrackingIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)
    first_ref = _page_ref("v1")
    second_ref = _page_ref("v2")

    assert (await orchestrator.push_ref(first_ref)).action == "created"
    second = await orchestrator.push_ref(second_ref)

    assert second.action == "replaced"
    cursor = state.get(second_ref.kind, second_ref.item_id)
    assert cursor is not None
    assert cursor.change_token == "v2"
    assert ironrag.payloads[(LIB, second_ref.external_key)] == b"payload v2"


@pytest.mark.asyncio
async def test_public_push_item_serializes_same_identity_mutations(tmp_path: Path) -> None:
    adapter = EchoAdapter({})
    state = _state(tmp_path)
    ironrag = PauseFirstRevisionIronRag()
    old_ref = _page_ref("v1")
    _seed_existing_page(state, ironrag, old_ref, change_token="v1")
    orchestrator = _orchestrator(adapter, ironrag, state)
    route = _router().resolve(old_ref)
    first = SourceItem(
        ref=_page_ref("v2"),
        payload=b"v2",
        mime_type="text/markdown",
        file_name="1.md",
    )
    second = SourceItem(
        ref=_page_ref("v3"),
        payload=b"v3",
        mime_type="text/markdown",
        file_name="1.md",
    )

    first_task = asyncio.create_task(orchestrator.push_item(first, route, PushPolicy()))
    await ironrag.revision_entered.wait()
    second_task = asyncio.create_task(orchestrator.push_item(second, route, PushPolicy()))
    await asyncio.sleep(0)
    assert not second_task.done(), "public push_item must share the identity lock"

    ironrag.release_revision.set()
    first_outcome, second_outcome = await asyncio.gather(first_task, second_task)

    assert first_outcome.action == "replaced"
    assert second_outcome.action == "replaced"
    cursor = state.get(second.ref.kind, second.ref.item_id)
    assert cursor is not None
    assert cursor.change_token == "v3"
    assert len(ironrag.revisions) == 2


@pytest.mark.asyncio
async def test_required_reconciliation_serializes_newer_same_identity_push(
    tmp_path: Path,
) -> None:
    """A paused old-route proof cannot erase or roll back a newer route push."""
    state = _state(tmp_path)
    ironrag = PauseFirstExactFindIronRag()
    old_ref = _required_page_ref("v1")
    _seed_existing_page(state, ironrag, old_ref, change_token="v1")
    adapter = RefDrivenFetchAdapter()
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        router=_router_with_configured_secondary(LIB, LIB2),
    )
    first_ref = _required_page_ref("v2")
    second_ref = SourceItemRef(
        item_id="1",
        kind="page",
        external_key="echo:page:1",
        change_token="v3",
        routing_facts={"destination": "secondary"},
        materialization_enforcement=SourceMaterializationEnforcement.REQUIRED,
    )

    first_task = asyncio.create_task(orchestrator.push_ref(first_ref))
    await ironrag.find_entered.wait()
    second_task = asyncio.create_task(orchestrator.push_ref(second_ref))
    await asyncio.sleep(0)
    assert not second_task.done(), "same-identity push must wait for reconciliation"

    ironrag.release_find.set()
    first, second = await asyncio.gather(first_task, second_task)

    assert first.action == "replaced"
    assert second.action == "created"
    cursor = state.get(second_ref.kind, second_ref.item_id)
    assert cursor is not None
    assert cursor.change_token == "v3"
    assert cursor.current_library_id == str(LIB2)
    assert set(ironrag.documents) == {
        (LIB, second_ref.external_key),
        (LIB2, second_ref.external_key),
    }
    assert ironrag.documents[(LIB, second_ref.external_key)].readable_revision_id is None
    assert ironrag.deletes == []


@pytest.mark.asyncio
async def test_fetch_delete_serializes_newer_same_identity_push(tmp_path: Path) -> None:
    """Delete-first ordering lets a newer push recreate; push-first defers delete."""
    state = _state(tmp_path)
    ironrag = PauseFirstExactFindIronRag()
    old_ref = _page_ref("v1")
    _seed_existing_page(state, ironrag, old_ref, change_token="v1")
    adapter = RefDrivenFetchAdapter(delete_tokens=frozenset({"v2-delete"}))
    orchestrator = _orchestrator(
        adapter,
        ironrag,
        state,
        policy=PushPolicy(on_missing=DeleteAction.DELETE),
    )
    delete_ref = _page_ref("v2-delete")
    recreated_ref = _page_ref("v3")

    delete_task = asyncio.create_task(orchestrator.push_ref(delete_ref))
    await ironrag.find_entered.wait()
    recreate_task = asyncio.create_task(orchestrator.push_ref(recreated_ref))
    await asyncio.sleep(0)
    assert not recreate_task.done(), "same-identity push must wait for fetch-delete"

    ironrag.release_find.set()
    deleted, recreated = await asyncio.gather(delete_task, recreate_task)

    assert deleted.action == "deleted"
    assert recreated.action == "created"
    cursor = state.get(recreated_ref.kind, recreated_ref.item_id)
    assert cursor is not None
    assert cursor.change_token == "v3"
    assert set(ironrag.documents) == {(LIB, recreated_ref.external_key)}


# --- durable dependent lifecycle: census, claims, release, cascade ---

DOC_STALE_IMAGE = _doc_id(4)
DOC_ATTACHMENT_PRE = _doc_id(5)


class PolicyFailureIronRag(FakeIronRag):
    """Stateful terminal revisions; failed records remain failed across retries."""

    def __init__(self, *failed_keys: str) -> None:
        super().__init__()
        self.failed_keys = set(failed_keys)
        self.failures: dict[UUID, IronRagOperationFailedError] = {}

    async def create_revision(self, document_id: UUID | str, **kwargs: Any) -> OperationHandle:
        handle = await super().create_revision(document_id, **kwargs)
        document = await self.get_document(document_id)
        assert document is not None
        if document.external_key in self.failed_keys:
            status = self._operations[handle.operation_id].model_copy(
                update={
                    "status": OperationStatusValue.FAILED,
                    "failure_disposition": "retry_exhausted",
                }
            )
            self._operations[status.id] = status
            self.failures[status.id] = IronRagOperationFailedError(status)
        return handle

    async def wait_for_operation(self, operation_id: UUID | str, **kwargs: Any) -> OperationStatus:
        failure = self.failures.get(UUID(str(operation_id)))
        if failure is not None:
            raise failure
        return await super().wait_for_operation(operation_id, **kwargs)


@pytest.mark.asyncio
@pytest.mark.parametrize("with_dependent", [False, True])
async def test_failed_policy_primary_receipt_settles_only_real_dependents(
    tmp_path: Path, with_dependent: bool
) -> None:
    specs = (DependentSpec("image", "live", "i1"),) if with_dependent else ()
    adapter = _dependent_adapter("t2", *specs)
    state = _state(tmp_path)
    server = PolicyFailureIronRag("echo:page:1")
    ref = await first_ref(adapter)
    seed_document(state, server.documents, ref, change_token="t1", document_id=DOC_PRE)
    before = state.get("page", "1")

    with pytest.raises(OrchestrationFailedError) as caught:
        await _orchestrator(adapter, server, state).push_ref(ref)

    receipt = caught.value
    assert receipt.primary_error is server.failures[receipt.status.id]
    assert receipt.__cause__ is receipt.primary_error
    assert receipt.dependent_error is None
    assert [out.ref.item_id for out in receipt.dependent_outcomes] == (["live"] if specs else [])
    census = state.parent_census("page", "1")
    assert census is not None
    assert (census.change_token, census.dependents_token) == ("t2", ref.dependents_token)
    assert state.get("page", "1") == before
    assert all(
        error.status.status is OperationStatusValue.FAILED for error in server.failures.values()
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("dependent_failure", ["error", "defer"])
async def test_failed_primary_preserves_completed_dependents_without_inventing_census(
    tmp_path: Path, dependent_failure: str
) -> None:
    adapter = _dependent_adapter(
        "t2", DependentSpec("image", "first", "i1"), DependentSpec("image", "later", "i2")
    )
    server = PolicyFailureIronRag("echo:page:1")
    state = _state(tmp_path)
    ref = await first_ref(adapter)
    seed_document(state, server.documents, ref, change_token="t1", document_id=DOC_PRE)
    seed_document(
        state,
        server.documents,
        dependent_ref("image", "later", "i1"),
        change_token="i1",
        document_id=DOC_IMAGE_PRE,
    )
    if dependent_failure == "error":
        server.failed_keys.add("echo:image:later")
    else:
        server.revision_conflicts.add(str(DOC_IMAGE_PRE))

    with pytest.raises(OrchestrationFailedError) as caught:
        await _orchestrator(adapter, server, state).push_ref(ref)

    receipt = caught.value
    assert receipt.dependent_outcomes[0].action == "created"
    assert receipt.primary_error is server.failures[receipt.status.id]
    assert (receipt.dependent_error is not None) == (dependent_failure == "error")
    if dependent_failure == "defer":
        assert receipt.dependent_outcomes[-1].deferred is True
    assert state.parent_census("page", "1") is None
    assert state.has_claims("image", "first", str(LIB))
    assert state.has_claims("image", "later", str(LIB))


@pytest.mark.asyncio
@pytest.mark.parametrize("failure", ["transport", "wrong_operation", "nonterminal"])
async def test_unvalidated_primary_failure_does_not_settle_dependents(
    tmp_path: Path, failure: str
) -> None:
    class UnprovenFailure(PolicyFailureIronRag):
        async def wait_for_operation(
            self, operation_id: UUID | str, **kwargs: Any
        ) -> OperationStatus:
            if failure == "transport":
                raise IronRagError("unavailable")
            status = self._operations[UUID(str(operation_id))]
            status = status.model_copy(
                update={"id": UUID(int=99999)}
                if failure == "wrong_operation"
                else {"status": OperationStatusValue.PROCESSING}
            )
            raise IronRagOperationFailedError(status)

    adapter = _dependent_adapter("t2", DependentSpec("image", "live", "i1"))
    state = _state(tmp_path)
    server = UnprovenFailure("echo:page:1")
    ref = await first_ref(adapter)
    seed_document(state, server.documents, ref, change_token="t1", document_id=DOC_PRE)

    with pytest.raises(IronRagError) as caught:
        await _orchestrator(adapter, server, state).push_ref(ref)

    assert not isinstance(caught.value, OrchestrationFailedError)
    assert state.parent_census("page", "1") is None
    assert state.claims_of_parent("page", "1") == []
    assert server.creates == []


def _dependent_adapter(
    page_token: str,
    *specs: DependentSpec,
    bounded: bool = True,
) -> DependentPageAdapter:
    return DependentPageAdapter({"1": echo_page("1", page_token)}, {"1": specs}, bounded=bounded)


def _claim(state: StateStore, ref: SourceItemRef, library_id: UUID = LIB) -> None:
    state.claim_member(
        parent_kind="page",
        parent_item_id="1",
        member=MemberTarget(
            kind=ref.kind,
            item_id=ref.item_id,
            external_key=ref.external_key,
            ironrag_library_id=str(library_id),
        ),
    )


@pytest.mark.asyncio
async def test_omitted_dependent_is_released_then_deleted_by_complete_sweep(tmp_path: Path) -> None:
    """Release claims on fetch; only the sweep can prove physical orphanhood."""
    adapter = _dependent_adapter("t2", DependentSpec("image", "img-1", "i1"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = await first_ref(adapter)
    seed_document(state, ironrag.documents, ref, change_token="t1", document_id=DOC_PRE)
    stale = dependent_ref("image", "img-old", "i0")
    seed_document(state, ironrag.documents, stale, change_token="i0", document_id=DOC_STALE_IMAGE)
    _claim(state, stale)
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(ref)

    assert out.action == "replaced"
    assert (LIB, "echo:image:img-old") in ironrag.documents
    assert ironrag.deletes == []
    assert (LIB, "echo:image:img-1") in ironrag.documents
    assert state.has_claims("image", "img-old", str(LIB)) is False
    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert state.get("image", "img-old") is not None
    assert state.locations_for("image", "img-old")
    census = state.parent_census("page", "1")
    assert census is not None
    assert (census.change_token, census.dependents_token) == ("t2", ref.dependents_token)
    assert [(d.ref.item_id, d.action) for d in out.dependent_outcomes] == [
        ("img-1", "created"),
    ]
    await _sweep_released_dependents(adapter, ironrag, state)
    assert (LIB, "echo:image:img-old") not in ironrag.documents
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]
    assert state.get("image", "img-old") is None
    assert state.locations_for("image", "img-old") == []


@pytest.mark.asyncio
async def test_omitted_dependent_survives_while_another_parent_claims_it(tmp_path: Path) -> None:
    adapter = DependentPageAdapter(
        {"1": echo_page("1", "t2"), "2": echo_page("2", "t1")},
        {"1": (), "2": (DependentSpec("image", "shared", "i1"),)},
    )
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    refs = [ref async for ref in adapter.iter_items()]
    seed_document(state, ironrag.documents, refs[0], change_token="t1", document_id=DOC_PRE)
    shared = dependent_ref("image", "shared", "i1")
    seed_document(state, ironrag.documents, shared, change_token="i1", document_id=DOC_IMAGE_PRE)
    _claim(state, shared)
    state.claim_member(
        parent_kind="page",
        parent_item_id="2",
        member=MemberTarget(
            kind="image",
            item_id="shared",
            external_key=shared.external_key,
            ironrag_library_id=str(LIB),
        ),
    )
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(refs[0])

    assert out.action == "replaced"
    assert ironrag.deletes == []
    assert (LIB, "echo:image:shared") in ironrag.documents
    assert state.claims_of_parent("page", "1") == []
    assert state.has_claims("image", "shared", str(LIB)) is True
    assert out.dependent_outcomes == ()


@pytest.mark.asyncio
async def test_deferred_parent_fetch_keeps_last_proven_dependent_set(tmp_path: Path) -> None:
    adapter = _dependent_adapter("t2")
    adapter.deferred_pages = {"1"}
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = await first_ref(adapter)
    seed_document(state, ironrag.documents, ref, change_token="t1", document_id=DOC_PRE)
    stale = dependent_ref("image", "img-old", "i0")
    seed_document(state, ironrag.documents, stale, change_token="i0", document_id=DOC_STALE_IMAGE)
    _claim(state, stale)
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(ref)

    assert out.action == "fetch_deferred"
    assert ironrag.deletes == []
    assert state.has_claims("image", "img-old", str(LIB)) is True
    assert state.parent_census("page", "1") is None
    cursor = state.get("page", "1")
    assert cursor is not None
    assert cursor.change_token == "t1"


@pytest.mark.asyncio
async def test_deferred_dependent_push_records_no_census_and_releases_nothing(
    tmp_path: Path,
) -> None:
    adapter = _dependent_adapter("t2", DependentSpec("image", "img-1", "i1"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ironrag.revision_conflicts = {str(DOC_IMAGE_PRE)}
    ref = await first_ref(adapter)
    seed_document(state, ironrag.documents, ref, change_token="t1", document_id=DOC_PRE)
    current = dependent_ref("image", "img-1", "i0")
    seed_document(state, ironrag.documents, current, change_token="i0", document_id=DOC_IMAGE_PRE)
    stale = dependent_ref("image", "img-old", "i0")
    seed_document(state, ironrag.documents, stale, change_token="i0", document_id=DOC_STALE_IMAGE)
    _claim(state, stale)
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(ref)

    assert out.deferred is True
    assert out.dependent_outcomes[0].action == "skipped_changed"
    assert ironrag.deletes == []
    assert state.parent_census("page", "1") is None
    assert state.has_claims("image", "img-old", str(LIB)) is True
    assert state.has_claims("image", "img-1", str(LIB)) is True, "claim precedes the push"
    cursor = state.get("page", "1")
    assert cursor is not None
    assert cursor.change_token == "t1"


async def _sweep_released_dependents(
    adapter: DependentPageAdapter,
    ironrag: FakeIronRag,
    state: StateStore,
    *,
    policies: PolicyOverrides | None = None,
    router: Router | None = None,
) -> None:
    """Exercise released-copy cleanup through the real completeness-gated entrypoint."""
    resolved_policies = policies or _policies()
    resolved_router = router or _router()
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type] -- stateful client double
        router=resolved_router,
        state=state,
        policies=resolved_policies,
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type] -- stateful client double
        orchestrator=orchestrator,
        router=resolved_router,
        state=state,
        policies=resolved_policies,
        concurrency=1,
        interval_seconds=60,
    )
    report = await manager.run_once(reason="released-copy-cleanup")
    assert report.dependent_census_complete is True


def _seed_proven_parent_with_two_dependents(
    state: StateStore, ironrag: FakeIronRag, ref: SourceItemRef
) -> tuple[SourceItemRef, SourceItemRef]:
    seed_document(state, ironrag.documents, ref, change_token="t1", document_id=DOC_PRE)
    attachment = dependent_ref("attachment", "att-1", "a1")
    seed_document(
        state, ironrag.documents, attachment, change_token="a1", document_id=DOC_ATTACHMENT_PRE
    )
    image = dependent_ref("image", "img-1", "i1")
    seed_document(
        state,
        ironrag.documents,
        image,
        change_token="i1",
        document_id=DOC_IMAGE_PRE,
        library_id=LIB2,
    )
    state.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t1",
        dependents_token="d1",
        members=(
            MemberTarget("attachment", "att-1", attachment.external_key, str(LIB)),
            MemberTarget("image", "img-1", image.external_key, str(LIB2)),
        ),
    )
    return attachment, image


@pytest.mark.asyncio
async def test_parent_delete_releases_membership_before_gated_dependent_cleanup(
    tmp_path: Path,
) -> None:
    adapter = DependentPageAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = _page_ref("t1")
    _seed_proven_parent_with_two_dependents(state, ironrag, ref)
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(ref)

    assert out.action == "deleted"
    assert ironrag.deletes == [str(DOC_PRE)]
    assert (LIB, "echo:attachment:att-1") in ironrag.documents
    assert (LIB2, "echo:image:img-1") in ironrag.documents
    assert state.parent_census("page", "1") is None
    assert state.claims_of_parent("page", "1") == []
    assert state.locations_for("attachment", "att-1")
    assert state.locations_for("image", "img-1")
    assert out.dependent_outcomes == ()
    await _sweep_released_dependents(adapter, ironrag, state)
    assert ironrag.documents == {}
    assert state.locations_for("attachment", "att-1") == []
    assert state.locations_for("image", "img-1") == []


@pytest.mark.asyncio
async def test_delete_by_ref_cascades_like_a_fetch_delete(tmp_path: Path) -> None:
    adapter = DependentPageAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = _page_ref("t1")
    _seed_proven_parent_with_two_dependents(state, ironrag, ref)
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.delete_by_ref(ref)

    assert out.action == "deleted"
    assert ironrag.deletes == [str(DOC_PRE)]
    assert (LIB, "echo:attachment:att-1") in ironrag.documents
    assert (LIB2, "echo:image:img-1") in ironrag.documents
    assert state.claims_of_parent("page", "1") == []
    assert out.dependent_outcomes == ()
    await _sweep_released_dependents(adapter, ironrag, state)
    assert ironrag.documents == {}


@pytest.mark.asyncio
async def test_parent_retained_by_ignore_policy_keeps_dependents_and_membership(
    tmp_path: Path,
) -> None:
    adapter = DependentPageAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = _page_ref("t1")
    _seed_proven_parent_with_two_dependents(state, ironrag, ref)
    orchestrator = _orchestrator(
        adapter, ironrag, state, policy=PushPolicy(on_missing=DeleteAction.IGNORE)
    )

    out = await orchestrator.push_ref(ref)

    assert out.action == "skipped_missing"
    assert ironrag.deletes == []
    assert state.parent_census("page", "1") is not None
    assert len(state.claims_of_parent("page", "1")) == 2
    assert out.dependent_outcomes == ()


@pytest.mark.asyncio
async def test_parent_delete_cascade_honours_dependent_kind_on_missing(tmp_path: Path) -> None:
    adapter = DependentPageAdapter({})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = _page_ref("t1")
    _seed_proven_parent_with_two_dependents(state, ironrag, ref)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=_router(),
        state=state,
        policies=PolicyOverrides(
            default=PushPolicy(),
            by_kind={"image": PushPolicy(on_missing=DeleteAction.IGNORE)},
        ),
    )

    out = await orchestrator.push_ref(ref)

    assert out.action == "deleted"
    assert (LIB2, "echo:image:img-1") in ironrag.documents, "image kind retains on missing"
    assert (LIB, "echo:attachment:att-1") in ironrag.documents
    assert state.claims_of_parent("page", "1") == [], "membership is withdrawn with the parent"
    assert out.dependent_outcomes == ()
    await _sweep_released_dependents(
        adapter,
        ironrag,
        state,
        policies=PolicyOverrides(
            default=PushPolicy(), by_kind={"image": PushPolicy(on_missing=DeleteAction.IGNORE)}
        ),
    )
    assert (LIB2, "echo:image:img-1") in ironrag.documents
    assert (LIB, "echo:attachment:att-1") not in ironrag.documents


@pytest.mark.asyncio
async def test_unchanged_parent_without_census_is_proven_by_refetch_without_revisions(
    tmp_path: Path,
) -> None:
    """After an upgrade the parent token is unchanged but its dependent set is
    unproven: the parent is fetched once, nothing is revised, the census and
    claims are recorded, and the next sweep short-circuits without a fetch."""
    adapter = _dependent_adapter("t1", DependentSpec("image", "img-1", "i1"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = await first_ref(adapter)
    seed_document(state, ironrag.documents, ref, change_token="t1", document_id=DOC_PRE)
    image = dependent_ref("image", "img-1", "i1")
    seed_document(state, ironrag.documents, image, change_token="i1", document_id=DOC_IMAGE_PRE)
    orchestrator = _orchestrator(adapter, ironrag, state)

    proving = await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == ["1"]
    assert proving.action == "noop_unchanged"
    assert [d.action for d in proving.dependent_outcomes] == ["noop_unchanged"]
    assert ironrag.revisions == []
    assert ironrag.creates == []
    assert state.has_claims("image", "img-1", str(LIB)) is True
    census = state.parent_census("page", "1")
    assert census is not None
    assert census.dependents_token == ref.dependents_token

    orchestrator.reset_sweep_cache()
    proven = await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == ["1"], "a proven parent is not fetched again"
    assert proven.action == "noop_unchanged"


@pytest.mark.asyncio
async def test_changed_dependents_token_refetches_parent_without_page_revision(
    tmp_path: Path,
) -> None:
    adapter = _dependent_adapter("t1", DependentSpec("image", "img-1", "i1"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)
    first = await orchestrator.push_ref(await first_ref(adapter))
    assert first.action == "created"
    adapter.set_dependents(
        "1", DependentSpec("image", "img-1", "i1"), DependentSpec("attachment", "att-2", "a1")
    )
    orchestrator.reset_sweep_cache()

    ref = await first_ref(adapter)
    out = await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == ["1", "1"]
    assert out.action == "noop_unchanged"
    assert ironrag.revisions == []
    assert (LIB, "echo:attachment:att-2") in ironrag.documents
    assert state.has_claims("attachment", "att-2", str(LIB)) is True
    census = state.parent_census("page", "1")
    assert census is not None
    assert census.dependents_token == ref.dependents_token


@pytest.mark.asyncio
async def test_unbounded_dependents_token_never_proves_the_parent(tmp_path: Path) -> None:
    adapter = _dependent_adapter("t1", DependentSpec("image", "img-1", "i1"), bounded=False)
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)
    ref = await first_ref(adapter)
    assert ref.dependents_token is None

    await orchestrator.push_ref(ref)
    orchestrator.reset_sweep_cache()
    second = await orchestrator.push_ref(ref)

    assert adapter.fetch_calls == ["1", "1"]
    assert second.action == "noop_unchanged"
    assert ironrag.revisions == []
    census = state.parent_census("page", "1")
    assert census is not None
    assert census.dependents_token is None


@pytest.mark.asyncio
async def test_member_claim_is_durable_before_the_dependent_push(tmp_path: Path) -> None:
    class FailingImageCreate(FakeIronRag):
        async def create_document(self, library_id: UUID, **kwargs: Any) -> DocumentResource:
            if kwargs["external_key"] == "echo:image:img-1":
                raise IronRagError("synthetic image create failure")
            return await super().create_document(library_id, **kwargs)

    adapter = _dependent_adapter("t1", DependentSpec("image", "img-1", "i1"))
    state = _state(tmp_path)
    ironrag = FailingImageCreate()
    orchestrator = _orchestrator(adapter, ironrag, state)

    ref = await first_ref(adapter)
    with pytest.raises(IronRagError, match="synthetic image create failure"):
        await orchestrator.push_ref(ref)

    assert state.has_claims("image", "img-1", str(LIB)) is True
    assert state.parent_census("page", "1") is None


@pytest.mark.asyncio
async def test_a_failed_release_deletion_keeps_the_parent_outcome_and_its_census(
    tmp_path: Path,
) -> None:
    """Deletion failure belongs to the gated reaper, not parent settlement."""

    class FailingDelete(FakeIronRag):
        async def delete_document(
            self, document_id: UUID | str, *, idempotency_key: str
        ) -> OperationHandle | None:
            self.deletes.append(str(document_id))
            raise IronRagError("synthetic delete failure")

    adapter = _dependent_adapter("t2")
    state = _state(tmp_path)
    ironrag = FailingDelete()
    ref = await first_ref(adapter)
    seed_document(state, ironrag.documents, ref, change_token="t1", document_id=DOC_PRE)
    stale = dependent_ref("image", "img-old", "i0")
    seed_document(state, ironrag.documents, stale, change_token="i0", document_id=DOC_STALE_IMAGE)
    _claim(state, stale)
    orchestrator = _orchestrator(adapter, ironrag, state)

    out = await orchestrator.push_ref(ref)

    assert out.action == "replaced"
    assert out.dependent_outcomes == ()
    assert ironrag.deletes == []
    assert (LIB, "echo:image:img-old") in ironrag.documents
    assert state.has_claims("image", "img-old", str(LIB)) is False
    census = state.parent_census("page", "1")
    assert census is not None
    assert census.change_token == "t2"
    await _sweep_released_dependents(adapter, ironrag, state)
    assert ironrag.deletes == [str(DOC_STALE_IMAGE)]
    assert (LIB, "echo:image:img-old") in ironrag.documents
    assert state.parent_census("page", "1") == census


@pytest.mark.asyncio
async def test_route_move_releases_old_library_copy_of_dependent(tmp_path: Path) -> None:
    adapter = _dependent_adapter("t1", DependentSpec("image", "img-1", "i1"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    ref = await first_ref(adapter)
    before = await _orchestrator(adapter, ironrag, state).push_ref(ref)
    assert before.action == "created"
    assert state.has_claims("image", "img-1", str(LIB)) is True

    moved = _orchestrator(adapter, ironrag, state, router=_router_to(LIB2))
    out = await moved.push_ref(ref)

    assert out.action == "created"
    assert (LIB2, "echo:page:1") in ironrag.documents
    assert (LIB2, "echo:image:img-1") in ironrag.documents
    assert (LIB, "echo:image:img-1") in ironrag.documents, "release alone cannot prove orphanhood"
    assert (LIB, "echo:page:1") in ironrag.documents, "the primary copy is the reaper's"
    assert state.has_claims("image", "img-1", str(LIB)) is False
    assert state.has_claims("image", "img-1", str(LIB2)) is True
    assert [(d.ref.item_id, d.action, d.library_id) for d in out.dependent_outcomes] == [
        ("img-1", "created", LIB2),
    ]
    await _sweep_released_dependents(adapter, ironrag, state, router=_router_to(LIB2))
    assert (LIB, "echo:image:img-1") not in ironrag.documents
    assert (LIB2, "echo:image:img-1") in ironrag.documents


@pytest.mark.asyncio
async def test_dependent_of_primary_kind_is_rejected_before_any_mutation(tmp_path: Path) -> None:
    adapter = _dependent_adapter("t1", DependentSpec("page", "2", "t9"))
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)

    ref = await first_ref(adapter)
    with pytest.raises(ValueError, match="primary kind"):
        await orchestrator.push_ref(ref)

    assert ironrag.creates == []
    assert state.get("page", "1") is None


@pytest.mark.asyncio
async def test_materialized_dependents_token_must_match_enumerated_ref(tmp_path: Path) -> None:
    class DriftingAdapter(DependentPageAdapter):
        async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
            result = await super().fetch(ref)
            assert isinstance(result, SourceFetchMaterialized)
            item = result.item
            return SourceFetchMaterialized(
                replace(item, ref=replace(item.ref, dependents_token="drifted"))
            )

    adapter = DriftingAdapter({"1": echo_page("1", "t1")}, {"1": ()})
    state = _state(tmp_path)
    ironrag = FakeIronRag()
    orchestrator = _orchestrator(adapter, ironrag, state)

    ref = await first_ref(adapter)
    with pytest.raises(ValueError, match="snapshot does not match"):
        await orchestrator.push_ref(ref)

    assert ironrag.creates == []
