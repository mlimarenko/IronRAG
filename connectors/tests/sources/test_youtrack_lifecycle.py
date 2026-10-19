from __future__ import annotations

from collections.abc import AsyncIterator, Sequence
from copy import deepcopy
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, NotRequired, TypedDict, Unpack
from uuid import UUID, uuid4

import pytest
from sdk_dependent_fixtures import RevisionRequest, revision_request_identity

from ironrag_connector.ironrag import (
    DocumentResource,
    IronRagConflictError,
    IronRagNotFoundError,
    OperationHandle,
    OperationProgress,
    OperationStatus,
    OperationStatusValue,
    ProblemDetails,
    VisibilityIntentReceipt,
    VisibilityIntentRequest,
)
from ironrag_connector.orchestrator import Orchestrator
from ironrag_connector.policy import DeleteAction, PushPolicy
from ironrag_connector.routing import (
    PolicyOverrides,
    ResolvedLibraryTarget,
    Router,
    RoutingConfig,
)
from ironrag_connector.sources.youtrack.adapter import YouTrackAdapter
from ironrag_connector.sources.youtrack.config import YouTrackSettings
from ironrag_connector.sources.youtrack.youtrack import YouTrackError, YouTrackNotFoundError
from ironrag_connector.state import StateStore
from ironrag_connector.sync import SyncManager

WORKSPACE_ID = UUID("00000000-0000-0000-0000-000000000099")
LIBRARY_ID = UUID("00000000-0000-0000-0000-000000000077")
LIBRARY_REF = "default/youtrack-knowledge-base"


def _settings(**overrides: object) -> YouTrackSettings:
    values: dict[str, object] = {
        "youtrack_base_url": "https://tracker.example.com/youtrack",
        "youtrack_token": "perm:synthetic-token",
        "ironrag_base_url": "https://ironrag.example.com",
        "ironrag_api_token": "synthetic-ironrag-token",
        "admin_bearer_token": "synthetic-admin-token",
        "sync_concurrency": 1,
    }
    values.update(overrides)
    return YouTrackSettings(**values)


def _article(
    article_id: str, readable_id: str, summary: str, content: str, updated: int
) -> dict[str, Any]:
    return {
        "id": article_id,
        "idReadable": readable_id,
        "summary": summary,
        "content": content,
        "created": updated - 1,
        "updated": updated,
        "project": {
            "id": "0-7",
            "shortName": "DOCS",
            "name": "Documentation",
            "archived": False,
        },
        "parentArticle": None,
        "reporter": {"id": "1-2", "login": "writer", "name": "Writer"},
        "tags": [],
        "attachments": [],
        "visibility": {"$type": "UnlimitedVisibility"},
    }


def _issue(
    issue_id: str,
    readable_id: str,
    summary: str,
    description: str,
    updated: int,
    *,
    comments_count: int = 0,
) -> dict[str, Any]:
    return {
        "id": issue_id,
        "idReadable": readable_id,
        "summary": summary,
        "description": description,
        "created": updated - 1,
        "updated": updated,
        "resolved": None,
        "commentsCount": comments_count,
        "project": {
            "id": "0-8",
            "shortName": "IRG",
            "name": "IronRAG",
            "archived": False,
        },
        "reporter": {"id": "1-2", "login": "writer", "name": "Writer"},
        "updater": {"id": "1-2", "login": "writer", "name": "Writer"},
        "tags": [],
        "customFields": [],
        "attachments": [],
        "links": [],
        "visibility": {"$type": "UnlimitedVisibility"},
    }


def _comment(
    comment_id: str,
    text: str,
    *,
    created: int,
    updated: int | None = None,
    visibility: dict[str, Any] | None = None,
) -> dict[str, Any]:
    return {
        "id": comment_id,
        "text": text,
        "author": {"id": "1-3", "login": "commenter", "name": "Commenter"},
        "created": created,
        "updated": created if updated is None else updated,
        "deleted": False,
        "pinned": False,
        "attachments": [],
        "visibility": visibility or {"$type": "UnlimitedVisibility"},
    }


