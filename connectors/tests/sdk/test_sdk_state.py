from __future__ import annotations

import sqlite3
import threading
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path
from uuid import UUID

import pytest

from ironrag_connector.ironrag import OperationHandle, ReplacementIdentity, VisibilityIntentReceipt
from ironrag_connector.state import MemberTarget, PayloadIdentity, StateStore


def _observe_materialization(store: StateStore, token: str | None = "A", **changes: object):
    descriptor = ReplacementIdentity(
        checksum="sha256:" + "a" * 64,
        byte_size=3,
        mime_type="text/plain",
        file_name="page.txt",
    )
    args = {
        "adapter": "echo",
        "kind": "page",
        "item_id": "1",
        "external_key": "echo:page:1",
        "library_id": str(UUID(int=1)),
        "materialization_token": token,
        "replacement": descriptor,
        "target_document_id": str(UUID(int=3)),
        **changes,
    }
    return store.observe_materialization(**args)


def test_visibility_contract_observation_cycles_and_retransmission(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "visibility.sqlite")
    first = _observe_materialization(store)
    assert _observe_materialization(store) == first
    second = _observe_materialization(store, "B")
    third = _observe_materialization(store)
    assert len({first.episode_id, second.episode_id, third.episode_id}) == 3
    changed_route = _observe_materialization(store, library_id=str(UUID(int=2)))
    assert changed_route.episode_id != third.episode_id
    changed_target = _observe_materialization(
        store, library_id=str(UUID(int=2)), target_document_id=str(UUID(int=7))
    )
    assert changed_target.episode_id != changed_route.episode_id
    null_token = _observe_materialization(store, None)
    assert _observe_materialization(store, None) == null_token
    store.close()


