from __future__ import annotations

import json
from pathlib import Path
from uuid import UUID

import httpx
import pytest
from test_sdk_ironrag_client import _client
from test_sdk_state import _observe_materialization

from ironrag_connector.ironrag import IronRagError, VisibilityIntentReceipt
from ironrag_connector.state import PayloadIdentity, StateStore


def _submitting(store: StateStore, bound: bool):
    observation = _observe_materialization(store, library_id=str(UUID(int=2)))
    if not bound:
        observation = _observe_materialization(
            store,
            library_id=str(UUID(int=2)),
            payload_identity=PayloadIdentity(
                document_id=UUID(int=3), operation="replace", replacement=observation.replacement
            ),
        )
    attempt = store.prepare_materialization_attempt(
        observation,
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=2)),
        intent_required=bound,
    )
    if bound:
        attempt = store.transition_materialization_attempt(attempt, phase="intent_submitting")
        receipt = VisibilityIntentReceipt(
            visibility_intent_id=UUID(int=9),
            mutation_item_id=UUID(int=9),
            mutation_id=UUID(int=10),
            async_operation_id=UUID(int=11),
            document_id=UUID(int=3),
            revision_floor=1,
            source_identity=attempt.request.source_identity,
            materialization_token=attempt.request.materialization_token,
            replacement=attempt.request.replacement,
            request_fingerprint="opaque-intent",
        )
        attempt = store.transition_materialization_attempt(
            attempt, phase="intent_accepted", receipt=receipt
        )
        attempt = store.transition_materialization_attempt(attempt, phase="payload_submitting")
    return attempt


@pytest.mark.asyncio
@pytest.mark.parametrize("bound", [False, True])
async def test_native_receipt_persists_before_poll_and_reopens(
    native_content_receipts: dict[str, bytes], tmp_path: Path, bound: bool
) -> None:
    body = native_content_receipts["bound" if bound else "ordinary"]
    paths: list[str] = []

    def transport(request: httpx.Request) -> httpx.Response:
        paths.append(request.url.path)
        return httpx.Response(202, content=body, headers={"content-type": "application/json"})

    client = _client(httpx.MockTransport(transport))
    path = tmp_path / "receipt.sqlite"
    store = StateStore(path)
    attempt = _submitting(store, bound)
    handle = await client.create_revision(
        UUID(int=3),
        mode="replace",
        markdown="safe",
        idempotency_key=attempt.payload_key,
        visibility_intent_id=UUID(int=9) if bound else None,
    )
    accepted = store.record_payload_receipt(attempt, handle)
    assert accepted.payload_receipt == handle
    assert handle.document_id == UUID(int=3)
    assert handle.mutation_item_id == UUID(int=5)
    assert handle.result_revision_id == UUID(int=6)
    assert handle.visibility_intent_id == (UUID(int=9) if bound else None)
    assert store.get("page", "1") is None
    assert paths == [f"/v1/content/documents/{UUID(int=3)}/revisions"]
    store.close()
    reopened = StateStore(path)
    assert reopened.materialization_attempts_for_document(str(UUID(int=3))) == [accepted]
    reopened.close()
    await client.aclose()