class MutableYouTrack:
    def __init__(
        self,
        articles: list[dict[str, Any]],
        *,
        issues: list[dict[str, Any]] | None = None,
        article_comments: dict[str, list[dict[str, Any]]] | None = None,
        issue_comments: dict[str, list[dict[str, Any]]] | None = None,
    ) -> None:
        self.articles = {article["id"]: article for article in articles}
        self.issues = {issue["id"]: issue for issue in issues or []}
        self.article_comments = deepcopy(article_comments or {})
        self.issue_comments = deepcopy(issue_comments or {})
        for issue_id, issue in self.issues.items():
            declared = issue.get("commentsCount")
            if isinstance(declared, bool) or not isinstance(declared, int) or declared < 0:
                raise AssertionError("synthetic issue commentsCount must be a non-negative integer")
            if declared != len(self.issue_comments.get(issue_id, [])):
                raise AssertionError("synthetic issue commentsCount must match issue_comments")
        self.fetch_count = 0

    def set_issue_comments(self, issue_id: str, comments: list[dict[str, Any]]) -> None:
        if issue_id not in self.issues:
            raise AssertionError("synthetic issue must exist before setting comments")
        self.issue_comments[issue_id] = deepcopy(comments)
        self.issues[issue_id]["commentsCount"] = len(comments)

    async def validate_credentials(self) -> dict[str, Any]:
        return {"id": "1-2", "login": "writer"}

    async def list_articles(self) -> AsyncIterator[dict[str, Any]]:
        for article_id in sorted(self.articles):
            yield deepcopy(self.articles[article_id])

    async def get_article(self, article_id: str) -> dict[str, Any]:
        self.fetch_count += 1
        if article_id not in self.articles:
            raise YouTrackNotFoundError(article_id)
        return deepcopy(self.articles[article_id])

    async def list_article_comments(self, article_id: str) -> AsyncIterator[dict[str, Any]]:
        for comment in self.article_comments.get(article_id, []):
            yield deepcopy(comment)

    async def list_article_attachments(self, article_id: str) -> AsyncIterator[dict[str, Any]]:
        for attachment in self.articles.get(article_id, {}).get("attachments", []):
            yield deepcopy(attachment)

    async def list_issues(self) -> AsyncIterator[dict[str, Any]]:
        for issue_id in sorted(self.issues):
            yield deepcopy(self.issues[issue_id])

    async def get_issue(self, issue_id: str) -> dict[str, Any]:
        self.fetch_count += 1
        if issue_id not in self.issues:
            raise YouTrackNotFoundError(issue_id)
        return deepcopy(self.issues[issue_id])

    async def list_issue_comments(self, issue_id: str) -> AsyncIterator[dict[str, Any]]:
        for comment in self.issue_comments.get(issue_id, []):
            yield deepcopy(comment)

    async def list_issue_attachments(self, issue_id: str) -> AsyncIterator[dict[str, Any]]:
        for attachment in self.issues.get(issue_id, {}).get("attachments", []):
            yield deepcopy(attachment)

    async def list_issue_links(self, issue_id: str) -> AsyncIterator[dict[str, Any]]:
        for link in self.issues.get(issue_id, {}).get("links", []):
            yield deepcopy(link)

    async def list_issue_link_targets(
        self, issue_id: str, link_id: str
    ) -> AsyncIterator[dict[str, Any]]:
        del issue_id, link_id
        return
        yield  # pragma: no cover - makes this an async generator with no items

    async def aclose(self) -> None:
        return None


class FailingEnumerationYouTrack(MutableYouTrack):
    async def list_articles(self) -> AsyncIterator[dict[str, Any]]:
        for index, article_id in enumerate(sorted(self.articles)):
            if index == 1:
                raise YouTrackError("synthetic second-page failure")
            yield deepcopy(self.articles[article_id])


class _CreateDocumentRequest(TypedDict):
    external_key: str
    file_bytes: NotRequired[bytes | None]
    file_name: NotRequired[str | None]
    mime_type: NotRequired[str | None]
    title: NotRequired[str | None]
    document_hint: NotRequired[str | None]
    parent_external_key: NotRequired[str | None]


def _not_found_error(document_id: str) -> IronRagNotFoundError:
    return IronRagNotFoundError(
        ProblemDetails(
            type="urn:ironrag:error:not_found",
            title="Not Found",
            status=404,
            detail=f"synthetic not found: {document_id}",
            code="not_found",
        )
    )


