from __future__ import annotations

from uuid import uuid4

from ironrag_connector.ironrag import OperationHandle


def test_admission_identifiers_survive_handle_roundtrip() -> None:
    handle = OperationHandle(
        operation_id=uuid4(),
        document_id=uuid4(),
        visibility_intent_id=uuid4(),
        mutation_item_id=uuid4(),
        mutation_id=uuid4(),
        job_id=uuid4(),
        result_revision_id=uuid4(),
        result_checksum="sha256:abc",
        request_fingerprint="opaque",
    )
    restored = OperationHandle.model_validate_json(handle.model_dump_json())
    assert restored == handle
    assert restored.mutation_id == handle.mutation_id
    assert restored.job_id == handle.job_id