def test_visibility_contract_unknown_send_reopen_keeps_exact_request(tmp_path: Path) -> None:
    path = tmp_path / "visibility.sqlite"
    store = StateStore(path)
    observation = _observe_materialization(store)
    attempt = store.prepare_materialization_attempt(
        observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    submitting = store.transition_materialization_attempt(attempt, phase="intent_submitting")
    store.close()
    reopened = StateStore(path)
    restored_observation = _observe_materialization(reopened)
    restored = reopened.prepare_materialization_attempt(
        restored_observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    assert restored == submitting
    assert restored.receipt is None
    assert reopened.get("page", "1") is None
    reopened.close()


def test_visibility_contract_receipt_floor_and_payload_retry_are_independent(
    tmp_path: Path,
) -> None:
    store = StateStore(tmp_path / "visibility.sqlite")
    observation = _observe_materialization(store)
    attempt = store.prepare_materialization_attempt(
        observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    submitting = store.transition_materialization_attempt(attempt, phase="intent_submitting")
    receipt = VisibilityIntentReceipt(
        visibility_intent_id=UUID(int=4),
        mutation_item_id=UUID(int=4),
        mutation_id=UUID(int=5),
        async_operation_id=UUID(int=6),
        document_id=UUID(int=3),
        revision_floor=9,
        source_identity=attempt.request.source_identity,
        materialization_token=attempt.request.materialization_token,
        replacement=attempt.request.replacement,
        request_fingerprint="opaque-server-fingerprint",
    )
    accepted = store.transition_materialization_attempt(
        submitting, phase="intent_accepted", receipt=receipt
    )
    payload = store.transition_materialization_attempt(accepted, phase="payload_submitting")
    terminal = store.transition_materialization_attempt(payload, phase="payload_terminal")
    assert terminal.payload_key != payload.payload_key
    assert terminal.request == attempt.request
    assert terminal.receipt == receipt
    assert terminal.receipt.revision_floor == 9
    assert store.get("page", "1") is None
    changed_receipt = receipt.model_copy(update={"revision_floor": 10})
    with pytest.raises(ValueError, match="immutable"):
        store.transition_materialization_attempt(
            terminal,
            phase="payload_submitting",
            receipt=changed_receipt,
        )
    superseded = store.transition_materialization_attempt(terminal, phase="intent_superseded")
    assert _observe_materialization(store) == observation
    with pytest.raises(ValueError, match="phase"):
        store.transition_materialization_attempt(superseded, phase="intent_submitting")
    store.close()
    reopened = StateStore(tmp_path / "visibility.sqlite")
    assert reopened.materialization_attempts_for_document(str(UUID(int=3))) == [superseded]
    assert reopened.get("page", "1") is None
    reopened.close()


def test_visibility_contract_receipt_required_and_cross_session_cas(tmp_path: Path) -> None:
    path = tmp_path / "visibility.sqlite"
    first = StateStore(path)
    second = StateStore(path)
    observation = _observe_materialization(first)
    attempt = first.prepare_materialization_attempt(
        observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    submitting = second.transition_materialization_attempt(attempt, phase="intent_submitting")
    with pytest.raises(ValueError, match="concurrently"):
        first.transition_materialization_attempt(attempt, phase="intent_submitting")
    with pytest.raises(ValueError, match="receipt"):
        first.transition_materialization_attempt(submitting, phase="intent_accepted")
    first.close()
    second.close()


def test_visibility_contract_applied_cursor_cas_refuses_older_observation(tmp_path: Path) -> None:
    path = tmp_path / "visibility.sqlite"
    first = StateStore(path)
    second = StateStore(path)
    old = _observe_materialization(first, "A")
    current = _observe_materialization(second, "B", library_id=str(UUID(int=2)))
    document_id = str(UUID(int=3))
    library_id = str(UUID(int=1))
    with pytest.raises(ValueError, match="observation changed"):
        first.upsert(
            kind="page",
            item_id="1",
            change_token="A",
            external_key="echo:page:1",
            ironrag_document_id=document_id,
            ironrag_library_id=library_id,
            expected_visibility_episode=old.episode_id,
        )
    assert first.get("page", "1") is None
    second.upsert(
        kind="page",
        item_id="1",
        change_token="B",
        external_key="echo:page:1",
        ironrag_document_id=str(UUID(int=4)),
        ironrag_library_id=str(UUID(int=2)),
        expected_visibility_episode=current.episode_id,
    )
    assert first.get("page", "1").change_token == "B"
    first.close()
    second.close()


def test_materialization_format_upgrade_preserves_old_intent_request(tmp_path: Path) -> None:
    path = tmp_path / "upgrade.sqlite"
    store = StateStore(path)
    observation = _observe_materialization(store)
    attempt = store.prepare_materialization_attempt(
        observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    store.close()
    with sqlite3.connect(path) as conn:
        conn.executescript("""
            ALTER TABLE visibility_observation DROP COLUMN payload_identity_json;
            ALTER TABLE visibility_attempt RENAME TO saved_attempt;
            CREATE TABLE visibility_attempt (
                episode_id TEXT NOT NULL, document_id TEXT NOT NULL, library_id TEXT NOT NULL,
                phase TEXT NOT NULL, request_json TEXT NOT NULL, receipt_json TEXT,
                payload_key TEXT NOT NULL, payload_receipt_json TEXT,
                PRIMARY KEY (episode_id, document_id));
            INSERT INTO visibility_attempt SELECT episode_id, document_id, library_id, phase,
                request_json, receipt_json, payload_key, payload_receipt_json FROM saved_attempt;
            DROP TABLE saved_attempt;
        """)
    reopened = StateStore(path)
    assert reopened.materialization_attempts_for_document(attempt.document_id) == [attempt]
    assert _observe_materialization(reopened) == observation
    identity = PayloadIdentity(
        document_id=UUID(int=3), operation="replace", replacement=observation.replacement
    )
    ordinary_observation = _observe_materialization(reopened, "B", payload_identity=identity)
    ordinary = reopened.prepare_materialization_attempt(
        ordinary_observation,
        document_id=attempt.document_id,
        library_id=attempt.library_id,
        intent_required=False,
    )
    assert ordinary.request is None
    assert ordinary.receipt is None
    reopened.close()
    again = StateStore(path)
    assert again.materialization_attempts_for_document(attempt.document_id) == [attempt, ordinary]
    again.close()


@pytest.mark.parametrize(
    "changes",
    [
        {"replacement_field": "language_code", "value": "ru"},
        {"replacement_field": "source_uri", "value": "https://source.invalid"},
        {"operation": "append", "appended_text": "extra", "base_revision_id": UUID(int=5)},
        {"caller_key": "caller"},
        {"document_id": UUID(int=8)},
    ],
)
def test_materialization_complete_identity_changes_episode(tmp_path: Path, changes: dict) -> None:
    store = StateStore(tmp_path / "identity.sqlite")
    descriptor = _observe_materialization(store).replacement
    first_identity = PayloadIdentity(
        document_id=UUID(int=3), operation="replace", replacement=descriptor
    )
    first = _observe_materialization(store, payload_identity=first_identity)
    if "replacement_field" in changes:
        identity = first_identity.model_copy(
            update={
                "replacement": descriptor.model_copy(
                    update={changes["replacement_field"]: changes["value"]}
                )
            }
        )
    else:
        identity = first_identity.model_copy(update=changes)
    changed = _observe_materialization(store, payload_identity=identity)
    assert changed.episode_id != first.episode_id
    assert _observe_materialization(store, payload_identity=identity) == changed
    store.close()


@pytest.mark.parametrize(
    ("conflict", "message"),
    [
        ("attempt", "attempt changed"),
        ("episode", "another observation"),
        ("membership", "copy membership changed"),
    ],
)
def test_applied_token_commit_fences_receipt_and_copy_set_atomically(
    tmp_path: Path, conflict: str, message: str
) -> None:
    store = StateStore(tmp_path / "commit.sqlite")
    document_id, library_id = str(UUID(int=3)), str(UUID(int=1))
    store.upsert(
        kind="page",
        item_id="1",
        change_token="A",
        external_key="echo:page:1",
        ironrag_document_id=document_id,
        ironrag_library_id=library_id,
    )
    previous_cursor = store.get("page", "1")
    identity = PayloadIdentity(
        document_id=UUID(document_id),
        operation="replace",
        replacement=_observe_materialization(store).replacement,
    )
    observation = _observe_materialization(store, "B", payload_identity=identity)
    submitting = store.prepare_materialization_attempt(
        observation, document_id=document_id, library_id=library_id, intent_required=False
    )
    handle = OperationHandle(
        operation_id=UUID(int=5),
        document_id=UUID(document_id),
        result_revision_id=UUID(int=6),
        mutation_item_id=UUID(int=7),
        result_checksum=identity.replacement.checksum,
    )
    accepted = store.record_payload_receipt(submitting, handle)
    expected = accepted
    membership = frozenset({(library_id, document_id, "echo:page:1")})
    if conflict == "attempt":
        expected = submitting
    elif conflict == "episode":
        expected = replace(accepted, episode_id="another-episode")
    else:
        store.record_document_location(
            kind="page",
            item_id="1",
            external_key="echo:page:1",
            ironrag_document_id=str(UUID(int=4)),
            ironrag_library_id=str(UUID(int=2)),
        )
    locations = store.locations_for("page", "1")
    with pytest.raises(ValueError, match=message):
        store.upsert(
            kind="page",
            item_id="1",
            change_token="B",
            external_key="echo:page:1",
            ironrag_document_id=document_id,
            ironrag_library_id=library_id,
            expected_visibility_episode=observation.episode_id,
            expected_materialization_attempts=(expected,),
            expected_copy_membership=membership,
        )
    assert store.get("page", "1") == previous_cursor
    assert store.locations_for("page", "1") == locations
    assert store.materialization_attempts_for_document(document_id) == [accepted]
    current_membership = frozenset(
        (location.ironrag_library_id, location.ironrag_document_id, location.external_key)
        for location in locations
    )
    store.upsert(
        kind="page",
        item_id="1",
        change_token="B",
        external_key="echo:page:1",
        ironrag_document_id=document_id,
        ironrag_library_id=library_id,
        expected_visibility_episode=observation.episode_id,
        expected_materialization_attempts=(accepted,),
        expected_copy_membership=current_membership,
    )
    assert store.get("page", "1").change_token == "B"
    store.close()


def _create_legacy_cursor_db(db: Path) -> None:
    conn = sqlite3.connect(db)
    try:
        conn.executescript(
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
            INSERT INTO cursor VALUES (
                'page', '42', 'v1', 'echo:page:42', 'doc-old', 'lib-old', '2026-01-01'
            );
            """
        )
        conn.commit()
    finally:
        conn.close()


def test_upsert_then_get_roundtrip(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="2026-01-01T00:00:00Z",
        external_key="echo:page:42",
        ironrag_document_id="doc-1",
        ironrag_library_id="lib-1",
    )
    row = store.get("page", "42")
    assert row is not None
    assert row.external_key == "echo:page:42"
    assert row.current_library_id == "lib-1"
    assert row.change_token == "2026-01-01T00:00:00Z"
    location = store.get_location("page", "42", "lib-1")
    assert location is not None
    assert location.ironrag_document_id == "doc-1"
    store.close()


def test_upsert_without_document_preserves_existing_location(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="t1",
        external_key="echo:page:42",
        ironrag_document_id="doc-1",
        ironrag_library_id="lib-1",
    )
    store.upsert(
        kind="page",
        item_id="42",
        change_token="t2",
        external_key="echo:page:42",
        ironrag_document_id=None,
        ironrag_library_id="lib-1",
    )
    row = store.get("page", "42")
    assert row is not None
    assert row.change_token == "t2"
    assert row.current_library_id == "lib-1"
    location = store.get_location("page", "42", "lib-1")
    assert location is not None
    assert location.ironrag_document_id == "doc-1"
    store.close()


def test_upsert_replaces_library_id_when_route_moves(tmp_path: Path) -> None:
    """Route state advances while old materialized ownership is retained."""
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="t1",
        external_key="echo:page:42",
        ironrag_document_id="doc-1",
        ironrag_library_id="lib-1",
    )
    store.upsert(
        kind="page",
        item_id="42",
        change_token="t2",
        external_key="echo:page:42",
        ironrag_document_id=None,
        ironrag_library_id="lib-2",
    )
    row = store.get("page", "42")
    assert row is not None
    assert row.current_library_id == "lib-2"
    assert store.get_location("page", "42", "lib-1") is not None
    assert store.get_location("page", "42", "lib-2") is None
    store.close()


def test_record_document_location_preserves_source_cursor_and_route(
    tmp_path: Path,
) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="t1",
        external_key="echo:page:42",
        ironrag_document_id=None,
        ironrag_library_id="lib-1",
    )
    before = store.get("page", "42")
    assert before is not None

    store.record_document_location(
        kind="page",
        item_id="42",
        external_key="echo:page:42",
        ironrag_document_id="doc-1",
        ironrag_library_id="lib-2",
    )

    row = store.get("page", "42")
    assert row is not None
    assert row.change_token == "t1"
    assert row.last_pushed_at == before.last_pushed_at
    assert row.current_library_id == "lib-1"
    location = store.get_location("page", "42", "lib-2")
    assert location is not None
    assert location.ironrag_document_id == "doc-1"
    store.close()


def test_fresh_database_declares_library_id_not_null(tmp_path: Path) -> None:
    """Current route and materialized location libraries are never unknown."""
    db = tmp_path / "s.sqlite"
    store = StateStore(db)
    store.close()

    conn = sqlite3.connect(db)
    try:
        columns = {
            row[1]: row[3]  # name -> notnull flag
            for row in conn.execute("PRAGMA table_info(cursor)")
        }
    finally:
        conn.close()
    assert columns["current_library_id"] == 1


def test_legacy_cursor_keeps_route_without_adopting_unverified_document_id(
    tmp_path: Path,
) -> None:
    db = tmp_path / "legacy-cursor.sqlite"
    _create_legacy_cursor_db(db)

    store = StateStore(db)
    store.close()

    conn = sqlite3.connect(db)
    try:
        cursor_columns = {row[1] for row in conn.execute("PRAGMA table_info(cursor)")}
        cursor = conn.execute(
            "SELECT kind, item_id, external_key, current_library_id FROM cursor"
        ).fetchone()
        location = conn.execute(
            "SELECT kind, item_id, external_key, ironrag_library_id, "
            "ironrag_document_id FROM document_location"
        ).fetchone()
    finally:
        conn.close()

    assert cursor_columns == {
        "kind",
        "item_id",
        "change_token",
        "external_key",
        "current_library_id",
        "last_pushed_at",
    }
    assert cursor == ("page", "42", "echo:page:42", "lib-old")
    assert location is None


def test_concurrent_state_stores_migrate_one_legacy_cursor_atomically(tmp_path: Path) -> None:
    db = tmp_path / "concurrent-legacy.sqlite"
    _create_legacy_cursor_db(db)
    worker_count = 8
    ready = threading.Barrier(worker_count)

    def open_and_read(_: int) -> tuple[str, bool]:
        ready.wait(timeout=10)
        store = StateStore(db)
        try:
            cursor = store.get("page", "42")
            location = store.get_location("page", "42", "lib-old")
            assert cursor is not None
            return cursor.current_library_id, location is None
        finally:
            store.close()

    with ThreadPoolExecutor(max_workers=worker_count) as pool:
        results = list(pool.map(open_and_read, range(worker_count)))

    assert results == [("lib-old", True)] * worker_count


def test_location_ownership_contract_clears_precontract_rows_once(tmp_path: Path) -> None:
    db = tmp_path / "precontract-location.sqlite"
    precontract = StateStore(db)
    precontract.upsert(
        kind="page",
        item_id="42",
        change_token="v1",
        external_key="echo:page:42",
        ironrag_document_id="foreign-document-id",
        ironrag_library_id="lib-old",
    )
    precontract.close()

    # Recreate the state an intermediate/pre-contract build would leave: the
    # canonical table exists and contains a row, but no trust-generation
    # marker proves that row came from exact-key ownership.
    conn = sqlite3.connect(db)
    try:
        conn.execute(
            "DELETE FROM state_metadata WHERE key = ?",
            ("document_location_ownership_contract",),
        )
        conn.commit()
    finally:
        conn.close()

    upgraded = StateStore(db)
    cursor = upgraded.get("page", "42")
    assert cursor is not None
    assert cursor.current_library_id == "lib-old"
    assert upgraded.locations_for("page", "42") == []
    upgraded.upsert(
        kind="page",
        item_id="42",
        change_token="v2",
        external_key="echo:page:42",
        ironrag_document_id="verified-document-id",
        ironrag_library_id="lib-new",
    )
    upgraded.close()

    restarted = StateStore(db)
    location = restarted.get_location("page", "42", "lib-new")
    assert location is not None
    assert location.ironrag_document_id == "verified-document-id"
    restarted.close()

    conn = sqlite3.connect(db)
    try:
        marker = conn.execute(
            "SELECT value FROM state_metadata WHERE key = ?",
            ("document_location_ownership_contract",),
        ).fetchone()
    finally:
        conn.close()
    assert marker == ("1",)


def test_confirm_library_absence_clears_only_proven_scope_and_current_cursor(
    tmp_path: Path,
) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="v1",
        external_key="echo:page:42",
        ironrag_document_id="doc-old",
        ironrag_library_id="lib-old",
    )
    store.upsert(
        kind="page",
        item_id="42",
        change_token="v2",
        external_key="echo:page:42",
        ironrag_document_id="doc-current",
        ironrag_library_id="lib-current",
    )
    old_location = store.get_location("page", "42", "lib-old")

    assert store.confirm_library_absence(
        kind="page",
        item_id="42",
        ironrag_library_id="lib-old",
        expected_cursor=None,
        expected_location=old_location,
    )

    assert store.get_location("page", "42", "lib-old") is None
    assert store.get_location("page", "42", "lib-current") is not None
    current = store.get("page", "42")
    assert current is not None
    assert current.current_library_id == "lib-current"
    current_location = store.get_location("page", "42", "lib-current")

    assert store.confirm_library_absence(
        kind="page",
        item_id="42",
        ironrag_library_id="lib-current",
        expected_cursor=current,
        expected_location=current_location,
    )

    assert store.locations_for("page", "42") == []
    assert store.get("page", "42") is None
    store.close()


def test_confirm_library_absence_rejects_stale_state_evidence(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="v1",
        external_key="echo:page:42",
        ironrag_document_id="doc-old",
        ironrag_library_id="lib-1",
    )
    stale_cursor = store.get("page", "42")
    stale_location = store.get_location("page", "42", "lib-1")
    store.upsert(
        kind="page",
        item_id="42",
        change_token="v2",
        external_key="echo:page:42",
        ironrag_document_id="doc-fresh",
        ironrag_library_id="lib-1",
    )

    assert not store.confirm_library_absence(
        kind="page",
        item_id="42",
        ironrag_library_id="lib-1",
        expected_cursor=stale_cursor,
        expected_location=stale_location,
    )

    current = store.get("page", "42")
    location = store.get_location("page", "42", "lib-1")
    assert current is not None
    assert current.change_token == "v2"
    assert location is not None
    assert location.ironrag_document_id == "doc-fresh"
    store.close()


def test_delete_cursor_preserves_materialized_ownership(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="x",
        change_token="t",
        external_key="echo:page:x",
        ironrag_document_id="d",
        ironrag_library_id="lib-1",
    )
    store.delete_cursor("page", "x")
    assert store.get("page", "x") is None
    assert store.get_location("page", "x", "lib-1") is not None
    store.close()


def test_lookup_by_external_key(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="x",
        change_token="t",
        external_key="echo:page:x",
        ironrag_document_id="d",
        ironrag_library_id="lib-1",
    )
    row = store.get_by_external_key("echo:page:x")
    assert row is not None
    assert row.item_id == "x"
    assert store.get_by_external_key("echo:page:missing") is None
    store.close()


def test_items_of_kind_returns_only_matching_kind(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="a",
        change_token="t",
        external_key="echo:page:a",
        ironrag_document_id="d1",
        ironrag_library_id="lib-1",
    )
    store.upsert(
        kind="attachment",
        item_id="b",
        change_token="t",
        external_key="echo:attachment:b",
        ironrag_document_id="d2",
        ironrag_library_id="lib-1",
    )
    rows = store.items_of_kind("page")
    assert [r.item_id for r in rows] == ["a"]
    store.close()


def test_legacy_ambiguous_empty_fetch_rows_are_cleared_once(tmp_path: Path) -> None:
    db = tmp_path / "legacy.sqlite"
    conn = sqlite3.connect(db)
    try:
        conn.executescript(
            """
            CREATE TABLE empty_fetch (
                kind TEXT NOT NULL,
                item_id TEXT NOT NULL,
                change_token TEXT,
                external_key TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                PRIMARY KEY (kind, item_id)
            );
            INSERT INTO empty_fetch VALUES (
                'file', 'repo:main:path', 'v1', 'gitrepos:file:repo:main:path', '2026-01-01'
            );
            """
        )
        conn.commit()
    finally:
        conn.close()

    upgraded = StateStore(db)
    assert upgraded.get_empty_fetch("file", "repo:main:path") is None
    upgraded.record_empty_fetch(
        kind="file",
        item_id="repo:main:path",
        change_token="v2",
        external_key="gitrepos:file:repo:main:path",
        scope_fingerprint="scope-v2",
    )
    upgraded.close()

    reopened = StateStore(db)
    cached_absence = reopened.get_empty_fetch("file", "repo:main:path")
    assert cached_absence is not None
    assert cached_absence.change_token == "v2"
    assert cached_absence.scope_fingerprint == "scope-v2"
    reopened.close()


# --- durable dependent membership (parent census + member claims) ---


def _member(kind: str, item_id: str, library: str = "lib-1") -> MemberTarget:
    return MemberTarget(
        kind=kind,
        item_id=item_id,
        external_key=f"echo:{kind}:{item_id}",
        ironrag_library_id=library,
    )


def test_claim_member_is_visible_per_library_and_parent(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.claim_member(parent_kind="page", parent_item_id="1", member=_member("image", "a"))

    assert store.has_claims("image", "a", "lib-1") is True
    assert store.has_claims("image", "a", "lib-2") is False
    assert store.has_claims("image", "b", "lib-1") is False
    claims = store.claims_of_parent("page", "1")
    assert [(c.kind, c.item_id, c.ironrag_library_id) for c in claims] == [("image", "a", "lib-1")]
    assert claims[0].external_key == "echo:image:a"
    assert claims[0].parent_kind == "page"
    assert claims[0].parent_item_id == "1"
    store.close()


def test_record_parent_census_releases_omitted_members_and_asserts_listed(
    tmp_path: Path,
) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.claim_member(parent_kind="page", parent_item_id="1", member=_member("image", "old"))
    store.claim_member(parent_kind="page", parent_item_id="1", member=_member("image", "kept"))

    released = store.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t2",
        dependents_token="d2",
        members=(_member("image", "kept"), _member("attachment", "new")),
    )

    assert [(c.kind, c.item_id, c.ironrag_library_id) for c in released] == [
        ("image", "old", "lib-1")
    ]
    assert store.has_claims("image", "old", "lib-1") is False
    assert store.has_claims("image", "kept", "lib-1") is True
    assert store.has_claims("attachment", "new", "lib-1") is True
    census = store.parent_census("page", "1")
    assert census is not None
    assert (census.change_token, census.dependents_token) == ("t2", "d2")
    store.close()


def test_record_parent_census_upserts_tokens_without_touching_other_parents(
    tmp_path: Path,
) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.claim_member(parent_kind="page", parent_item_id="2", member=_member("image", "shared"))
    store.record_parent_census(
        kind="page", item_id="1", change_token="t1", dependents_token="d1", members=()
    )

    released = store.record_parent_census(
        kind="page", item_id="1", change_token="t2", dependents_token=None, members=()
    )

    assert released == []
    census = store.parent_census("page", "1")
    assert census is not None
    assert (census.change_token, census.dependents_token) == ("t2", None)
    assert store.has_claims("image", "shared", "lib-1") is True
    assert store.parent_census("page", "2") is None
    store.close()


def test_release_parent_returns_every_claim_and_forgets_census(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t1",
        dependents_token="d1",
        members=(_member("image", "a", "lib-1"), _member("image", "a", "lib-2")),
    )

    released = store.release_parent("page", "1")

    assert sorted((c.item_id, c.ironrag_library_id) for c in released) == [
        ("a", "lib-1"),
        ("a", "lib-2"),
    ]
    assert store.parent_census("page", "1") is None
    assert store.claims_of_parent("page", "1") == []
    assert store.release_parent("page", "1") == []
    store.close()


def test_shared_member_keeps_claims_of_other_parents(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.claim_member(parent_kind="page", parent_item_id="1", member=_member("image", "shared"))
    store.claim_member(parent_kind="page", parent_item_id="2", member=_member("image", "shared"))

    store.release_parent("page", "1")

    assert store.has_claims("image", "shared", "lib-1") is True
    store.release_parent("page", "2")
    assert store.has_claims("image", "shared", "lib-1") is False
    store.close()


def test_delete_by_library_drops_claims_in_that_library_only(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.claim_member(
        parent_kind="page", parent_item_id="1", member=_member("image", "a", "lib-1")
    )
    store.claim_member(
        parent_kind="page", parent_item_id="1", member=_member("image", "a", "lib-2")
    )
    store.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t1",
        dependents_token="d1",
        members=(_member("image", "a", "lib-1"), _member("image", "a", "lib-2")),
    )

    store.delete_by_library("lib-1")

    assert store.has_claims("image", "a", "lib-1") is False
    assert store.has_claims("image", "a", "lib-2") is True
    # The census proved a set that included the dropped library, so it goes
    # with it: the parent must fetch again before its remaining copy counts as
    # proven, otherwise the dependent phase would read the narrowed membership
    # as the parent's own verdict and reap the copy in lib-2.
    assert store.parent_census("page", "1") is None
    assert store.known_parents("page") == {"1"}
    store.close()


def test_parents_with_census_filters_by_kind(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.record_parent_census(
        kind="page", item_id="1", change_token="t1", dependents_token="d1", members=()
    )
    store.record_parent_census(
        kind="article", item_id="9", change_token="t1", dependents_token="d1", members=()
    )

    assert [c.item_id for c in store.parents_with_census("page")] == ["1"]
    assert [c.item_id for c in store.parents_with_census("article")] == ["9"]
    assert store.parents_with_census("issue") == []
    store.close()


def test_membership_survives_reopen(tmp_path: Path) -> None:
    db = tmp_path / "s.sqlite"
    store = StateStore(db)
    store.record_parent_census(
        kind="page",
        item_id="1",
        change_token="t1",
        dependents_token="d1",
        members=(_member("image", "a"),),
    )
    store.close()

    reopened = StateStore(db)
    assert reopened.has_claims("image", "a", "lib-1") is True
    census = reopened.parent_census("page", "1")
    assert census is not None
    assert census.dependents_token == "d1"
    reopened.close()


# --- consecutive item timeouts ---


def test_record_item_timeout_counts_consecutive_attempts(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    assert store.item_timeout_attempts("page", "1") == 0

    assert store.record_item_timeout(kind="page", item_id="1") == 1
    assert store.record_item_timeout(kind="page", item_id="1") == 2

    assert store.item_timeout_attempts("page", "1") == 2
    assert store.item_timeout_attempts("page", "2") == 0
    store.close()


def test_item_timeout_attempts_survive_reopen(tmp_path: Path) -> None:
    db = tmp_path / "state.sqlite"
    store = StateStore(db)
    store.record_item_timeout(kind="page", item_id="1")
    store.record_item_timeout(kind="page", item_id="1")
    store.close()

    reopened = StateStore(db)
    assert reopened.item_timeout_attempts("page", "1") == 2
    reopened.close()


def test_clear_item_timeout_returns_attempts_to_zero(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.record_item_timeout(kind="page", item_id="1")
    store.record_item_timeout(kind="page", item_id="1")

    store.clear_item_timeout(kind="page", item_id="1")

    assert store.item_timeout_attempts("page", "1") == 0
    store.close()


def test_delete_cursor_purges_item_timeout(tmp_path: Path) -> None:
    """A page deleted upstream and recreated must not inherit the old cap."""
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="1",
        change_token="v1",
        external_key="echo:page:1",
        ironrag_document_id=None,
        ironrag_library_id="lib-1",
    )
    store.record_item_timeout(kind="page", item_id="1")
    assert store.item_timeout_attempts("page", "1") == 1

    store.delete_cursor("page", "1")

    assert store.item_timeout_attempts("page", "1") == 0
    store.close()


def test_delete_by_library_purges_item_timeout_of_its_cursors(tmp_path: Path) -> None:
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="1",
        change_token="v1",
        external_key="echo:page:1",
        ironrag_document_id=None,
        ironrag_library_id="lib-1",
    )
    store.upsert(
        kind="page",
        item_id="2",
        change_token="v1",
        external_key="echo:page:2",
        ironrag_document_id=None,
        ironrag_library_id="lib-2",
    )
    store.record_item_timeout(kind="page", item_id="1")
    store.record_item_timeout(kind="page", item_id="2")

    store.delete_by_library("lib-1")

    assert store.item_timeout_attempts("page", "1") == 0
    assert store.item_timeout_attempts("page", "2") == 1
    store.close()


# ---------------------------------------------------------------------------
# Durable-store refusals: startup, identity uniqueness, evidence and commit.
# ---------------------------------------------------------------------------


class _OneRow:
    """The single-row cursor answer the WAL probe asks for."""

    def __init__(self, value: str) -> None:
        self._value = value

    def fetchone(self) -> tuple[str]:
        return (self._value,)


class _ScriptedConnection:
    """Answers the WAL journal-mode pragma from a script, delegating the rest."""

    def __init__(self, connection: sqlite3.Connection, answers: list[object]) -> None:
        self._connection = connection
        self._answers = answers

    def __getattr__(self, name: str) -> object:
        return getattr(self._connection, name)

    def execute(self, sql: str, *parameters: object) -> object:
        if sql.startswith("PRAGMA journal_mode=WAL") and self._answers:
            answer = self._answers.pop(0)
            if isinstance(answer, Exception):
                raise answer
            assert isinstance(answer, str)
            return _OneRow(answer)
        return self._connection.execute(sql, *parameters)


def _patch_wal_answers(monkeypatch: pytest.MonkeyPatch, answers: list[object]) -> None:
    real_connect = sqlite3.connect

    def connect(*args: object, **kwargs: object) -> _ScriptedConnection:
        return _ScriptedConnection(real_connect(*args, **kwargs), answers)

    monkeypatch.setattr("ironrag_connector.state.sqlite3.connect", connect)


def test_state_store_refuses_an_unknown_cursor_schema(tmp_path: Path) -> None:
    """A database it cannot identify is refused instead of half-migrated."""
    db = tmp_path / "unknown.sqlite"
    connection = sqlite3.connect(db)
    try:
        connection.execute("CREATE TABLE cursor (kind TEXT NOT NULL, item_id TEXT NOT NULL)")
        connection.commit()
    finally:
        connection.close()

    with pytest.raises(RuntimeError, match="unsupported cursor schema"):
        StateStore(db)


def test_state_store_retries_wal_enable_while_the_journal_is_locked(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A transient SQLite lock race is retried instead of failing the startup."""
    busy = sqlite3.OperationalError("database is locked")
    busy.sqlite_errorcode = sqlite3.SQLITE_BUSY
    answers: list[object] = [busy, busy, "wal"]
    _patch_wal_answers(monkeypatch, answers)

    store = StateStore(tmp_path / "wal.sqlite")

    assert answers == [], "both lock answers must be consumed by the retry loop"
    store.upsert(
        kind="page",
        item_id="1",
        change_token="A",
        external_key="echo:page:1",
        ironrag_document_id=None,
        ironrag_library_id=str(UUID(int=1)),
    )
    row = store.get("page", "1")
    assert row is not None
    assert row.change_token == "A"
    store.close()


def test_state_store_refuses_a_database_that_will_not_enable_wal(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A journal mode it did not ask for is not accepted as durable state."""
    _patch_wal_answers(monkeypatch, ["delete"])

    with pytest.raises(RuntimeError, match="refused to enable WAL"):
        StateStore(tmp_path / "refused.sqlite")


def test_state_store_does_not_retry_a_non_lock_wal_failure(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Only lock races are retried; any other SQLite failure surfaces at once."""
    _patch_wal_answers(monkeypatch, [sqlite3.OperationalError("disk I/O error")])

    with pytest.raises(sqlite3.OperationalError, match="disk I/O error"):
        StateStore(tmp_path / "broken.sqlite")


def test_observation_requires_a_complete_source_identity(tmp_path: Path) -> None:
    """An empty identity component cannot form one comparable source episode."""
    store = StateStore(tmp_path / "s.sqlite")
    with pytest.raises(ValueError, match="must be nonempty"):
        _observe_materialization(store, adapter="")
    store.close()


def test_ordinary_materialization_requires_a_complete_payload_identity(
    tmp_path: Path,
) -> None:
    """An intent-free attempt has no request to bind, so its body identity is required."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    with pytest.raises(ValueError, match="requires complete payload identity"):
        store.prepare_materialization_attempt(
            observation,
            document_id=str(UUID(int=3)),
            library_id=str(UUID(int=1)),
            intent_required=False,
        )
    store.close()


def test_ordinary_attempt_refuses_intent_phases_and_receipts(tmp_path: Path) -> None:
    """An intent-free attempt can never acquire a visibility request or receipt."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    store.prepare_materialization_attempt(
        observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    with pytest.raises(ValueError, match="identity conflicts with observation"):
        store.prepare_materialization_attempt(
            observation,
            document_id=str(UUID(int=3)),
            library_id=str(UUID(int=2)),
        )
    identity = PayloadIdentity(
        document_id=UUID(int=3),
        operation="replace",
        replacement=observation.replacement,
    )
    ordinary_observation = _observe_materialization(store, "B", payload_identity=identity)
    ordinary = store.prepare_materialization_attempt(
        ordinary_observation,
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=1)),
        intent_required=False,
    )
    with pytest.raises(ValueError, match="cannot enter an intent phase"):
        store.transition_materialization_attempt(ordinary, phase="intent_superseded")
    receipt = VisibilityIntentReceipt(
        visibility_intent_id=UUID(int=4),
        mutation_item_id=UUID(int=4),
        mutation_id=UUID(int=5),
        async_operation_id=UUID(int=6),
        document_id=UUID(int=3),
        revision_floor=1,
        source_identity=ordinary_observation.source_identity,
        materialization_token=ordinary_observation.materialization_token,
        replacement=ordinary_observation.replacement,
        request_fingerprint="opaque-server-fingerprint",
    )
    with pytest.raises(ValueError, match="no visibility receipt"):
        store.transition_materialization_attempt(
            ordinary, phase="payload_terminal", receipt=receipt
        )
    store.close()


def test_payload_receipt_requires_a_submitting_attempt_and_a_matching_binding(
    tmp_path: Path,
) -> None:
    """A receipt commits only the exact accepted body of a submitting payload."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    prepared = store.prepare_materialization_attempt(
        observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    handle = OperationHandle(
        operation_id=UUID(int=5),
        document_id=UUID(int=3),
        result_revision_id=UUID(int=6),
        mutation_item_id=UUID(int=7),
        result_checksum=observation.replacement.checksum,
    )
    with pytest.raises(ValueError, match="requires accepted intent and submitting phase"):
        store.record_payload_receipt(prepared, handle)

    identity = PayloadIdentity(
        document_id=UUID(int=3),
        operation="replace",
        replacement=observation.replacement,
    )
    ordinary_observation = _observe_materialization(store, "B", payload_identity=identity)
    ordinary = store.prepare_materialization_attempt(
        ordinary_observation,
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=1)),
        intent_required=False,
    )
    mismatched_receipt = handle.model_copy(update={"result_checksum": "sha256:" + "b" * 64})
    with pytest.raises(ValueError, match="does not match visibility binding"):
        store.record_payload_receipt(ordinary, mismatched_receipt)

    accepted = store.record_payload_receipt(ordinary, handle)
    rebound_receipt = handle.model_copy(update={"mutation_item_id": UUID(int=8)})
    with pytest.raises(ValueError, match="payload receipt is immutable"):
        store.record_payload_receipt(accepted, rebound_receipt)
    store.close()


def test_payload_identity_binding_refuses_a_foreign_target_and_rebinding(
    tmp_path: Path,
) -> None:
    """One durable key names one document and one complete body, forever."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    identity = PayloadIdentity(
        document_id=UUID(int=3),
        operation="replace",
        replacement=observation.replacement,
    )
    ordinary_observation = _observe_materialization(store, "B", payload_identity=identity)
    ordinary = store.prepare_materialization_attempt(
        ordinary_observation,
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=1)),
        intent_required=False,
    )

    foreign_target = identity.model_copy(update={"document_id": UUID(int=9)})
    with pytest.raises(ValueError, match="target mismatch"):
        store.bind_payload_identity(ordinary, foreign_target)
    rebound_identity = identity.model_copy(update={"caller_key": "another-namespace"})
    with pytest.raises(ValueError, match="payload identity is immutable"):
        store.bind_payload_identity(ordinary, rebound_identity)
    store.close()


def test_created_admission_requires_identity_and_only_settles_a_pending_row(
    tmp_path: Path,
) -> None:
    """A create journal row is identity-bound and settles exactly once."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    handle = OperationHandle(
        operation_id=UUID(int=5),
        document_id=UUID(int=3),
        result_revision_id=UUID(int=6),
        mutation_item_id=UUID(int=7),
        result_checksum=observation.replacement.checksum,
    )
    with pytest.raises(ValueError, match="requires complete payload identity"):
        store.record_created_admission(
            document_id=str(UUID(int=3)),
            library_id=str(UUID(int=1)),
            observation=observation,
            handle=handle,
        )

    identity = PayloadIdentity(
        document_id=UUID(int=3),
        operation="create",
        replacement=observation.replacement,
    )
    bound = _observe_materialization(store, payload_identity=identity)
    pending = store.record_created_admission(
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=1)),
        observation=bound,
        handle=handle,
    )
    assert pending.phase == "payload_submitting"
    assert store.pending_created_admission(str(UUID(int=3)), observation.replacement) == pending

    settled = store.finalize_created_admission(pending)
    assert settled.phase == "payload_terminal"
    with pytest.raises(ValueError, match="only a pending created admission"):
        store.finalize_created_admission(settled)
    store.close()


def test_target_evidence_must_name_the_scanned_target(tmp_path: Path) -> None:
    """Absence is only applied to evidence captured for that exact target."""
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="1",
        change_token="A",
        external_key="echo:page:1",
        ironrag_document_id=str(UUID(int=3)),
        ironrag_library_id="lib-1",
    )
    cursor = store.get("page", "1")
    location = store.get_location("page", "1", "lib-1")
    assert location is not None

    with pytest.raises(ValueError, match="cursor absence evidence"):
        store.confirm_library_absence(
            kind="page",
            item_id="1",
            ironrag_library_id="lib-2",
            expected_cursor=cursor,
            expected_location=None,
        )
    foreign_location = replace(location, ironrag_library_id="lib-2")
    with pytest.raises(ValueError, match="location absence evidence"):
        store.confirm_library_absence(
            kind="page",
            item_id="1",
            ironrag_library_id="lib-1",
            expected_cursor=None,
            expected_location=foreign_location,
        )
    assert store.get("page", "1") == cursor
    store.close()


def test_applied_commit_requires_the_observation_behind_its_attempts(
    tmp_path: Path,
) -> None:
    """Attempt and membership fences are meaningless without an observation."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    identity = PayloadIdentity(
        document_id=UUID(int=3),
        operation="replace",
        replacement=observation.replacement,
    )
    bound = _observe_materialization(store, "B", payload_identity=identity)
    attempt = store.prepare_materialization_attempt(
        bound,
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=1)),
        intent_required=False,
    )
    upsert_args = {
        "kind": "page",
        "item_id": "1",
        "change_token": "B",
        "external_key": "echo:page:1",
        "ironrag_document_id": str(UUID(int=3)),
        "ironrag_library_id": str(UUID(int=1)),
    }
    with pytest.raises(ValueError, match="requires observation and current document"):
        store.upsert(**upsert_args, expected_materialization_attempts=(attempt,))
    with pytest.raises(ValueError, match="copy membership commit requires observation"):
        store.upsert(
            **upsert_args,
            expected_visibility_episode=attempt.episode_id,
            expected_copy_membership=frozenset(),
        )
    assert store.get("page", "1") is None
    store.close()