def _ready_operation(operation_id: UUID) -> OperationStatus:
    return OperationStatus(
        id=operation_id,
        workspace_id=WORKSPACE_ID,
        library_id=LIBRARY_ID,
        operation_kind="synthetic",
        status=OperationStatusValue.READY,
        created_at=datetime.now(tz=UTC),
        progress=OperationProgress(),
    )


class MemoryIronRag:
    """In-memory double for the redesigned :class:`IronRagClient` surface:
    synchronous ``create_document``, asynchronous ``create_revision``/
    ``delete_document`` polled via ``wait_for_operation``, and
    ``find_document``/``list_documents`` reads."""

    def __init__(self) -> None:
        self.documents: dict[str, dict[str, Any]] = {}
        self.deleted_ids: list[str] = []
        self.revision_counts: dict[str, int] = {}
        self._operations: dict[UUID, OperationStatus] = {}
        self.intents: dict[str, VisibilityIntentReceipt] = {}
        self.intent_bodies: dict[str, tuple[UUID, VisibilityIntentRequest]] = {}
        self.payload_handles: dict[str, OperationHandle] = {}
        self.payload_bodies: dict[str, tuple[Any, ...]] = {}

    async def ensure_visibility_contract(self) -> None:
        return None

    async def get_document(self, document_id: UUID | str) -> DocumentResource | None:
        return next(
            (
                self._resource(doc)
                for doc in self.documents.values()
                if doc["id"] == str(document_id)
            ),
            None,
        )

    async def create_visibility_intent(
        self, document_id: UUID | str, *, request: VisibilityIntentRequest
    ) -> VisibilityIntentReceipt:
        body = (UUID(str(document_id)), request)
        if request.idempotency_key in self.intents:
            if self.intent_bodies[request.idempotency_key] != body:
                raise IronRagConflictError(
                    ProblemDetails(
                        type="urn:ironrag:error:idempotency_conflict",
                        title="Idempotency conflict",
                        status=409,
                        code="idempotency_conflict",
                        detail="complete visibility identity differs",
                    )
                )
            return self.intents[request.idempotency_key]
        intent_id = uuid4()
        receipt = VisibilityIntentReceipt(
            visibility_intent_id=intent_id,
            mutation_item_id=intent_id,
            mutation_id=uuid4(),
            async_operation_id=uuid4(),
            document_id=UUID(str(document_id)),
            revision_floor=1,
            source_identity=request.source_identity,
            materialization_token=request.materialization_token,
            replacement=request.replacement,
            request_fingerprint="opaque-visibility-fingerprint",
        )
        self.intents[request.idempotency_key] = receipt
        self.intent_bodies[request.idempotency_key] = body
        for document in self.documents.values():
            if document["id"] == str(document_id):
                document.update(
                    visibilityIntentId=intent_id,
                    readableRevisionId=None,
                    readableMutationItemId=None,
                )
        return receipt

    def _resource(self, document: dict[str, Any]) -> DocumentResource:
        return DocumentResource(
            id=UUID(document["id"]),
            library_id=UUID(document["libraryId"]),
            external_key=document["externalKey"],
            status="ready",
            document_hint=document["documentHint"],
            visibility_intent_id=document.get("visibilityIntentId"),
            readable_revision_id=document.get("readableRevisionId"),
            readable_mutation_item_id=document.get("readableMutationItemId"),
            admission=document.get("admission"),
        )

    def _admit(self) -> OperationHandle:
        operation_id = uuid4()
        self._operations[operation_id] = _ready_operation(operation_id)
        return OperationHandle(operation_id=operation_id)

    async def find_document(self, library_id: UUID, external_key: str) -> DocumentResource | None:
        document = self.documents.get(external_key)
        if document and document["libraryId"] == str(library_id):
            return self._resource(document)
        return None

    async def list_documents(
        self,
        library_id: UUID,
        *,
        search: str | None = None,
        external_key: str | None = None,
        status: Sequence[str] = (),
        include_deleted: bool = False,
        limit: int = 200,
    ) -> AsyncIterator[DocumentResource]:
        for key in sorted(self.documents):
            document = self.documents[key]
            if document["libraryId"] != str(library_id):
                continue
            if external_key and key != external_key:
                continue
            if search and search not in key:
                continue
            yield self._resource(document)

    async def create_document(
        self, library_id: UUID, **request: Unpack[_CreateDocumentRequest]
    ) -> DocumentResource:
        external_key = request["external_key"]
        document = {
            "id": str(uuid4()),
            "externalKey": external_key,
            "libraryId": str(library_id),
            "title": request.get("title"),
            "payload": bytes(request.get("file_bytes") or b""),
            "documentHint": request.get("document_hint"),
        }
        self.documents[external_key] = document
        self.revision_counts[external_key] = 1
        import hashlib

        admission = self._admit().model_copy(
            update={
                "document_id": UUID(document["id"]),
                "result_revision_id": uuid4(),
                "mutation_item_id": uuid4(),
                "result_checksum": "sha256:" + hashlib.sha256(document["payload"]).hexdigest(),
            }
        )
        document.update(
            admission=admission,
            readableRevisionId=admission.result_revision_id,
            readableMutationItemId=admission.mutation_item_id,
        )
        return self._resource(document)

    async def create_revision(
        self,
        document_id: UUID | str,
        **request: Unpack[RevisionRequest],
    ) -> OperationHandle:
        target = str(document_id)
        body = revision_request_identity(document_id, request)
        idempotency_key = request["idempotency_key"]
        file_bytes = request.get("file_bytes")
        if idempotency_key in self.payload_handles:
            if self.payload_bodies[idempotency_key] != body:
                raise IronRagConflictError(
                    ProblemDetails(
                        type="urn:ironrag:error:idempotency_conflict",
                        title="Idempotency conflict",
                        status=409,
                        code="idempotency_conflict",
                        detail="complete request identity differs",
                    )
                )
            return self.payload_handles[idempotency_key]
        for external_key, document in self.documents.items():
            if document["id"] == target:
                document["payload"] = bytes(file_bytes or b"")
                self.revision_counts[external_key] += 1
                import hashlib

                handle = self._admit().model_copy(
                    update={
                        "visibility_intent_id": request.get("visibility_intent_id"),
                        "document_id": UUID(target),
                        "result_revision_id": uuid4(),
                        "mutation_item_id": uuid4(),
                        "result_checksum": "sha256:"
                        + hashlib.sha256(file_bytes or b"").hexdigest(),
                        "request_fingerprint": "opaque-payload-fingerprint",
                    }
                )
                document.update(
                    title=request.get("title"),
                    documentHint=request.get("document_hint"),
                    readableRevisionId=handle.result_revision_id,
                    readableMutationItemId=handle.mutation_item_id,
                )
                self.payload_handles[idempotency_key] = handle
                self.payload_bodies[idempotency_key] = body
                return handle
        raise _not_found_error(target)

    async def delete_document(
        self, document_id: UUID | str, *, idempotency_key: str
    ) -> OperationHandle | None:
        target = str(document_id)
        for key, document in list(self.documents.items()):
            if document["id"] == target:
                del self.documents[key]
                self.deleted_ids.append(target)
                return self._admit()
        return None

    async def wait_for_operation(
        self,
        operation_id: UUID | str,
        *,
        poll_interval: float | None = None,
        budget: float | None = None,
    ) -> OperationStatus:
        return self._operations[UUID(str(operation_id))]