@pytest.mark.asyncio
async def test_native_nonpayload_admission_has_explicit_unbound_identity(
    native_content_receipts: dict[str, bytes],
) -> None:
    wire = native_content_receipts["metadata_create"]
    assert json.loads(wire)["mutation"]["visibilityIntentId"] is None
    client = _client(httpx.MockTransport(lambda request: httpx.Response(201, content=wire)))
    document = await client.create_document(UUID(int=2), external_key="native:page:1")
    assert document.admission is None
    assert document.readable_revision_id is None
    await client.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "defect",
    [
        "missing_identity",
        "bad_identity",
        "wrong_identity",
        "wrong_document",
        "wrong_revision",
        "wrong_checksum",
        "missing_fingerprint",
        "empty_items",
        "multiple_items",
        "bad_item_id",
        "bad_operation_id",
    ],
)
async def test_native_receipt_boundary_rejects_corrupt_lineage_without_cursor_advance(
    native_content_receipts: dict[str, bytes], tmp_path: Path, defect: str
) -> None:
    body = json.loads(native_content_receipts["ordinary"])
    if defect == "missing_identity":
        del body["visibilityIntentId"]
    elif defect == "bad_identity":
        body["visibilityIntentId"] = "not-a-uuid"
    elif defect == "wrong_identity":
        body["visibilityIntentId"] = str(UUID(int=19))
    elif defect == "wrong_document":
        body["items"][0]["document_id"] = str(UUID(int=19))
    elif defect == "wrong_revision":
        body["items"][0]["result_revision_id"] = str(UUID(int=19))
    elif defect == "wrong_checksum":
        body["resultChecksum"] = "sha256:" + "b" * 64
    elif defect == "missing_fingerprint":
        del body["requestFingerprint"]
    elif defect == "empty_items":
        body["items"] = []
    elif defect == "multiple_items":
        body["items"] *= 2
    elif defect == "bad_item_id":
        body["items"][0]["id"] = "not-a-uuid"
    elif defect == "bad_operation_id":
        body["asyncOperationId"] = "not-a-uuid"
    client = _client(httpx.MockTransport(lambda request: httpx.Response(202, json=body)))
    store = StateStore(tmp_path / "invalid.sqlite")
    attempt = _submitting(store, False)
    with pytest.raises((IronRagError, ValueError)):
        handle = await client.create_revision(
            UUID(int=3), mode="replace", markdown="safe", idempotency_key=attempt.payload_key
        )
        store.record_payload_receipt(attempt, handle)
    assert store.get("page", "1") is None
    assert store.materialization_attempts_for_document(str(UUID(int=3))) == [attempt]
    store.close()
    await client.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize("name", ["published", "suppressed"])
async def test_native_head_currentness_has_no_mutation_item_authority(
    native_content_receipts: dict[str, bytes], name: str
) -> None:
    client = _client(
        httpx.MockTransport(
            lambda request: httpx.Response(200, content=native_content_receipts[name])
        )
    )
    document = await client.get_document(UUID(int=3))
    assert document.id == UUID(int=3)
    assert document.visibility_intent_id == UUID(int=9)
    assert document.readable_revision_id == (UUID(int=6) if name == "published" else None)
    await client.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "defect", ["wrong_document", "missing_intent", "missing_revision", "bad_uuid"]
)
async def test_native_head_rejects_incomplete_or_foreign_identity(
    native_content_receipts: dict[str, bytes], defect: str
) -> None:
    body = json.loads(native_content_receipts["published"])
    if defect == "wrong_document":
        body["head"]["document_id"] = str(UUID(int=19))
    elif defect == "missing_intent":
        del body["head"]["visibility_intent_id"]
    elif defect == "missing_revision":
        del body["head"]["readable_revision_id"]
    else:
        body["head"]["readable_revision_id"] = "not-a-uuid"
    client = _client(httpx.MockTransport(lambda request: httpx.Response(200, json=body)))
    with pytest.raises(IronRagError, match="head identity"):
        await client.get_document(UUID(int=3))
    await client.aclose()


@pytest.mark.asyncio
async def test_native_create_envelope_returns_complete_payload_receipt(
    native_content_receipts: dict[str, bytes],
) -> None:
    client = _client(
        httpx.MockTransport(
            lambda request: httpx.Response(201, content=native_content_receipts["create"])
        )
    )
    document = await client.create_document(
        UUID(int=2),
        external_key="native:page:1",
        file_bytes=b"safe",
        file_name="page.txt",
        mime_type="text/plain",
    )
    assert document.id == UUID(int=3)
    assert document.library_id == UUID(int=2)
    assert document.admission is not None
    assert document.admission.document_id == document.id
    assert document.admission.result_revision_id == UUID(int=6)
    assert document.admission.visibility_intent_id is None
    await client.aclose()
