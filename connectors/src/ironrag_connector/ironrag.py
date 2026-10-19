"""Async IronRAG content client — targets the redesigned `/v1` REST surface.

Endpoints
=========

* ``GET /v1/catalog/workspaces`` and
  ``GET /v1/catalog/workspaces/{id}/libraries`` — resolve canonical
  ``workspace-slug/library-slug`` routing refs once per routing snapshot.
  The two have **different response shapes**, and each has its own reader
  here rather than one shape-sniffing helper: workspaces come back as a
  bare JSON array, while libraries come back as a bounded
  ``{items, totalCount, offset, limit}`` page with ``search``/``offset``/
  ``limit`` query parameters. A library ref is therefore resolved by
  walking that page to exhaustion, never by reading the first page.
* ``GET /v1/content/libraries/{libraryId}/documents`` — the single
  read-collection interface: cursor pagination, ``search`` (a
  case-insensitive substring/ILIKE filter on ``external_key`` only --
  there is no dedicated exact-match filter), ``includeDeleted``,
  ``status``. Backs both :meth:`IronRagClient.find_document` and
  :meth:`IronRagClient.list_documents`; both reconstruct an exact-key
  lookup by passing the key as ``search`` and filtering the (typically
  single) page of matches client-side for an exact ``externalKey``
  equality, since ``search`` narrows on the same column.
* ``POST /v1/content/libraries/{libraryId}/documents`` — content-negotiated
  create (``application/json`` for metadata/pointer documents,
  ``multipart/form-data`` for byte uploads). Synchronous: 201 + Location.
* ``POST /v1/content/documents/{id}/revisions`` — content-negotiated
  revision (JSON ``{mode: "append"|"replace"}`` for text, multipart for a
  file replace). Asynchronous: 202 + ``Location`` to the canonical
  ``/v1/ops/operations/{operationId}``.
* ``DELETE /v1/content/documents/{id}`` — asynchronous in effect (always
  creates an ``ops_async_operation`` row to poll), but returns a plain
  200 with the operation id in the JSON body rather than 202 + Location
  like revisions. Handled uniformly through the same typed body field.
* ``GET /v1/content/documents/{id}`` — single document detail.
* ``GET /v1/ops/operations/{operationId}`` — the canonical poll target
  for every asynchronous mutation. :meth:`IronRagClient.wait_for_operation`
  is the one poll-to-terminal primitive every mutating call funnels
  through.

Errors
======

Every non-2xx response is RFC 9457 ``application/problem+json``:
``{type, title, status, detail, code, requestId?, ...extensions}``, with
extension members (e.g. ``existingDocumentId`` on a 409 duplicate-content
conflict) flattened at the top level rather than nested. Error handling
here is entirely typed field access — no regex or substring sniffing of
error messages. A 400 admission rejection from
:meth:`IronRagClient.create_document` or
:meth:`IronRagClient.create_revision` carrying the explicit
``failureDisposition=content_unsupported`` extension is the synchronous
content rejection and maps to :class:`IronRagContentUnsupportedProblemError`;
every other status, every other disposition, and the same field seen by a
non-admission call (a read, delete or operation poll) stay a
:class:`IronRagProblemError`.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import AsyncIterator, Callable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime
from enum import StrEnum
from typing import Any, Literal, Protocol
from uuid import UUID

import httpx
from pydantic import (
    AliasChoices,
    BaseModel,
    ConfigDict,
    Field,
    StrictInt,
    StrictStr,
    ValidationError,
    model_validator,
)

from .config import BaseConnectorSettings
from .observability import get_logger
from .routing import ResolvedLibraryTarget, normalize_library_ref

log = get_logger(__name__)

DEFAULT_REWALK_CONCURRENCY = 4
"""Conservative default fan-out for a post-upgrade full re-walk (S7.6).