@pytest.mark.asyncio
async def test_memory_revision_replay_checks_complete_body() -> None:
    server = MemoryIronRag()
    document = await server.create_document(
        LIBRARY_ID, external_key="fake:page:1", file_bytes=b"same"
    )
    body = {"mode": "replace", "file_bytes": b"same", "idempotency_key": "fixed"}
    handle = await server.create_revision(document.id, **body)
    assert await server.create_revision(document.id, **body) == handle
    with pytest.raises(IronRagConflictError) as caught:
        await server.create_revision(document.id, **body, title="changed")
    assert caught.value.problem.code == "idempotency_conflict"


@pytest.mark.asyncio
@pytest.mark.parametrize("changed", ["target", "source", "token", "metadata"])
async def test_memory_visibility_replay_checks_complete_body(changed: str) -> None:
    server = MemoryIronRag()
    document = await server.create_document(
        LIBRARY_ID, external_key="fake:page:1", file_bytes=b"same"
    )
    request = VisibilityIntentRequest.model_validate(
        {
            "idempotencyKey": "fixed",
            "sourceIdentity": "fake:page:1",
            "materializationToken": "v1",
            "replacement": {
                "checksum": "sha256:" + "a" * 64,
                "byteSize": 4,
                "mimeType": "text/plain",
                "fileName": "page.txt",
            },
        }
    )
    receipt = await server.create_visibility_intent(document.id, request=request)
    assert await server.create_visibility_intent(document.id, request=request) == receipt
    altered = request
    target = uuid4() if changed == "target" else document.id
    if changed == "source":
        altered = request.model_copy(update={"source_identity": "other"})
    elif changed == "token":
        altered = request.model_copy(update={"materialization_token": "v2"})
    elif changed == "metadata":
        altered = request.model_copy(
            update={"replacement": request.replacement.model_copy(update={"title": "new"})}
        )
    with pytest.raises(IronRagConflictError) as caught:
        await server.create_visibility_intent(target, request=altered)
    assert caught.value.problem.code == "idempotency_conflict"
    assert server.intents == {"fixed": receipt}