def test_applied_commit_refuses_a_superseded_intent_and_an_unaccepted_payload(
    tmp_path: Path,
) -> None:
    """Neither a dead intent nor a payload without its receipt can advance a cursor."""
    store = StateStore(tmp_path / "s.sqlite")
    observation = _observe_materialization(store)
    identity = PayloadIdentity(
        document_id=UUID(int=3),
        operation="replace",
        replacement=observation.replacement,
    )
    bound = _observe_materialization(store, "B", payload_identity=identity)
    ordinary = store.prepare_materialization_attempt(
        bound,
        document_id=str(UUID(int=3)),
        library_id=str(UUID(int=1)),
        intent_required=False,
    )
    with pytest.raises(ValueError, match="exact accepted payload receipt"):
        store.upsert(
            kind="page",
            item_id="1",
            change_token="B",
            external_key="echo:page:1",
            ironrag_document_id=str(UUID(int=3)),
            ironrag_library_id=str(UUID(int=1)),
            expected_visibility_episode=bound.episode_id,
            expected_materialization_attempts=(ordinary,),
        )

    intent_observation = _observe_materialization(store, "C")
    prepared = store.prepare_materialization_attempt(
        intent_observation, document_id=str(UUID(int=3)), library_id=str(UUID(int=1))
    )
    submitting = store.transition_materialization_attempt(prepared, phase="intent_submitting")
    request = submitting.request
    assert request is not None
    receipt = VisibilityIntentReceipt(
        visibility_intent_id=UUID(int=4),
        mutation_item_id=UUID(int=4),
        mutation_id=UUID(int=5),
        async_operation_id=UUID(int=6),
        document_id=UUID(int=3),
        revision_floor=1,
        source_identity=request.source_identity,
        materialization_token=request.materialization_token,
        replacement=request.replacement,
        request_fingerprint="opaque-server-fingerprint",
    )
    accepted = store.transition_materialization_attempt(
        submitting, phase="intent_accepted", receipt=receipt
    )
    superseded = store.transition_materialization_attempt(accepted, phase="intent_superseded")
    with pytest.raises(ValueError, match="accepted current intents"):
        store.upsert(
            kind="page",
            item_id="1",
            change_token="C",
            external_key="echo:page:1",
            ironrag_document_id=str(UUID(int=3)),
            ironrag_library_id=str(UUID(int=1)),
            expected_visibility_episode=intent_observation.episode_id,
            expected_materialization_attempts=(superseded,),
        )
    assert store.get("page", "1") is None
    store.close()


def test_restore_deletes_an_absent_cursor_and_refuses_a_foreign_row(tmp_path: Path) -> None:
    """A deferred parent restores exactly its own row, or none at all."""
    store = StateStore(tmp_path / "s.sqlite")
    store.upsert(
        kind="page",
        item_id="1",
        change_token="v1",
        external_key="echo:page:1",
        ironrag_document_id=None,
        ironrag_library_id="lib-1",
    )
    store.upsert(
        kind="page",
        item_id="2",
        change_token="v1",
        external_key="echo:page:2",
        ironrag_document_id=None,
        ironrag_library_id="lib-1",
    )
    foreign = store.get("page", "2")
    assert foreign is not None

    store.restore(None, kind="page", item_id="1")

    assert store.get("page", "1") is None
    with pytest.raises(ValueError, match="restore target does not match"):
        store.restore(foreign, kind="page", item_id="1")
    assert store.get("page", "1") is None
    assert store.get("page", "2") == foreign
    store.close()