Bounds how many in-flight create/revision calls a caller issues while
consuming :meth:`IronRagClient.walk_all_documents` or driving a sweep --
this module never fetches list pages concurrently (cursor pagination is
inherently sequential; page N+1 needs page N's cursor). Operators should
confirm this default against each source system's documented rate limit
before the first production re-walk (plan S7.6/S9 step 2).
"""

CATALOG_LIBRARY_PAGE_LIMIT = 100
"""Page size requested when walking the catalog library collection.

The endpoint clamps ``limit`` to its own maximum, so a page may come back
shorter than this without meaning the collection is exhausted -- only
``totalCount`` decides that.
"""


# ---------------------------------------------------------------------------
# Typed response models
# ---------------------------------------------------------------------------


@dataclass(frozen=True, slots=True)
class RevisionProvenance:
    """Optional source origin and language carried together on a replacement."""

    language_code: str | None = None
    source_uri: str | None = None


_EMPTY_REVISION_PROVENANCE = RevisionProvenance()


@dataclass(frozen=True, slots=True)
class _CatalogPage:
    """One ``{items, totalCount, offset, limit}`` catalog page.

    ``total_count`` is scoped to the request's filters, not to the whole
    collection -- a searched page reports how many rows matched the search.
    """

    items: tuple[Mapping[str, Any], ...]
    total_count: int


class DocumentResource(BaseModel):
    """One document as returned by list/find/get/create.

    The list endpoint (``ContentDocumentListItem``) and the get/create
    endpoints (``ContentDocumentDetailResponse``, unwrapped by
    :func:`_extract_document_payload`) are genuinely different response
    shapes, not just naming drift: the list row carries a derived
    ``status`` bucket (``queued``/``processing``/``ready``/``failed``/
    ``canceled``), while the detail/create row instead carries the raw
    ``documentState`` lifecycle value (``active``/``deleted``) with no
    ``status`` field at all. ``status`` accepts either source key so one
    model can populate from both surfaces; a value from ``documentState``
    is the raw lifecycle state, not the derived status bucket -- callers
    that need the canonical bucket must go through :meth:`IronRagClient.
    list_documents`/:meth:`~IronRagClient.find_document`, not
    :meth:`~IronRagClient.get_document`/:meth:`~IronRagClient.create_document`.
    """

    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    id: UUID
    library_id: UUID = Field(alias="libraryId")
    workspace_id: UUID | None = Field(default=None, alias="workspaceId")
    external_key: str = Field(alias="externalKey")
    status: str | None = Field(
        default=None, validation_alias=AliasChoices("status", "documentState")
    )
    file_name: str | None = Field(default=None, alias="fileName")
    document_hint: str | None = Field(default=None, alias="documentHint")
    uploaded_at: datetime | None = Field(default=None, alias="uploadedAt")
    visibility_intent_id: UUID | None = Field(default=None, alias="visibilityIntentId")
    readable_mutation_item_id: UUID | None = Field(default=None, alias="readableMutationItemId")
    readable_revision_id: UUID | None = Field(default=None, alias="readableRevisionId")
    admission: OperationHandle | None = Field(default=None, exclude=True)


class ReplacementIdentity(BaseModel):
    """Complete byte and metadata assertion; not an upstream freshness proof."""

    model_config = ConfigDict(populate_by_name=True, extra="forbid", frozen=True)

    checksum: StrictStr = Field(pattern=r"^sha256:[0-9a-f]{64}$")
    byte_size: StrictInt = Field(alias="byteSize", ge=0)
    mime_type: StrictStr = Field(alias="mimeType")
    file_name: StrictStr = Field(alias="fileName")
    title: StrictStr | None = None
    language_code: StrictStr | None = Field(default=None, alias="languageCode")
    source_uri: StrictStr | None = Field(default=None, alias="sourceUri")
    document_hint: StrictStr | None = Field(default=None, alias="documentHint")


class VisibilityIntentRequest(BaseModel):
    """Persist this complete request before sending its idempotent write."""

    model_config = ConfigDict(populate_by_name=True, extra="forbid", frozen=True)

    idempotency_key: StrictStr = Field(alias="idempotencyKey", min_length=1)
    source_identity: StrictStr = Field(alias="sourceIdentity", min_length=1)
    materialization_token: StrictStr | None = Field(alias="materializationToken")
    replacement: ReplacementIdentity


class VisibilityIntentReceipt(BaseModel):
    """Accepted denial only: no revision/job or continued-currentness claim.

    The fingerprint is opaque server evidence. The client never recomputes it.
    """

    model_config = ConfigDict(populate_by_name=True, extra="forbid", frozen=True)

    visibility_intent_id: UUID = Field(alias="visibilityIntentId")
    mutation_id: UUID = Field(alias="mutationId")
    mutation_item_id: UUID = Field(alias="mutationItemId")
    async_operation_id: UUID = Field(alias="asyncOperationId")
    document_id: UUID = Field(alias="documentId")
    revision_floor: StrictInt = Field(alias="revisionFloor", ge=0)
    source_identity: StrictStr = Field(alias="sourceIdentity", min_length=1)
    materialization_token: StrictStr | None = Field(alias="materializationToken")
    replacement: ReplacementIdentity
    request_fingerprint: StrictStr = Field(alias="requestFingerprint", min_length=1)

    @model_validator(mode="after")
    def validate_item_identity(self) -> VisibilityIntentReceipt:
        if self.visibility_intent_id != self.mutation_item_id:
            raise ValueError("visibilityIntentId must equal mutationItemId")
        return self

    def validate_binding(self, document_id: UUID, request: VisibilityIntentRequest) -> None:
        # Metadata normalization belongs to the server's single constructor.
        # Verify immutable bytes/source/target, retain the returned complete
        # descriptor, and submit that descriptor with the bound replacement.
        # Do not recreate the server's normalization or fingerprint algorithm.
        if (
            self.document_id != document_id
            or self.source_identity != request.source_identity
            or self.materialization_token != request.materialization_token
            or self.replacement.checksum != request.replacement.checksum
            or self.replacement.byte_size != request.replacement.byte_size
        ):
            raise IronRagError("IronRAG visibility receipt returned mismatched request binding")


class DocumentPage(BaseModel):
    """One cursor page from ``GET .../documents``."""

    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    items: list[DocumentResource]
    next_cursor: str | None = Field(default=None, alias="nextCursor")
    total: int | None = None


def _validate_collection_document_library(
    document: DocumentResource,
    requested_library_id: UUID,
) -> None:
    """Reject a collection row that escaped its requested library scope."""
    if document.library_id != requested_library_id:
        raise IronRagError(
            "IronRAG document collection returned mismatched library identity: "
            f"requested={requested_library_id}, returned={document.library_id}"
        )


class OperationStatusValue(StrEnum):
    """Canonical ``ops_async_operation.status`` values (server-authoritative)."""

    ACCEPTED = "accepted"
    PROCESSING = "processing"
    READY = "ready"
    FAILED = "failed"
    SUPERSEDED = "superseded"
    CANCELED = "canceled"

    @property
    def is_terminal(self) -> bool:
        return self in (
            OperationStatusValue.READY,
            OperationStatusValue.FAILED,
            OperationStatusValue.SUPERSEDED,
            OperationStatusValue.CANCELED,
        )


class OperationFailureDisposition(StrEnum):
    """Server-authoritative terminal disposition of a failed operation.

    Mirrors the server's ``OpsFailureDisposition``. Only
    :attr:`CONTENT_UNSUPPORTED` is a server-proven content rejection -- the
    bytes can never be ingested, so retrying them cannot change the outcome.
    The other members, and every value a newer server might introduce, are
    operational failures the connector must keep in ``errors``.
    """

    CONTENT_UNSUPPORTED = "content_unsupported"
    RETRY_EXHAUSTED = "retry_exhausted"
    OPERATIONAL_TERMINAL = "operational_terminal"


class OperationProgress(BaseModel):
    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    total: int = 0
    completed: int = 0
    failed: int = 0
    in_flight: int = Field(default=0, alias="inFlight")


class OperationStatus(BaseModel):
    """``GET /v1/ops/operations/{operationId}`` response body.

    The server's ``AsyncOperationDetailResponse`` applies
    ``#[serde(flatten)]`` to its ``OpsAsyncOperation`` row -- the row's
    fields (``id``, ``workspaceId``, ``operationKind``, ``status``, ...)
    sit directly at the top level of the JSON object, as siblings of
    ``progress``, not nested under an ``"operation"`` key. This model
    mirrors that flattened shape; there is no separate ``AsyncOperation``
    wrapper type.
    """

    model_config = ConfigDict(populate_by_name=True, extra="ignore")

    id: UUID
    workspace_id: UUID = Field(alias="workspaceId")
    library_id: UUID | None = Field(default=None, alias="libraryId")
    operation_kind: str = Field(alias="operationKind")
    status: OperationStatusValue
    failure_code: str | None = Field(default=None, alias="failureCode")
    # Kept as a plain string, not the enum: a server that introduces a new
    # disposition value must yield a failed operation the connector classifies
    # as an error, not a pydantic validation failure that hides the raw value.
    # ``operation_failure_exception`` is the single mapping decision.
    failure_disposition: str | None = Field(default=None, alias="failureDisposition")
    created_at: datetime = Field(alias="createdAt")
    completed_at: datetime | None = Field(default=None, alias="completedAt")
    progress: OperationProgress


class OperationHandle(BaseModel):
    """Payload admission, distinct from visibility acceptance and currentness."""

    model_config = ConfigDict(populate_by_name=True)

    operation_id: UUID
    document_id: UUID | None = None
    visibility_intent_id: UUID | None = None
    mutation_item_id: UUID | None = None
    result_revision_id: UUID | None = None
    result_checksum: str | None = None
    request_fingerprint: str | None = None


class ProblemDetails(BaseModel):
    """RFC 9457 ``application/problem+json`` error body.

    Known extension members (``existingDocumentId`` on the document-create
    duplicate-content conflict, and ``failureDisposition`` on a synchronous
    admission rejection) are declared as typed optional fields;
    ``extra="allow"`` preserves any other extension losslessly on
    ``model_extra`` without the client needing to know about it ahead of time.
    """

    model_config = ConfigDict(populate_by_name=True, extra="allow")

    type: str
    title: str
    status: int
    detail: str
    code: str
    request_id: str | None = Field(default=None, alias="requestId")
    existing_document_id: UUID | None = Field(default=None, alias="existingDocumentId")
    # The synchronous counterpart of ``OperationStatus.failure_disposition``:
    # a plain string, not the enum, so a value the connector does not know is
    # read losslessly and stays an error rather than a pydantic validation
    # failure. ``_raise_for_problem`` is the single mapping decision.
    failure_disposition: str | None = Field(default=None, alias="failureDisposition")


# ---------------------------------------------------------------------------
# Errors
# ---------------------------------------------------------------------------


class IronRagError(RuntimeError):
    """IronRAG returned a non-recoverable status."""


class IronRagCatalogError(IronRagError):
    """IronRAG could not resolve a canonical catalog reference."""


class IronRagMutationTimeoutError(IronRagError):
    """An async operation did not reach a terminal state inside the poll budget."""


class IronRagProblemError(IronRagError):
    """Base class for typed RFC 9457 problem+json errors.

    Carries the fully parsed :class:`ProblemDetails` so callers branch on
    ``.problem.code`` (and, for the duplicate-content conflict,
    ``.existing_document_id``) instead of sniffing the message text.
    """

    def __init__(self, problem: ProblemDetails) -> None:
        self.problem = problem
        super().__init__(problem.detail)


class IronRagNotFoundError(IronRagProblemError):
    """404 -- the referenced resource does not exist."""


class IronRagDuplicateContentError(IronRagProblemError):
    """409 ``duplicate_content`` -- an active document with this external key,
    or identical bytes, already exists. Carries the conflicting document id
    via the typed ``existingDocumentId`` problem+json extension member."""

    @property
    def existing_document_id(self) -> UUID | None:
        return self.problem.existing_document_id


class IronRagConflictError(IronRagProblemError):
    """409 for any conflict other than duplicate content (e.g. a document
    with a mutation already in flight)."""


class IronRagVisibilityConflictError(IronRagConflictError):
    """Typed visibility authority/binding rejection, never ordinary payload busy."""


class IronRagOperationFailedError(IronRagError):
    """A polled operation reached terminal status ``failed``.

    Raised for every terminal failure EXCEPT a server-proven content
    rejection. Callers must treat it as an operational error, never a skip:
    the server holds no terminal proof that the bytes are at fault, so
    retrying is the only honest response. Carries the full
    :class:`OperationStatus` (``failure_code``, ``failure_disposition``) for
    audit.
    """

    def __init__(self, status: OperationStatus) -> None:
        self.status = status
        super().__init__(
            f"operation {status.id} failed"
            + (f" ({status.failure_code})" if status.failure_code else "")
            + (f" [{status.failure_disposition}]" if status.failure_disposition else "")
        )


class IronRagContentUnsupportedError(IronRagOperationFailedError):
    """A failed operation the server proved is a content rejection.

    The server wrote ``failureDisposition=content_unsupported``: a
    deterministic content admission/rejection, so the same bytes can never be
    ingested and retrying cannot change the outcome. This is the asynchronous
    carrier of that proof; :class:`IronRagContentUnsupportedProblemError` is
    the synchronous one, and both are accounted as one terminal skip. It
    preserves the operation status, failure code and disposition for audit.
    """

    @property
    def failure_code(self) -> str | None:
        """Server failure code -- audit evidence, never a skip decision."""
        return self.status.failure_code

    @property
    def failure_disposition(self) -> str | None:
        """The server-proven disposition carried by the failed operation."""
        return self.status.failure_disposition


class IronRagContentUnsupportedProblemError(IronRagProblemError):
    """A synchronous admission problem the server proved is a content rejection.

    The server set ``failureDisposition=content_unsupported`` on an HTTP 400
    ``application/problem+json`` body: the same bytes can never be admitted, so
    retrying cannot change the outcome. This is the synchronous counterpart of
    :class:`IronRagContentUnsupportedError` and is accounted on the same
    terminal-skip branch. It carries the parsed :class:`ProblemDetails`
    (``code``/``detail``/``status``/``request_id``) for audit.
    """

    @property
    def failure_code(self) -> str | None:
        """Problem ``code`` -- audit evidence, never a skip decision."""
        return self.problem.code

    @property
    def failure_disposition(self) -> str:
        """The one proven disposition this exception is raised for."""
        return OperationFailureDisposition.CONTENT_UNSUPPORTED


def operation_failure_exception(status: OperationStatus) -> IronRagError:
    """The single mapping from a terminal ``failed`` operation to its exception.

    ``content_unsupported`` is the only disposition the server proves is a
    content rejection. ``retry_exhausted`` and ``operational_terminal`` are
    operational failures; a missing/``null`` disposition means an older server
    holds no proof; an unknown future value is unproven by construction. All of
    those map to :class:`IronRagOperationFailedError`, so no caller can turn an
    unproven failure into a skip.
    """
    if status.failure_disposition == OperationFailureDisposition.CONTENT_UNSUPPORTED:
        return IronRagContentUnsupportedError(status)
    return IronRagOperationFailedError(status)


def _malformed_uuid(value: Any) -> bool:
    """Whether ``value`` cannot be read as a UUID."""
    try:
        UUID(str(value))
    except (TypeError, ValueError, AttributeError):
        return True
    return False


def _sanitize_problem_body(body: Mapping[str, Any], response: httpx.Response) -> dict[str, Any]:
    """Coerce an arbitrary problem+json body into the declared typed shape.

    A malformed server must fail closed, not crash: a value of the wrong JSON
    type for a declared field is replaced with a safe scalar default (or
    dropped to ``None`` for an optional extension) before pydantic sees it.
    HTTP status/code/detail are preserved when the body supplies them as
    strings/int, and every unknown extension stays losslessly on
    ``model_extra``. In particular a non-string ``failureDisposition`` becomes
    ``None``, so it can never be read as the ``content_unsupported`` proof.
    """
    sanitized = dict(body)
    required_defaults: tuple[tuple[str, Any], ...] = (
        ("type", "urn:ironrag:error:unknown"),
        ("title", "Error"),
        ("detail", response.text[:400] or response.reason_phrase),
        ("code", "unknown"),
    )
    for name, default in required_defaults:
        if not isinstance(sanitized.get(name), str):
            sanitized[name] = default
    status = sanitized.get("status")
    if not isinstance(status, int) or isinstance(status, bool):
        sanitized["status"] = response.status_code
    # Pydantic accepts both wire aliases and field names (populate_by_name).
    # Sanitizing only the aliases would let malformed snake_case values force
    # the generic fallback and discard otherwise valid code/detail/extensions.
    for name, malformed in _OPTIONAL_PROBLEM_FIELD_CHECKS:
        if sanitized.get(name) is not None and malformed(sanitized[name]):
            sanitized.pop(name, None)
    return sanitized


def _not_a_string(value: Any) -> bool:
    """Whether ``value`` is not a JSON string."""
    return not isinstance(value, str)


# Optional problem extensions, under both wire alias and field name, with the
# predicate that marks a present value as malformed (dropped before pydantic).
_OPTIONAL_PROBLEM_FIELD_CHECKS: tuple[tuple[str, Callable[[Any], bool]], ...] = (
    ("requestId", _not_a_string),
    ("request_id", _not_a_string),
    ("existingDocumentId", _malformed_uuid),
    ("existing_document_id", _malformed_uuid),
    ("failureDisposition", _not_a_string),
    ("failure_disposition", _not_a_string),
)


def _parse_problem(response: httpx.Response) -> ProblemDetails:
    try:
        raw: Any = response.json()
    except ValueError:
        raw = {}
    if not isinstance(raw, Mapping):
        raw = {}
    body = _sanitize_problem_body(raw, response)
    try:
        return ProblemDetails.model_validate(body)
    except ValidationError:
        # Belt and braces: nothing in a problem body may escape as a raw
        # pydantic ValidationError, which is not an IronRagError and would
        # bypass every caller's typed catch. The sanitizer above covers every
        # declared field; this preserves the HTTP status if a future field is
        # added without one.
        return ProblemDetails(
            type="urn:ironrag:error:unknown",
            title="Error",
            status=response.status_code,
            detail=response.text[:400] or response.reason_phrase,
            code="unknown",
        )


def _raise_for_problem(response: httpx.Response, *, content_admission: bool = False) -> None:
    problem = _parse_problem(response)
    # The synchronous counterpart of the operation disposition. Only a caller
    # that actually submitted content for admission (create_document /
    # create_revision) may turn an HTTP 400 carrying the explicit server proof
    # into a content skip. A read, delete or operation poll that happens to see
    # the same field -- or a malformed server that set it on any other status --
    # stays a normal problem error, so the decision needs the caller's
    # admission flag AND the status/field agreement. No code-name allowlist.
    if (
        content_admission
        and response.status_code == 400
        and problem.failure_disposition == OperationFailureDisposition.CONTENT_UNSUPPORTED
    ):
        raise IronRagContentUnsupportedProblemError(problem)
    if response.status_code == 404:
        raise IronRagNotFoundError(problem)
    if response.status_code == 409 and problem.code == "duplicate_content":
        raise IronRagDuplicateContentError(problem)
    if response.status_code == 409 and problem.code in {
        "visibility_intent_required",
        "visibility_intent_superseded",
        "visibility_binding_conflict",
    }:
        raise IronRagVisibilityConflictError(problem)
    if response.status_code == 409:
        raise IronRagConflictError(problem)
    raise IronRagProblemError(problem)


def _parse_document_page(response: httpx.Response) -> DocumentPage:
    """Parse a 2xx collection page without treating malformed data as empty."""
    try:
        return DocumentPage.model_validate(response.json())
    except ValueError as exc:
        raise IronRagError("IronRAG returned a malformed document collection page") from exc


def _next_document_cursor(page: DocumentPage, seen: set[str]) -> str | None:
    """Accept only a null terminator or a previously unseen non-empty cursor."""
    cursor = page.next_cursor
    if cursor is None:
        return None
    if not cursor:
        raise IronRagError("IronRAG returned an empty nonterminal pagination cursor")
    if cursor in seen:
        raise IronRagError(f"IronRAG returned repeated pagination cursor {cursor!r}")
    seen.add(cursor)
    return cursor


def _extract_document_payload(payload: Mapping[str, Any]) -> Mapping[str, Any]:
    """Unwrap the ``{"document": ...}`` envelope used by create/get responses.

    Some server responses nest a canonical document object a second level
    deep (detail responses wrap the storage row inside a richer envelope).
    Handle one level of that nesting by merging the outer scalar fields
    (which may carry ``externalKey``/``libraryId`` the inner row omits)
    under the inner object.
    """
    doc = payload.get("document", payload)
    if isinstance(doc, Mapping) and isinstance(doc.get("document"), Mapping):
        inner = doc["document"]
        merged = {**doc, **inner}
        head = doc.get("head")
        if isinstance(head, Mapping):
            for field in ("visibilityIntentId", "readableMutationItemId", "readableRevisionId"):
                merged[field] = head.get(field)
        merged.pop("document", None)
        return merged
    if isinstance(doc, Mapping) and isinstance(payload.get("head"), Mapping):
        return {
            **doc,
            **{
                field: payload["head"].get(field)
                for field in ("visibilityIntentId", "readableMutationItemId", "readableRevisionId")
            },
        }
    return doc if isinstance(doc, Mapping) else payload


def _operation_id_from_response(body: Mapping[str, Any]) -> UUID:
    """Extract the operation id to poll from a mutation-admission body.

    ``ContentMutationDetailResponse`` carries a top-level ``asyncOperationId``
    on every mutation admission this client issues -- revisions (202
    Accepted) and deletes alike. Deliberately does NOT key off the
    ``Location`` header: create-revision sets it (202 + Location to
    ``/v1/ops/operations/{operationId}``) but delete-document does not (its
    handler returns a plain 200 with the operation id only in the body,
    despite carrying the same async-admission semantics) -- reading the
    typed body field uniformly covers both without the caller needing to
    know which status/header combination a given mutation kind uses.
    """
    raw = body.get("asyncOperationId")
    if raw is None:
        raise IronRagError("IronRAG accepted the mutation but returned no operation id")
    return UUID(str(raw))


def _optional_uuid(raw: Any) -> UUID | None:
    return None if raw is None else UUID(str(raw))


def _receipt_from_response(body: Mapping[str, Any]) -> dict[str, Any]:
    """Typed admission receipt fields from ``ContentMutationDetailResponse``.

    Missing keys stay ``None`` so a REQUIRED caller can fail closed on an older
    server instead of silently assuming readability.
    """
    items = body.get("items", [])
    if not isinstance(items, list) or len(items) > 1:
        raise IronRagError("IronRAG single-document admission returned invalid mutation items")
    item = items[0] if items else {}
    if not isinstance(item, Mapping):
        raise IronRagError("IronRAG single-document admission returned an invalid mutation item")
    return {
        "visibility_intent_id": _optional_uuid(item.get("visibilityIntentId")),
        "mutation_item_id": _optional_uuid(item.get("id")),
        "document_id": _optional_uuid(item.get("documentId")),
        "result_revision_id": _optional_uuid(body.get("resultRevisionId")),
        "result_checksum": body.get("resultChecksum"),
        "request_fingerprint": body.get("requestFingerprint"),
    }


class WalkCheckpointStore(Protocol):
    """Injectable checkpoint persistence for :meth:`IronRagClient.walk_all_documents`.

    The connector's own local state store (SQLite cursor) is the intended
    implementation -- this client has no persistence of its own.
    """

    def load_cursor(self) -> str | None: ...

    def save_cursor(self, cursor: str | None) -> None: ...


class IronRagClient:
    def __init__(
        self,
        settings: BaseConnectorSettings,
        client: httpx.AsyncClient | None = None,
    ) -> None:
        self._settings = settings
        self._owns_client = client is None
        self._visibility_contract_verified = False
        self._default_poll_interval = settings.operation_poll_interval_seconds
        self._default_poll_budget = settings.operation_poll_budget_seconds
        self._client = client or httpx.AsyncClient(
            base_url=settings.ironrag_base_url.rstrip("/"),
            timeout=settings.request_timeout_seconds,
            headers={
                "Authorization": f"Bearer {settings.ironrag_api_token}",
                "Accept": "application/json",
            },
        )

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()

    async def __aenter__(self) -> IronRagClient:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()

    async def ensure_visibility_contract(self) -> None:
        """Refuse unsupported backends before any visibility write.

        Synchronization must also call this before asking the source to enumerate.
        Only a successfully verified capability is cached for this client session.
        """
        if self._visibility_contract_verified:
            return
        response = await self._client.get("/v1/version")
        if response.status_code >= 400:
            _raise_for_problem(response)
        try:
            body = response.json()
        except ValueError as exc:
            raise IronRagError("IronRAG version response is not valid JSON") from exc
        if (
            not isinstance(body, dict)
            or body.get("contentVisibilityContract") != "visibility-intent/1"
        ):
            raise IronRagError("IronRAG requires contentVisibilityContract visibility-intent/1")
        self._visibility_contract_verified = True

    async def create_visibility_intent(
        self,
        document_id: UUID | str,
        *,
        request: VisibilityIntentRequest,
    ) -> VisibilityIntentReceipt:
        """Accept or replay one durable complete denial request, without polling.

        A transport failure provides no acceptance evidence. Replay the same
        persisted request, including its key, to recover the original floor.
        """
        target = UUID(str(document_id))
        await self.ensure_visibility_contract()
        response = await self._client.post(
            f"/v1/content/documents/{target}/visibility-intents",
            json=request.model_dump(mode="json", by_alias=True),
        )
        if response.status_code >= 400:
            _raise_for_problem(response)
        if response.status_code not in (200, 201):
            raise IronRagError("IronRAG visibility intent returned unexpected acceptance status")
        try:
            receipt = VisibilityIntentReceipt.model_validate(response.json())
        except (ValueError, ValidationError) as exc:
            raise IronRagError("IronRAG visibility intent returned an invalid receipt") from exc
        receipt.validate_binding(target, request)
        return receipt

    # -- catalog -----------------------------------------------------------

    async def resolve_library_refs(
        self,
        library_refs: set[str],
    ) -> dict[str, ResolvedLibraryTarget]:
        """Resolve canonical ``workspace/library`` refs to internal UUIDs.

        IronRAG catalog list endpoints are permission-filtered, so a missing
        ref is deliberately indistinguishable from a ref the caller cannot
        discover. Resolution happens once for the complete routing snapshot;
        document mutations continue using UUIDs internally.
        """
        normalized_refs = {normalize_library_ref(ref) for ref in library_refs}
        if normalized_refs != library_refs:
            raise IronRagCatalogError("library refs must already be canonical")
        if not normalized_refs:
            return {}

        workspaces_payload = await self._catalog_array(
            "/v1/catalog/workspaces",
            resource="workspaces",
        )
        workspaces = _index_catalog_rows(workspaces_payload, resource="workspace")

        refs_by_workspace: dict[str, set[str]] = {}
        for library_ref in normalized_refs:
            workspace_slug, _ = library_ref.split("/", 1)
            refs_by_workspace.setdefault(workspace_slug, set()).add(library_ref)

        result: dict[str, ResolvedLibraryTarget] = {}
        for workspace_slug in sorted(refs_by_workspace):
            workspace = workspaces.get(workspace_slug)
            if workspace is None:
                refs = ", ".join(sorted(refs_by_workspace[workspace_slug]))
                raise IronRagCatalogError(
                    f"IronRAG workspace slug '{workspace_slug}' is not visible "
                    f"for library ref(s): {refs}"
                )
            workspace_id = _catalog_uuid(workspace, "id", "workspace", workspace_slug)

            for library_ref in sorted(refs_by_workspace[workspace_slug]):
                _, library_slug = library_ref.split("/", 1)
                library = await self._find_library_row(
                    workspace_id=workspace_id,
                    workspace_slug=workspace_slug,
                    library_slug=library_slug,
                    library_ref=library_ref,
                )
                row_workspace_id = _catalog_uuid(
                    library,
                    "workspaceId",
                    "library",
                    library_ref,
                )
                if row_workspace_id != workspace_id:
                    raise IronRagCatalogError(
                        f"IronRAG library ref '{library_ref}' returned workspaceId "
                        f"{row_workspace_id}, expected {workspace_id}"
                    )
                result[library_ref] = ResolvedLibraryTarget(
                    library_ref=library_ref,
                    workspace_id=workspace_id,
                    library_id=_catalog_uuid(library, "id", "library", library_ref),
                )
        return result

    async def _find_library_row(
        self,
        *,
        workspace_id: UUID,
        workspace_slug: str,
        library_slug: str,
        library_ref: str,
    ) -> Mapping[str, Any]:
        """Resolve one library slug through the paginated catalog page.

        The endpoint's ``search`` filter is a case-insensitive substring
        match over ``slug`` and ``displayName``, so searching for the exact
        slug is guaranteed to include the wanted row if it is visible -- but
        it also returns every row that merely contains the slug, and it
        returns them a bounded page at a time. Both facts are load-bearing:
        the exact-slug comparison below rejects the substring neighbours, and
        the walk keeps requesting pages until the whole filtered result has
        been seen. A workspace with thousands of libraries would otherwise
        report a perfectly healthy ref as invisible.

        The walk stops only on ``totalCount``, never on a short page: the
        server clamps ``limit``, so "fewer rows than requested" says nothing
        about exhaustion. If the server stops handing out new rows while it
        still claims more, that is raised with the counts rather than
        silently downgraded to "not visible".
        """
        path = f"/v1/catalog/workspaces/{workspace_id}/libraries"
        resource = f"libraries in workspace '{workspace_slug}'"
        seen_ids: set[str] = set()
        received = 0
        total = 0
        while True:
            page = await self._catalog_page(
                path,
                resource=resource,
                params={
                    "search": library_slug,
                    "offset": received,
                    "limit": CATALOG_LIBRARY_PAGE_LIMIT,
                },
            )
            total = page.total_count
            for row in page.items:
                if row.get("slug") == library_slug:
                    return row
            received += len(page.items)
            if received >= total:
                break
            # Count distinct rows so a server that ignores `offset` cannot
            # inflate the reported figure, and stop as soon as a page adds
            # nothing new -- completeness is no longer reachable by asking.
            before = len(seen_ids)
            seen_ids.update(str(row.get("id")) for row in page.items)
            if len(seen_ids) == before:
                raise IronRagCatalogError(
                    f"IronRAG catalog {resource} returned an incomplete page walk for "
                    f"'{library_slug}': scanned {len(seen_ids)} of {total} matching rows "
                    f"before the server stopped returning new rows"
                )
        raise IronRagCatalogError(
            f"IronRAG library ref '{library_ref}' is not visible "
            f"(scanned {received} of {total} catalog rows matching '{library_slug}' "
            f"in workspace '{workspace_slug}')"
        )

    async def _catalog_array(
        self,
        path: str,
        *,
        resource: str,
    ) -> list[Mapping[str, Any]]:
        """Read a catalog endpoint whose response is a bare JSON array.

        ``GET /v1/catalog/workspaces`` is unpaginated and returns the visible
        workspaces directly. Do not point this at a paginated endpoint --
        the page shape has its own reader below.
        """
        payload = await self._catalog_payload(path, resource=resource)
        if not isinstance(payload, list) or not all(isinstance(item, Mapping) for item in payload):
            raise IronRagCatalogError(
                f"IronRAG catalog {resource} must return a JSON array of objects"
            )
        return payload

    async def _catalog_page(
        self,
        path: str,
        *,
        resource: str,
        params: Mapping[str, Any],
    ) -> _CatalogPage:
        """Read a catalog endpoint whose response is a bounded page envelope.

        ``GET /v1/catalog/workspaces/{workspaceId}/libraries`` answers with
        ``{items, totalCount, offset, limit}``. Every field is validated
        rather than defaulted: a missing ``items`` defaulted to ``[]`` or a
        non-numeric ``totalCount`` coerced to ``0`` would turn a malformed
        response into a confident "not visible".
        """
        payload = await self._catalog_payload(path, resource=resource, params=params)
        if not isinstance(payload, Mapping):
            raise IronRagCatalogError(f"IronRAG catalog {resource} must return a JSON page object")
        items = payload.get("items")
        if not isinstance(items, list) or not all(isinstance(item, Mapping) for item in items):
            raise IronRagCatalogError(
                f"IronRAG catalog {resource} page must carry an 'items' array of objects"
            )
        total_count = payload.get("totalCount")
        if not isinstance(total_count, int) or isinstance(total_count, bool) or total_count < 0:
            raise IronRagCatalogError(
                f"IronRAG catalog {resource} page must carry a non-negative integer 'totalCount'"
            )
        return _CatalogPage(items=tuple(items), total_count=total_count)

    async def _catalog_payload(
        self,
        path: str,
        *,
        resource: str,
        params: Mapping[str, Any] | None = None,
    ) -> Any:
        response = await self._client.get(path, params=dict(params) if params else None)
        if response.status_code >= 400:
            raise IronRagCatalogError(
                f"IronRAG catalog {resource} -> {response.status_code}: {response.text[:400]}"
            )
        try:
            return response.json()
        except ValueError as exc:
            raise IronRagCatalogError(f"IronRAG catalog {resource} returned invalid JSON") from exc

    # -- documents -----------------------------------------------------------

    async def find_document(self, library_id: UUID, external_key: str) -> DocumentResource | None:
        """Resolve a document by its exact external key.

        There is no dedicated exact-match filter on the list endpoint --
        only a ``search`` substring/ILIKE filter on ``external_key`` or the
        rendered filename
        (plan S7.1 describes an exact filter as an aspiration; the landed
        API does not implement one). This narrows server-side via
        ``search=external_key`` and returns the first page item whose
        ``external_key`` equals the requested key exactly, which is still
        a single bounded request rather than the old substring-ILIKE +
        client filter + full-scan-fallback dance.

        A library that no longer exists is treated the same as "no match"
        here -- ``list_documents`` itself must not blur that distinction,
        since callers like the reap sweep (sync.py) need the real signal to
        self-heal a stale cursor row.
        """
        try:
            async for document in self.list_documents(
                library_id, external_key=external_key, limit=50
            ):
                return document
        except IronRagNotFoundError:
            return None
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
        """Walk every page of the library's document collection.

        The only read-collection interface (plan S7.1) -- cursor pagination
        to ``nextCursor is None``, never a ``total``-driven offset fallback.

        ``external_key`` is not a server-side parameter (see
        :meth:`find_document`): when set, it is sent as ``search`` to
        narrow server-side, and every yielded item is additionally
        filtered to an exact ``external_key`` match client-side, so this
        behaves as an exact-key list (normally 0 or 1 result) rather than
        a substring search. Passing both ``search`` and ``external_key``
        is not supported -- they would need conflicting narrowing.
        """
        if search and external_key:
            raise ValueError("list_documents accepts either search or external_key, not both")
        server_search = external_key or search
        cursor: str | None = None
        seen_cursors: set[str] = set()
        while True:
            params: dict[str, Any] = {"limit": limit}
            if server_search:
                params["search"] = server_search
            if status:
                params["status"] = ",".join(status)
            if include_deleted:
                params["includeDeleted"] = "true"
            if cursor:
                params["cursor"] = cursor

            response = await self._client.get(
                f"/v1/content/libraries/{library_id}/documents", params=params
            )
            if response.status_code >= 400:
                _raise_for_problem(response)
            page = _parse_document_page(response)
            for item in page.items:
                _validate_collection_document_library(item, library_id)
                if external_key and item.external_key != external_key:
                    continue
                yield item
            cursor = _next_document_cursor(page, seen_cursors)
            if cursor is None:
                return

    async def walk_all_documents(
        self,
        library_id: UUID,
        *,
        concurrency: int = DEFAULT_REWALK_CONCURRENCY,
        resume_from_checkpoint: bool = True,
        checkpoint_store: WalkCheckpointStore | None = None,
    ) -> AsyncIterator[DocumentResource]:
        """Checkpointed full walk of every document in ``library_id``.

        List pagination is inherently sequential (page N+1 needs page N's
        cursor), so ``concurrency`` does not parallelize the walk itself --
        it is the fan-out bound the caller should apply when it issues
        create/revision calls in response to each yielded item (the same
        semaphore pattern :class:`~ironrag_connector.sync.SyncManager`
        already uses for a sweep). ``checkpoint_store`` persists the
        in-flight cursor after every page so an interrupted or
        rate-limited re-walk resumes instead of restarting (plan S7.6).
        """
        if concurrency < 1:
            raise ValueError("concurrency must be >= 1")
        cursor = (
            checkpoint_store.load_cursor()
            if resume_from_checkpoint and checkpoint_store is not None
            else None
        )
        seen_cursors = {cursor} if cursor is not None else set()
        limit = 200
        while True:
            params: dict[str, Any] = {"limit": limit}
            if cursor:
                params["cursor"] = cursor
            response = await self._client.get(
                f"/v1/content/libraries/{library_id}/documents", params=params
            )
            if response.status_code >= 400:
                _raise_for_problem(response)
            page = _parse_document_page(response)
            for item in page.items:
                _validate_collection_document_library(item, library_id)
                yield item
            cursor = _next_document_cursor(page, seen_cursors)
            if checkpoint_store is not None:
                checkpoint_store.save_cursor(cursor)
            if cursor is None:
                return

    async def get_document(self, document_id: UUID | str) -> DocumentResource | None:
        response = await self._client.get(f"/v1/content/documents/{document_id}")
        if response.status_code == 404:
            return None
        if response.status_code >= 400:
            _raise_for_problem(response)
        payload = response.json()
        return DocumentResource.model_validate(_extract_document_payload(payload))

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
        """Create a document. Content-negotiated: passing ``file_bytes``
        sends a multipart upload; omitting it sends a JSON metadata-only
        create. One creation door, one HTTP path (plan S7.1/S7.2).

        No ``idempotency_key`` parameter: create dedup is the server's
        exact ``externalKey`` uniqueness check plus the typed
        :class:`IronRagDuplicateContentError` 409, not an idempotency key
        (plan S7.1) -- content-hash idempotency keys apply to revisions and
        deletes only.

        Raises :class:`IronRagDuplicateContentError` on a 409 conflict. The
        typed error exposes the server's ``existing_document_id`` only as
        diagnostic evidence; callers must not adopt it as canonical ownership
        without an exact external-key proof.
        """
        await self.ensure_visibility_contract()
        log.info("ironrag.create_document.start", external_key=external_key)
        if file_bytes is not None:
            if not file_name or not mime_type:
                raise ValueError("file_name and mime_type are required when file_bytes is set")
            files = {"file": (file_name, file_bytes, mime_type)}
            # The library is already identified by the path segment. The
            # server's multipart parser now rejects any field outside its
            # allow-list with 400 "unknown multipart field", so a redundant
            # `library_id` field makes every upload fail. It is deliberately
            # not sent: the path parameter is the single source of truth.
            data: dict[str, Any] = {
                "external_key": external_key,
            }
            if title:
                data["title"] = title
            if document_hint is not None:
                data["document_hint"] = document_hint
            if parent_external_key is not None:
                data["parent_external_key"] = parent_external_key
            response = await self._client.post(
                f"/v1/content/libraries/{library_id}/documents",
                data=data,
                files=files,
            )
        else:
            json_body: dict[str, Any] = {"externalKey": external_key}
            if title:
                json_body["title"] = title
            if document_hint is not None:
                json_body["documentHint"] = document_hint
            if parent_external_key is not None:
                json_body["parentExternalKey"] = parent_external_key
            response = await self._client.post(
                f"/v1/content/libraries/{library_id}/documents",
                json=json_body,
            )

        if response.status_code >= 400:
            _raise_for_problem(response, content_admission=True)
        payload = response.json()
        document = DocumentResource.model_validate(_extract_document_payload(payload))
        mutation = payload.get("mutation")
        if isinstance(mutation, Mapping) and mutation.get("asyncOperationId") is not None:
            document.admission = OperationHandle(
                operation_id=_operation_id_from_response(mutation),
                **_receipt_from_response(mutation),
            )
        log.info(
            "ironrag.create_document.created",
            external_key=external_key,
            document_id=str(document.id),
        )
        return document

    async def create_revision(
        self,
        document_id: UUID | str,
        *,
        mode: Literal["append", "replace"],
        markdown: str | None = None,
        appended_text: str | None = None,
        file_bytes: bytes | None = None,
        file_name: str | None = None,
        mime_type: str | None = None,
        idempotency_key: str,
        visibility_intent_id: UUID | str | None = None,
        base_revision_id: UUID | str | None = None,
        title: str | None = None,
        provenance: RevisionProvenance = _EMPTY_REVISION_PROVENANCE,
        document_hint: str | None = None,
    ) -> OperationHandle:
        """Create a new revision. Content-negotiated, mirroring
        :meth:`create_document`: ``file_bytes`` sends a multipart file
        replace; otherwise a JSON ``{mode, appendedText|markdown}`` body.
        Always asynchronous -- returns an :class:`OperationHandle` to poll
        via :meth:`wait_for_operation`.
        """
        if mode == "append" and base_revision_id is None:
            raise ValueError("base_revision_id is required when mode='append'")
        if file_bytes is not None and mode != "replace":
            raise ValueError("file replacement requires mode='replace'")
        await self.ensure_visibility_contract()
        metadata = {
            "title": title,
            "language_code": provenance.language_code,
            "source_uri": provenance.source_uri,
            "document_hint": document_hint,
        }
        log.info(
            "ironrag.create_revision.start",
            document_id=str(document_id),
            mode=mode,
        )
        if file_bytes is not None:
            if not file_name or not mime_type:
                raise ValueError("file_name and mime_type are required when file_bytes is set")
            files = {"file": (file_name, file_bytes, mime_type)}
            data = {"idempotency_key": idempotency_key}
            data.update({key: value for key, value in metadata.items() if value is not None})
            if visibility_intent_id is not None:
                data["visibility_intent_id"] = str(UUID(str(visibility_intent_id)))
            response = await self._client.post(
                f"/v1/content/documents/{document_id}/revisions",
                data=data,
                files=files,
                timeout=self._settings.revision_create_timeout_seconds,
            )
        else:
            json_body: dict[str, Any] = {"mode": mode, "idempotencyKey": idempotency_key}
            if mode == "append":
                if appended_text is None:
                    raise ValueError("appended_text is required when mode='append'")
                json_body["appendedText"] = appended_text
                json_body["baseRevisionId"] = str(UUID(str(base_revision_id)))
            else:
                if markdown is None:
                    raise ValueError("markdown is required when mode='replace' without file_bytes")
                json_body["markdown"] = markdown
                json_body.update(
                    {
                        "title": title,
                        "languageCode": provenance.language_code,
                        "sourceUri": provenance.source_uri,
                        "documentHint": document_hint,
                        "fileName": file_name,
                        "mimeType": mime_type,
                    }
                )
            if visibility_intent_id is not None:
                json_body["visibilityIntentId"] = str(UUID(str(visibility_intent_id)))
            response = await self._client.post(
                f"/v1/content/documents/{document_id}/revisions",
                json=json_body,
                timeout=self._settings.revision_create_timeout_seconds,
            )

        if response.status_code >= 400:
            _raise_for_problem(response, content_admission=True)
        body = response.json() if response.content else {}
        operation_id = _operation_id_from_response(body)
        log.info(
            "ironrag.create_revision.accepted",
            document_id=str(document_id),
            operation_id=str(operation_id),
        )
        handle = OperationHandle(operation_id=operation_id, **_receipt_from_response(body))
        if handle.document_id is not None and handle.document_id != UUID(str(document_id)):
            raise IronRagError("IronRAG revision receipt returned mismatched document identity")
        return handle

    async def delete_document(
        self, document_id: UUID | str, *, idempotency_key: str
    ) -> OperationHandle | None:
        """Delete a document. Returns ``None`` when IronRAG already has no
        such document (idempotent no-op); otherwise an
        :class:`OperationHandle` to poll to terminal.

        Deletes are asynchronous like revisions in effect (an
        ``ops_async_operation`` row is always created and must be polled to
        terminal), but NOT in HTTP shape: the handler returns a plain 200
        with the operation id only in the JSON body, not 202 + Location like
        create-revision. Both are handled uniformly via the typed body field
        (see :func:`_operation_id_from_response`), so this difference is
        invisible to callers of this method.
        """
        await self.ensure_visibility_contract()
        log.info("ironrag.delete_document.start", document_id=str(document_id))
        response = await self._client.request(
            "DELETE",
            f"/v1/content/documents/{document_id}",
            headers={"Idempotency-Key": idempotency_key},
        )
        if response.status_code == 404:
            return None
        if response.status_code >= 400:
            _raise_for_problem(response)
        body = response.json() if response.content else {}
        operation_id = _operation_id_from_response(body)
        log.info(
            "ironrag.delete_document.accepted",
            document_id=str(document_id),
            operation_id=str(operation_id),
        )
        return OperationHandle(operation_id=operation_id)

    # -- async operations -----------------------------------------------------

    async def get_operation(self, operation_id: UUID | str) -> OperationStatus:
        response = await self._client.get(f"/v1/ops/operations/{operation_id}")
        if response.status_code >= 400:
            _raise_for_problem(response)
        return OperationStatus.model_validate(response.json())

    async def wait_for_operation(
        self,
        operation_id: UUID | str,
        *,
        poll_interval: float | None = None,
        budget: float | None = None,
    ) -> OperationStatus:
        """Poll ``GET /v1/ops/operations/{operationId}`` to a terminal state.

        The one poll-to-terminal primitive every mutating call funnels
        through (plan S7.2/S7.3). Terminal states:

        * ``ready`` -- returned normally.
        * ``superseded`` / ``canceled`` -- returned normally; the caller
          decides how to react (a later write won, or the operation was
          explicitly canceled -- neither is a client-side failure).
        * ``failed`` -- raises :class:`IronRagOperationFailedError`, or
          :class:`IronRagContentUnsupportedError` when the operation carries
          the server-proven ``content_unsupported`` disposition
          (:func:`operation_failure_exception` is the one mapping).

        Raises :class:`IronRagMutationTimeoutError` if no terminal state is
        reached inside ``budget`` seconds.
        """
        resolved_interval = (
            poll_interval if poll_interval is not None else self._default_poll_interval
        )
        resolved_budget = budget if budget is not None else self._default_poll_budget
        deadline = time.monotonic() + resolved_budget
        while True:
            status = await self.get_operation(operation_id)
            if status.id != UUID(str(operation_id)):
                raise IronRagError("operation poll returned mismatched operation identity")
            if status.status.is_terminal:
                if status.status is OperationStatusValue.FAILED:
                    raise operation_failure_exception(status)
                return status
            if time.monotonic() >= deadline:
                raise IronRagMutationTimeoutError(
                    f"operation {operation_id} did not reach a terminal state "
                    f"within {resolved_budget:.1f}s"
                )
            await asyncio.sleep(resolved_interval)


def _index_catalog_rows(
    rows: Sequence[Mapping[str, Any]],
    *,
    resource: str,
) -> dict[str, Mapping[str, Any]]:
    indexed: dict[str, Mapping[str, Any]] = {}
    for row in rows:
        slug = row.get("slug")
        if not isinstance(slug, str) or not slug:
            raise IronRagCatalogError(f"IronRAG catalog {resource} row has an invalid slug")
        if slug in indexed:
            raise IronRagCatalogError(
                f"IronRAG catalog returned duplicate {resource} slug '{slug}'"
            )
        indexed[slug] = row
    return indexed


def _catalog_uuid(
    row: Mapping[str, Any],
    key: str,
    resource: str,
    catalog_ref: str,
) -> UUID:
    raw = row.get(key)
    try:
        return UUID(str(raw))
    except (TypeError, ValueError, AttributeError) as exc:
        raise IronRagCatalogError(
            f"IronRAG {resource} '{catalog_ref}' returned invalid {key}"
        ) from exc