def _manager(
    tmp_path: Path,
    vendor: MutableYouTrack,
    ironrag: MemoryIronRag,
    *,
    on_missing: DeleteAction = DeleteAction.DELETE,
) -> SyncManager:
    policies = PolicyOverrides(default=PushPolicy(on_missing=on_missing), by_kind={})
    adapter = YouTrackAdapter(_settings(), policies, client=vendor)
    router = Router(
        RoutingConfig.model_validate(
            {
                "default": {
                    "library": LIBRARY_REF,
                }
            }
        ),
        resolved_targets={
            LIBRARY_REF: ResolvedLibraryTarget(
                library_ref=LIBRARY_REF,
                workspace_id=WORKSPACE_ID,
                library_id=LIBRARY_ID,
            )
        },
    )
    state = StateStore(tmp_path / "state.sqlite")
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    return SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=policies,
        concurrency=1,
        interval_seconds=60,
    )


@pytest.mark.asyncio
async def test_create_noop_restart_update_clear_and_delete_lifecycle(tmp_path: Path) -> None:
    first = _article("226-0", "DOCS-A-1", "First", "Initial body", 1000)
    second = _article("226-1", "DOCS-A-2", "Second", "Second body", 1000)
    vendor = MutableYouTrack([first, second])
    ironrag = MemoryIronRag()
    manager = _manager(tmp_path, vendor, ironrag)

    created = await manager.run_once(reason="initial")
    assert (created.created, created.errors) == (2, 0)
    first_document_id = ironrag.documents["youtrack:article:226-0"]["id"]
    initial_fetches = vendor.fetch_count

    unchanged = await manager.run_once(reason="unchanged")
    assert (unchanged.noop_unchanged, unchanged.created, unchanged.replaced) == (0, 0, 2)
    assert vendor.fetch_count > initial_fetches

    restarted = _manager(tmp_path, vendor, ironrag)
    after_restart = await restarted.run_once(reason="restart")
    assert after_restart.noop_unchanged == 2
    assert vendor.fetch_count > initial_fetches

    vendor.articles["226-0"].update(summary="First renamed", content="Updated body", updated=2000)
    updated = await restarted.run_once(reason="update")
    assert (updated.replaced, updated.noop_unchanged, updated.errors) == (1, 1, 0)
    document = ironrag.documents["youtrack:article:226-0"]
    assert document["id"] == first_document_id
    assert document["payload"].decode().startswith("# First renamed\n")
    assert "Updated body" in document["payload"].decode()

    vendor.articles["226-0"].update(content="", updated=3000)
    cleared = await restarted.run_once(reason="clear")
    assert cleared.replaced == 1
    cleared_payload = ironrag.documents["youtrack:article:226-0"]["payload"].decode()
    assert cleared_payload.startswith("# First renamed\n")
    assert "Updated body" not in cleared_payload

    deleted_source = vendor.articles.pop("226-1")
    deleted_document_id = ironrag.documents["youtrack:article:226-1"]["id"]
    deleted = await restarted.run_once(reason="delete")
    assert (deleted.reaped, deleted.errors) == (1, 0)
    assert "youtrack:article:226-1" not in ironrag.documents
    assert deleted_document_id in ironrag.deleted_ids
    assert deleted_source["id"] == "226-1"


@pytest.mark.asyncio
async def test_partial_enumeration_never_reaps_unseen_documents(tmp_path: Path) -> None:
    first = _article("226-0", "DOCS-A-1", "First", "Body", 1000)
    second = _article("226-1", "DOCS-A-2", "Second", "Body", 1000)
    healthy_vendor = MutableYouTrack([first, second])
    ironrag = MemoryIronRag()
    healthy = _manager(tmp_path, healthy_vendor, ironrag)
    assert (await healthy.run_once(reason="seed")).created == 2

    failing = _manager(tmp_path, FailingEnumerationYouTrack([first, second]), ironrag)
    report = await failing.run_once(reason="partial")

    assert report.errors == 1
    assert report.reaped == 0
    assert set(ironrag.documents) == {
        "youtrack:article:226-0",
        "youtrack:article:226-1",
    }


@pytest.mark.asyncio
async def test_issue_comment_create_edit_delete_and_restart_lifecycle(tmp_path: Path) -> None:
    issue = _issue("3-0", "IRG-92", "Comment lifecycle", "Stable parent body", 1000)
    vendor = MutableYouTrack([], issues=[issue])
    ironrag = MemoryIronRag()
    manager = _manager(tmp_path, vendor, ironrag)
    external_key = "youtrack:issue:3-0"

    initial = await manager.run_once(reason="issue-seed")
    assert (initial.created, initial.errors) == (1, 0)
    document_id = ironrag.documents[external_key]["id"]
    parent_updated = vendor.issues["3-0"]["updated"]
    assert ironrag.revision_counts[external_key] == 1

    vendor.set_issue_comments("3-0", [_comment("7-1", "VISIBLE-COMMENT-ALPHA", created=1100)])
    assert vendor.issues["3-0"]["commentsCount"] == 1
    created = await manager.run_once(reason="comment-create")
    assert (created.replaced, created.noop_unchanged, created.errors) == (1, 0, 0)
    assert ironrag.documents[external_key]["id"] == document_id
    assert b"VISIBLE-COMMENT-ALPHA" in ironrag.documents[external_key]["payload"]
    assert ironrag.revision_counts[external_key] == 2
    assert vendor.issues["3-0"]["updated"] == parent_updated

    stable = await manager.run_once(reason="comment-create-stable")
    assert (stable.noop_unchanged, stable.replaced, stable.errors) == (1, 0, 0)
    assert ironrag.revision_counts[external_key] == 2

    vendor.set_issue_comments(
        "3-0", [_comment("7-1", "VISIBLE-COMMENT-BETA", created=1100, updated=1200)]
    )
    assert vendor.issues["3-0"]["commentsCount"] == 1
    edited = await manager.run_once(reason="comment-edit")
    assert (edited.replaced, edited.errors) == (1, 0)
    edited_payload = ironrag.documents[external_key]["payload"]
    assert b"VISIBLE-COMMENT-BETA" in edited_payload
    assert b"VISIBLE-COMMENT-ALPHA" not in edited_payload
    assert ironrag.revision_counts[external_key] == 3
    assert vendor.issues["3-0"]["updated"] == parent_updated

    vendor.set_issue_comments("3-0", [])
    assert vendor.issues["3-0"]["commentsCount"] == 0
    deleted = await manager.run_once(reason="comment-delete")
    assert (deleted.replaced, deleted.errors) == (1, 0)
    assert b"VISIBLE-COMMENT-BETA" not in ironrag.documents[external_key]["payload"]
    assert ironrag.documents[external_key]["id"] == document_id
    assert ironrag.revision_counts[external_key] == 4

    restarted = _manager(tmp_path, vendor, ironrag)
    after_restart = await restarted.run_once(reason="comment-delete-restart")
    assert (after_restart.noop_unchanged, after_restart.replaced, after_restart.errors) == (1, 0, 0)
    assert ironrag.documents[external_key]["id"] == document_id
    assert ironrag.revision_counts[external_key] == 4


@pytest.mark.asyncio
async def test_restricted_issue_comment_transition_removes_prior_marker(tmp_path: Path) -> None:
    issue = _issue(
        "3-0",
        "IRG-92",
        "Visibility lifecycle",
        "Stable parent body",
        1000,
        comments_count=1,
    )
    vendor = MutableYouTrack(
        [],
        issues=[issue],
        issue_comments={
            "3-0": [_comment("7-1", "MUST-DISAPPEAR", created=1100)],
        },
    )
    ironrag = MemoryIronRag()
    manager = _manager(tmp_path, vendor, ironrag)
    external_key = "youtrack:issue:3-0"

    assert (await manager.run_once(reason="visible-comment")).created == 1
    document_id = ironrag.documents[external_key]["id"]
    assert b"MUST-DISAPPEAR" in ironrag.documents[external_key]["payload"]

    vendor.issue_comments["3-0"][0]["visibility"] = {
        "$type": "LimitedVisibility",
        "permittedUsers": [{"id": "1-private", "login": "must-not-leak"}],
        "permittedGroups": [],
    }
    restricted = await manager.run_once(reason="restricted-comment")

    assert (restricted.replaced, restricted.errors) == (1, 0)
    assert ironrag.documents[external_key]["id"] == document_id
    restricted_payload = ironrag.documents[external_key]["payload"]
    assert b"MUST-DISAPPEAR" not in restricted_payload
    assert b"must-not-leak" not in restricted_payload
    assert ironrag.revision_counts[external_key] == 2

    stable = await manager.run_once(reason="restricted-comment-stable")
    assert (stable.noop_unchanged, stable.replaced, stable.errors) == (1, 0, 0)
    assert ironrag.revision_counts[external_key] == 2


@pytest.mark.asyncio
async def test_required_parent_visibility_delete_overrides_on_missing_ignore(
    tmp_path: Path,
) -> None:
    issue = _issue("3-0", "IRG-92", "Visibility lifecycle", "Public body", 1000)
    vendor = MutableYouTrack([], issues=[issue])
    ironrag = MemoryIronRag()
    manager = _manager(tmp_path, vendor, ironrag, on_missing=DeleteAction.IGNORE)
    external_key = "youtrack:issue:3-0"

    public = await manager.run_once(reason="public-parent")
    assert (public.created, public.skipped_limited_visibility, public.errors) == (1, 0, 0)
    deleted_document_id = ironrag.documents[external_key]["id"]

    vendor.issues["3-0"]["visibility"] = {
        "$type": "LimitedVisibility",
        "permittedUsers": [],
        "permittedGroups": [{"id": "3-private", "name": "must-not-leak"}],
    }
    restricted = await manager.run_once(reason="restricted-parent")

    assert (restricted.reaped, restricted.skipped_missing, restricted.errors) == (1, 0, 0)
    assert restricted.skipped_limited_visibility == 1
    assert external_key not in ironrag.documents
    assert deleted_document_id in ironrag.deleted_ids

    stable = await manager.run_once(reason="restricted-parent-stable")
    assert (stable.skipped_empty, stable.reaped, stable.errors) == (1, 0, 0)
    assert stable.skipped_limited_visibility == 1
    assert external_key not in ironrag.documents


@pytest.mark.asyncio
async def test_never_indexed_limited_visibility_entity_is_skipped_every_sweep(
    tmp_path: Path,
) -> None:
    restricted = _article("226-0", "DOCS-A-1", "Restricted", "Secret body", 1000)
    restricted["visibility"] = {
        "$type": "LimitedVisibility",
        "permittedUsers": [],
        "permittedGroups": [{"id": "3-private", "name": "must-not-leak"}],
    }
    vendor = MutableYouTrack([restricted])
    ironrag = MemoryIronRag()
    manager = _manager(tmp_path, vendor, ironrag)

    # Limited from the first sweep, so it is never indexed: the REQUIRED delete
    # is a source_absent, not a reap, and the counter still records the
    # visibility-forced decision instead of hiding it in skipped_empty.
    first = await manager.run_once(reason="restricted-first-sweep")
    assert first.created == 0
    assert first.skipped_limited_visibility == 1
    assert "youtrack:article:226-0" not in ironrag.documents

    # The UnlimitedVisibility control is created and leaves the counter at zero,
    # so the counter isolates the visibility gate rather than any absence.
    vendor.articles = {"226-1": _article("226-1", "DOCS-A-2", "Public", "Public body", 1000)}
    control = await manager.run_once(reason="public-control")
    assert control.created == 1
    assert control.skipped_limited_visibility == 0
    assert "youtrack:article:226-1" in ironrag.documents
