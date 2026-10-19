"""Persistent source cursor and materialized-document registry backed by SQLite.

Why persistent
==============

The connector's diff stage compares each source item's ``change_token``
(typically ``updated_at``) against the value stored at the last successful
push. If the value matches, the item is skipped without downloading
bytes. Keeping that cursor across restarts means a connector that restarts
nightly does not re-push every item every morning.

SQLite over JSON
================

JSON is fine until two writers touch the file at once (multi-worker
deployments, manual /sync/run hitting a sweep already in progress).
SQLite gives us atomic upsert at zero ops cost -- single file, no daemon,
and serialized writes -- while WAL mode with ``synchronous=NORMAL`` keeps
readers independent of those writes. It is already part of the stdlib.

State model
===========

``cursor`` stores the last successfully applied source token and current route.
It deliberately does not own IronRAG document identities: one source item can
temporarily have materialized copies in more than one library after a route
move, especially when ``on_missing=ignore``. ``document_location`` is the sole
durable registry of those known copies, so a later source-required delete can
remove every copy even after its old library disappeared from routing.

Schema
======

::

    CREATE TABLE cursor (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        change_token TEXT,
        external_key TEXT NOT NULL,
        current_library_id TEXT NOT NULL,
        last_pushed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id)
    );

    CREATE TABLE document_location (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        external_key TEXT NOT NULL,
        ironrag_library_id TEXT NOT NULL,
        ironrag_document_id TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id, ironrag_library_id)
    );

    CREATE TABLE parent_census (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        change_token TEXT,
        dependents_token TEXT,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id)
    );

    CREATE TABLE dependent_membership (
        parent_kind TEXT NOT NULL,
        parent_item_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        external_key TEXT NOT NULL,
        ironrag_library_id TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (parent_kind, parent_item_id, kind, item_id, ironrag_library_id)
    );

All tables use the framework identity ``(kind, item_id)``. A cursor has one
current route, while a location adds ``ironrag_library_id`` to permit one known
copy per library. The backend's ``(library_id, external_key)`` uniqueness makes
that the complete multiplicity we need to represent.

Dependent membership
====================

A dependent (an attachment, an inline image) is never enumerated on its own, so
its absence can only be proven by its parent. ``dependent_membership`` records
which parent expects which dependent copy in which library; the claim is
written before the dependent is pushed so a crash cannot leave an unclaimed
live document. ``parent_census`` records the tokens at which the parent's
complete fetch last enumerated its dependents; a parent without a census row
has never been proven and is fetched again. Releasing a claim never deletes
anything by itself: the orchestrator deletes a copy only once no parent claims
it, and the reaper's dependent phase treats an unclaimed copy as an orphan.
"""

from __future__ import annotations

import json
import sqlite3
import threading
import time
from collections.abc import Iterable, Iterator
from contextlib import contextmanager
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any, Literal
from uuid import UUID, uuid4

from pydantic import BaseModel, ConfigDict

from .ironrag import (
    OperationHandle,
    ReplacementIdentity,
    VisibilityIntentReceipt,
    VisibilityIntentRequest,
)

_DELETE_EMPTY_FETCH_SQL = "DELETE FROM empty_fetch WHERE kind = ? AND item_id = ?"
_DELETE_ITEM_TIMEOUT_SQL = "DELETE FROM item_timeout WHERE kind = ? AND item_id = ?"
_SELECT_VISIBILITY_ATTEMPT_SQL = (
    "SELECT * FROM visibility_attempt WHERE episode_id = ? AND document_id = ?"
)
_SELECT_ITEM_TIMEOUT_ATTEMPTS_SQL = (
    "SELECT attempts FROM item_timeout WHERE kind = ? AND item_id = ?"
)


@dataclass(frozen=True)
class CursorRow:
    kind: str
    item_id: str
    change_token: str | None
    external_key: str
    current_library_id: str
    last_pushed_at: str


@dataclass(frozen=True)
class DocumentLocation:
    """One known IronRAG copy of a source identity in a library."""

    kind: str
    item_id: str
    external_key: str
    ironrag_library_id: str
    ironrag_document_id: str
    observed_at: str


@dataclass(frozen=True)
class EmptyFetchRow:
    """One cacheable authoritative absence and the library scope it proved."""

    change_token: str | None
    scope_fingerprint: str


@dataclass(frozen=True)
class MemberTarget:
    """One dependent copy a parent expects in one library."""

    kind: str
    item_id: str
    external_key: str
    ironrag_library_id: str


@dataclass(frozen=True)
class DependentClaim:
    """A parent's durable claim on one dependent copy."""

    parent_kind: str
    parent_item_id: str
    kind: str
    item_id: str
    external_key: str
    ironrag_library_id: str
    observed_at: str


@dataclass(frozen=True)
class ParentCensus:
    """Tokens at which a parent's dependent set was last proven complete."""

    kind: str
    item_id: str
    change_token: str | None
    dependents_token: str | None
    observed_at: str


class PayloadIdentity(BaseModel):
    """Complete client request semantics, not the backend fingerprint algorithm."""

    model_config = ConfigDict(frozen=True, extra="forbid")
    document_id: UUID
    operation: Literal["replace", "append"]
    replacement: ReplacementIdentity
    caller_key: str | None = None
    visibility_intent_id: UUID | None = None
    base_revision_id: UUID | None = None
    appended_text: str | None = None


@dataclass(frozen=True)
class MaterializationObservation:
    """One observation episode; opaque tokens are compared only for equality."""

    episode_id: str
    source_identity: str
    library_id: str
    target_document_id: str | None
    materialization_token: str | None
    replacement: ReplacementIdentity
    payload_identity: PayloadIdentity | None = None


@dataclass(frozen=True)
class MaterializationAttempt:
    """Durable request/receipt for one current or historical target copy."""

    episode_id: str
    document_id: str
    library_id: str
    phase: str
    request: VisibilityIntentRequest | None
    receipt: VisibilityIntentReceipt | None
    payload_key: str
    payload_receipt: OperationHandle | None = None
    intent_required: bool = True
    payload_identity: PayloadIdentity | None = None


class StateStore:
    """Thread-safe SQLite cursor store. One connection per process."""

    _DOCUMENT_LOCATION_OWNERSHIP_CONTRACT = "1"
    _SOURCE_DELETE_CACHE_CONTRACT = "2"

    _SCHEMA = """
    CREATE TABLE IF NOT EXISTS visibility_observation (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        episode_id TEXT NOT NULL UNIQUE,
        source_identity TEXT NOT NULL,
        library_id TEXT NOT NULL,
        target_document_id TEXT,
        materialization_token TEXT,
        replacement_json TEXT NOT NULL,
        payload_identity_json TEXT,
        PRIMARY KEY (kind, item_id)
    );
    CREATE TABLE IF NOT EXISTS visibility_attempt (
        episode_id TEXT NOT NULL,
        document_id TEXT NOT NULL,
        library_id TEXT NOT NULL,
        phase TEXT NOT NULL CHECK (phase IN (
            'intent_prepared', 'intent_submitting', 'intent_accepted',
            'payload_submitting', 'payload_terminal', 'intent_superseded'
        )),
        request_json TEXT,
        receipt_json TEXT,
        payload_key TEXT NOT NULL,
        payload_receipt_json TEXT,
        intent_required INTEGER NOT NULL DEFAULT 1 CHECK (intent_required IN (0, 1)),
        payload_identity_json TEXT,
        PRIMARY KEY (episode_id, document_id)
    );
    CREATE TABLE IF NOT EXISTS cursor (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        change_token TEXT,
        external_key TEXT NOT NULL,
        current_library_id TEXT NOT NULL,
        last_pushed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id)
    );
    CREATE INDEX IF NOT EXISTS cursor_external_key_idx ON cursor(external_key);
    CREATE INDEX IF NOT EXISTS cursor_kind_idx ON cursor(kind);

    CREATE TABLE IF NOT EXISTS document_location (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        external_key TEXT NOT NULL,
        ironrag_library_id TEXT NOT NULL,
        ironrag_document_id TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id, ironrag_library_id)
    );
    CREATE INDEX IF NOT EXISTS document_location_external_key_idx
        ON document_location(external_key);
    CREATE INDEX IF NOT EXISTS document_location_kind_idx
        ON document_location(kind);
    CREATE INDEX IF NOT EXISTS document_location_library_idx
        ON document_location(ironrag_library_id);

    -- Source-authoritative, successfully applied absence decisions. The
    -- answer belongs to one change_token, so it stays valid until the item is
    -- edited. Kept apart from `cursor` because it is a source fetch decision,
    -- not a successfully applied source token/current-route cursor.
    CREATE TABLE IF NOT EXISTS empty_fetch (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        change_token TEXT,
        external_key TEXT NOT NULL,
        scope_fingerprint TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id)
    );

    -- Consecutive item timeouts, keyed by source ref. While the timeouts keep
    -- coming the ref is replayed under the shorter retry budget, except on the
    -- first attempt and every escalation attempt, when the full budget returns.
    CREATE TABLE IF NOT EXISTS item_timeout (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        attempts INTEGER NOT NULL,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id)
    );

    CREATE TABLE IF NOT EXISTS state_metadata (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
    );

    -- Tokens at which a parent's complete fetch last enumerated its
    -- dependents. Absent until the parent has been proven once.
    CREATE TABLE IF NOT EXISTS parent_census (
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        change_token TEXT,
        dependents_token TEXT,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (kind, item_id)
    );

    -- Durable parent -> dependent-copy claims. A claim is written before the
    -- dependent is pushed and released only by a later complete census of the
    -- same parent or by that parent's authoritative absence. A dependent copy
    -- with no claim in its library is an orphan.
    CREATE TABLE IF NOT EXISTS dependent_membership (
        parent_kind TEXT NOT NULL,
        parent_item_id TEXT NOT NULL,
        kind TEXT NOT NULL,
        item_id TEXT NOT NULL,
        external_key TEXT NOT NULL,
        ironrag_library_id TEXT NOT NULL,
        observed_at TEXT NOT NULL,
        PRIMARY KEY (parent_kind, parent_item_id, kind, item_id, ironrag_library_id)
    );
    CREATE INDEX IF NOT EXISTS dependent_membership_member_idx
        ON dependent_membership(kind, item_id, ironrag_library_id);
    """

    def __init__(self, db_path: Path) -> None:
        self._db_path = db_path
        self._lock = threading.Lock()
        db_path.parent.mkdir(parents=True, exist_ok=True)
        self._conn = sqlite3.connect(
            str(db_path),
            check_same_thread=False,
            isolation_level=None,
            timeout=30.0,
        )
        try:
            self._enable_wal()
            self._conn.execute("PRAGMA synchronous=NORMAL")
            self._upgrade_cursor_locations()
            self._conn.executescript(self._SCHEMA)
            self._upgrade_materialization_identity()
            self._upgrade_document_location_ownership()
            self._upgrade_empty_fetch_scope()
            self._upgrade_source_delete_cache()
        except Exception:
            self._conn.close()
            raise

    def _enable_wal(self) -> None:
        """Enable WAL despite SQLite's non-waiting journal-mode lock race."""
        deadline = time.monotonic() + 30.0
        delay = 0.001
        while True:
            try:
                row = self._conn.execute("PRAGMA journal_mode=WAL").fetchone()
                if row is None or str(row[0]).lower() != "wal":
                    raise RuntimeError("SQLite refused to enable WAL journal mode")
                return
            except sqlite3.OperationalError as exc:
                error_code = getattr(exc, "sqlite_errorcode", None)
                primary_code = error_code & 0xFF if error_code is not None else None
                if primary_code not in {sqlite3.SQLITE_BUSY, sqlite3.SQLITE_LOCKED}:
                    raise
                if time.monotonic() >= deadline:
                    raise
                time.sleep(delay)
                delay = min(delay * 2, 0.05)

    def _upgrade_empty_fetch_scope(self) -> None:
        """Add the deletion-scope column before invalidating old cache rows."""
        with self._transaction() as cur:
            columns = {str(column[1]) for column in cur.execute("PRAGMA table_info(empty_fetch)")}
            if "scope_fingerprint" not in columns:
                cur.execute(
                    "ALTER TABLE empty_fetch ADD COLUMN scope_fingerprint TEXT NOT NULL DEFAULT ''"
                )

    def _upgrade_cursor_locations(self) -> None:
        """Split legacy document ownership out of ``cursor`` exactly once.

        The previous schema could remember only one library/document pair, and
        duplicate-content skip historically stored another source key's
        document id there. Preserve its library solely as current-route scan
        scope; the unverified legacy id is discarded. Exact-key discovery is
        the only path that may establish canonical ``document_location``
        ownership after migration.
        """
        with self._transaction() as cur:
            row = cur.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'cursor'"
            ).fetchone()
            if row is None:
                return
            columns = {str(column[1]) for column in cur.execute("PRAGMA table_info(cursor)")}
            canonical = {
                "kind",
                "item_id",
                "change_token",
                "external_key",
                "current_library_id",
                "last_pushed_at",
            }
            if columns == canonical:
                return
            legacy = {
                "kind",
                "item_id",
                "change_token",
                "external_key",
                "ironrag_document_id",
                "ironrag_library_id",
                "last_pushed_at",
            }
            if columns != legacy:
                raise RuntimeError(
                    "unsupported cursor schema; expected canonical or legacy columns"
                )
            cur.execute(
                """
                CREATE TABLE document_location (
                    kind TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    external_key TEXT NOT NULL,
                    ironrag_library_id TEXT NOT NULL,
                    ironrag_document_id TEXT NOT NULL,
                    observed_at TEXT NOT NULL,
                    PRIMARY KEY (kind, item_id, ironrag_library_id)
                )
                """
            )
            cur.execute(
                """
                CREATE TABLE cursor_next (
                    kind TEXT NOT NULL,
                    item_id TEXT NOT NULL,
                    change_token TEXT,
                    external_key TEXT NOT NULL,
                    current_library_id TEXT NOT NULL,
                    last_pushed_at TEXT NOT NULL,
                    PRIMARY KEY (kind, item_id)
                )
                """
            )
            cur.execute(
                """
                INSERT INTO cursor_next (
                    kind, item_id, change_token, external_key,
                    current_library_id, last_pushed_at
                )
                SELECT
                    kind, item_id, change_token, external_key,
                    ironrag_library_id, last_pushed_at
                FROM cursor
                """
            )
            cur.execute("DROP TABLE cursor")
            cur.execute("ALTER TABLE cursor_next RENAME TO cursor")

    def _upgrade_document_location_ownership(self) -> None:
        """Invalidate document ids recorded before exact-key provenance.

        Legacy duplicate-content handling could record another source item's
        id as if it were owned. Clearing the derived registry and installing
        its trust-generation marker in one write transaction makes every row
        visible after this point come only from exact discovery or a validated
        create/revision. The cursor's library remains available as scan scope.
        """
        with self._transaction() as cur:
            row = cur.execute(
                "SELECT value FROM state_metadata WHERE key = ?",
                ("document_location_ownership_contract",),
            ).fetchone()
            if row is None or row[0] != self._DOCUMENT_LOCATION_OWNERSHIP_CONTRACT:
                cur.execute("DELETE FROM document_location")
                cur.execute(
                    "INSERT INTO state_metadata (key, value) VALUES (?, ?) "
                    "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    (
                        "document_location_ownership_contract",
                        self._DOCUMENT_LOCATION_OWNERSHIP_CONTRACT,
                    ),
                )

    def _upgrade_source_delete_cache(self) -> None:
        """Discard pre-contract negative rows exactly once.

        Before typed fetch outcomes, ``empty_fetch`` also stored ambiguous
        ``None`` results, including transient vendor failures. No row-level fact
        can distinguish those from an authoritative empty item, so an upgrade
        must invalidate the small derived cache rather than trust stale content.
        The generation marker preserves newly established rows across every
        later restart.
        """
        with self._transaction() as cur:
            row = cur.execute(
                "SELECT value FROM state_metadata WHERE key = ?",
                ("source_delete_cache_contract",),
            ).fetchone()
            if row is None or row[0] != self._SOURCE_DELETE_CACHE_CONTRACT:
                cur.execute("DELETE FROM empty_fetch")
                cur.execute(
                    "INSERT INTO state_metadata (key, value) VALUES (?, ?) "
                    "ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    (
                        "source_delete_cache_contract",
                        self._SOURCE_DELETE_CACHE_CONTRACT,
                    ),
                )

    def close(self) -> None:
        with self._lock:
            self._conn.close()

    @contextmanager
    def _cursor(self) -> Iterator[sqlite3.Cursor]:
        with self._lock:
            cur = self._conn.cursor()
            try:
                yield cur
            finally:
                cur.close()

    @contextmanager
    def _transaction(self) -> Iterator[sqlite3.Cursor]:
        """Serialize a small state transition and commit it atomically."""
        with self._lock:
            cur = self._conn.cursor()
            cur.execute("BEGIN IMMEDIATE")
            try:
                yield cur
            except Exception:
                cur.execute("ROLLBACK")
                raise
            else:
                cur.execute("COMMIT")
            finally:
                cur.close()

    def _upgrade_materialization_identity(self) -> None:
        """One additive state-format migration; immutable old requests remain intact."""
        with self._transaction() as cur:
            observation_columns = {
                row[1] for row in cur.execute("PRAGMA table_info(visibility_observation)")
            }
            if "payload_identity_json" not in observation_columns:
                cur.execute(
                    "ALTER TABLE visibility_observation ADD COLUMN payload_identity_json TEXT"
                )
            attempt_columns = {
                row[1] for row in cur.execute("PRAGMA table_info(visibility_attempt)")
            }
            if "intent_required" not in attempt_columns:
                cur.execute(
                    "ALTER TABLE visibility_attempt ADD COLUMN intent_required "
                    "INTEGER NOT NULL DEFAULT 1 CHECK (intent_required IN (0, 1))"
                )
            if "payload_identity_json" not in attempt_columns:
                cur.execute("ALTER TABLE visibility_attempt ADD COLUMN payload_identity_json TEXT")
            if any(
                row[1] == "request_json" and row[3]
                for row in cur.execute("PRAGMA table_info(visibility_attempt)")
            ):
                # Rebuild only the changed nullability; preserve every receipt,
                # request, key, phase and identity byte-for-byte.
                cur.execute(
                    "ALTER TABLE visibility_attempt "
                    "RENAME TO materialization_attempt_previous_format"
                )
                schema = self._SCHEMA.split("CREATE TABLE IF NOT EXISTS visibility_attempt (", 1)[
                    1
                ].split(";", 1)[0]
                cur.execute("CREATE TABLE visibility_attempt (" + schema)
                cur.execute(
                    "INSERT INTO visibility_attempt "
                    "SELECT * FROM materialization_attempt_previous_format"
                )
                cur.execute("DROP TABLE materialization_attempt_previous_format")

    def observe_materialization(
        self,
        *,
        adapter: str,
        kind: str,
        item_id: str,
        external_key: str,
        library_id: str,
        target_document_id: str | None,
        materialization_token: str | None,
        replacement: ReplacementIdentity,
        payload_identity: PayloadIdentity | None = None,
    ) -> MaterializationObservation:
        """Persist desired identity before writes; A→B→A is three episodes.

        Retransmission of the last observation reuses its episode. Backend
        supersession is not a source observation and cannot roll this nonce.
        """
        if not all((adapter, kind, item_id, external_key)):
            raise ValueError("visibility source identity components must be nonempty")
        UUID(library_id)
        if target_document_id is not None:
            UUID(target_document_id)
        source_identity = json.dumps(
            [adapter, kind, item_id, external_key], ensure_ascii=True, separators=(",", ":")
        )
        descriptor = replacement.model_dump_json(by_alias=True)
        identity_json = None if payload_identity is None else payload_identity.model_dump_json()
        with self._transaction() as cur:
            row = cur.execute(
                "SELECT episode_id, source_identity, library_id, target_document_id, "
                "materialization_token, "
                "replacement_json, payload_identity_json FROM visibility_observation "
                "WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            ).fetchone()
            if row is not None and row[1:] == (
                source_identity,
                library_id,
                target_document_id,
                materialization_token,
                descriptor,
                identity_json,
            ):
                episode = row[0]
            else:
                episode = str(uuid4())
                cur.execute(
                    "INSERT INTO visibility_observation VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) "
                    "ON CONFLICT(kind, item_id) DO UPDATE SET "
                    "episode_id = excluded.episode_id, source_identity = excluded.source_identity, "
                    "library_id = excluded.library_id, "
                    "target_document_id = excluded.target_document_id, "
                    "materialization_token = excluded.materialization_token, "
                    "replacement_json = excluded.replacement_json, "
                    "payload_identity_json = excluded.payload_identity_json",
                    (
                        kind,
                        item_id,
                        episode,
                        source_identity,
                        library_id,
                        target_document_id,
                        materialization_token,
                        descriptor,
                        identity_json,
                    ),
                )
        return MaterializationObservation(
            episode,
            source_identity,
            library_id,
            target_document_id,
            materialization_token,
            replacement,
            payload_identity,
        )

    @staticmethod
    def _materialization_attempt(row: tuple[Any, ...]) -> MaterializationAttempt:
        return MaterializationAttempt(
            row[0],
            row[1],
            row[2],
            row[3],
            None if row[4] is None else VisibilityIntentRequest.model_validate_json(row[4]),
            None if row[5] is None else VisibilityIntentReceipt.model_validate_json(row[5]),
            row[6],
            None if row[7] is None else OperationHandle.model_validate_json(row[7]),
            bool(row[8]),
            None if row[9] is None else PayloadIdentity.model_validate_json(row[9]),
        )

    def prepare_materialization_attempt(
        self,
        observation: MaterializationObservation,
        *,
        document_id: str,
        library_id: str,
        intent_required: bool = True,
    ) -> MaterializationAttempt:
        """Persist an immutable per-copy request before its first HTTP send."""
        UUID(document_id)
        UUID(library_id)
        if not intent_required and observation.payload_identity is None:
            raise ValueError("ordinary materialization requires complete payload identity")
        request = (
            VisibilityIntentRequest(
                idempotency_key=str(uuid4()),
                source_identity=observation.source_identity,
                materialization_token=observation.materialization_token,
                replacement=observation.replacement,
            )
            if intent_required
            else None
        )
        with self._transaction() as cur:
            cur.execute(
                "INSERT INTO visibility_attempt VALUES "
                "(?, ?, ?, ?, ?, NULL, ?, NULL, ?, ?) "
                "ON CONFLICT(episode_id, document_id) DO NOTHING",
                (
                    observation.episode_id,
                    document_id,
                    library_id,
                    "intent_prepared" if intent_required else "payload_submitting",
                    request.model_dump_json(by_alias=True) if request is not None else None,
                    str(uuid4()),
                    int(intent_required),
                    None
                    if observation.payload_identity is None
                    else observation.payload_identity.model_dump_json(),
                ),
            )
            row = cur.execute(
                _SELECT_VISIBILITY_ATTEMPT_SQL,
                (observation.episode_id, document_id),
            ).fetchone()
        assert row is not None
        attempt = self._materialization_attempt(row)
        if (
            attempt.library_id != library_id
            or (
                intent_required
                and (
                    attempt.request is None
                    or attempt.request.source_identity != observation.source_identity
                    or attempt.request.materialization_token != observation.materialization_token
                    or attempt.request.replacement != observation.replacement
                )
            )
            or attempt.intent_required != intent_required
            or (not intent_required and attempt.payload_identity != observation.payload_identity)
        ):
            raise ValueError("visibility attempt identity conflicts with observation")
        return attempt

    @staticmethod
    def _validate_materialization_transition(
        attempt: MaterializationAttempt,
        *,
        phase: str,
        receipt: VisibilityIntentReceipt | None = None,
    ) -> VisibilityIntentReceipt | None:
        """Validate phase and immutable receipt independently of the storage CAS."""
        allowed = {
            "intent_prepared": {"intent_submitting"},
            "intent_submitting": {"intent_submitting", "intent_accepted"},
            "intent_accepted": {"payload_submitting", "intent_superseded"},
            "payload_submitting": {"payload_submitting", "payload_terminal", "intent_superseded"},
            "payload_terminal": {"payload_submitting", "intent_superseded"},
            "intent_superseded": set(),
        }
        if phase not in allowed[attempt.phase]:
            raise ValueError("invalid visibility attempt phase transition")
        if not attempt.intent_required and phase.startswith("intent_"):
            raise ValueError("ordinary materialization cannot enter an intent phase")
        effective_receipt = receipt if receipt is not None else attempt.receipt
        if effective_receipt is not None:
            if attempt.request is None:
                raise ValueError("ordinary payload has no visibility receipt")
            effective_receipt.validate_binding(UUID(attempt.document_id), attempt.request)
        if attempt.receipt is not None and effective_receipt != attempt.receipt:
            raise ValueError("visibility receipt is immutable")
        if (
            attempt.intent_required
            and phase not in {"intent_prepared", "intent_submitting"}
            and effective_receipt is None
        ):
            raise ValueError("visibility acceptance requires a complete receipt")
        return effective_receipt

    def transition_materialization_attempt(
        self,
        attempt: MaterializationAttempt,
        *,
        phase: str,
        receipt: VisibilityIntentReceipt | None = None,
    ) -> MaterializationAttempt:
        """Compare-and-set a phase; unknown sends retain their exact key and floor."""
        effective_receipt = self._validate_materialization_transition(
            attempt, phase=phase, receipt=receipt
        )
        payload_key = str(uuid4()) if phase == "payload_terminal" else attempt.payload_key
        with self._transaction() as cur:
            row = cur.execute(
                _SELECT_VISIBILITY_ATTEMPT_SQL,
                (attempt.episode_id, attempt.document_id),
            ).fetchone()
            if row is None or self._materialization_attempt(row) != attempt:
                raise ValueError("visibility attempt changed concurrently")
            cur.execute(
                "UPDATE visibility_attempt SET phase = ?, receipt_json = ?, payload_key = ?, "
                "payload_receipt_json = ? "
                "WHERE episode_id = ? AND document_id = ? AND phase = ? AND payload_key = ?",
                (
                    phase,
                    None
                    if effective_receipt is None
                    else effective_receipt.model_dump_json(by_alias=True),
                    payload_key,
                    None
                    if phase == "payload_terminal" or attempt.payload_receipt is None
                    else attempt.payload_receipt.model_dump_json(),
                    attempt.episode_id,
                    attempt.document_id,
                    attempt.phase,
                    attempt.payload_key,
                ),
            )
            if cur.rowcount != 1:
                raise ValueError("visibility attempt changed concurrently")
        return MaterializationAttempt(
            attempt.episode_id,
            attempt.document_id,
            attempt.library_id,
            phase,
            attempt.request,
            effective_receipt,
            payload_key,
            None if phase == "payload_terminal" else attempt.payload_receipt,
            attempt.intent_required,
            attempt.payload_identity,
        )

    def record_payload_receipt(
        self, attempt: MaterializationAttempt, handle: OperationHandle
    ) -> MaterializationAttempt:
        """Commit acceptance before polling; replay retains immutable payload lineage."""
        if attempt.phase != "payload_submitting" or (
            attempt.intent_required and attempt.receipt is None
        ):
            raise ValueError("payload receipt requires accepted intent and submitting phase")
        expected_checksum = None
        if attempt.receipt is not None:
            expected_checksum = attempt.receipt.replacement.checksum
        elif attempt.payload_identity is not None:
            expected_checksum = attempt.payload_identity.replacement.checksum
        if (
            handle.document_id != UUID(attempt.document_id)
            or handle.visibility_intent_id
            != (attempt.receipt.visibility_intent_id if attempt.receipt is not None else None)
            or handle.result_revision_id is None
            or handle.mutation_item_id is None
            or handle.result_checksum != expected_checksum
        ):
            raise ValueError("payload receipt does not match visibility binding")
        if attempt.payload_receipt is not None and attempt.payload_receipt != handle:
            raise ValueError("payload receipt is immutable")
        with self._transaction() as cur:
            row = cur.execute(
                _SELECT_VISIBILITY_ATTEMPT_SQL,
                (attempt.episode_id, attempt.document_id),
            ).fetchone()
            if row is None or self._materialization_attempt(row) != attempt:
                raise ValueError("payload receipt changed concurrently")
            cur.execute(
                "UPDATE visibility_attempt SET payload_receipt_json = ? WHERE episode_id = ? "
                "AND document_id = ? AND phase = 'payload_submitting' AND payload_key = ? "
                "AND (payload_receipt_json IS NULL OR payload_receipt_json = ?)",
                (
                    handle.model_dump_json(),
                    attempt.episode_id,
                    attempt.document_id,
                    attempt.payload_key,
                    handle.model_dump_json(),
                ),
            )
            if cur.rowcount != 1:
                raise ValueError("payload receipt changed concurrently")
        return MaterializationAttempt(
            attempt.episode_id,
            attempt.document_id,
            attempt.library_id,
            attempt.phase,
            attempt.request,
            attempt.receipt,
            attempt.payload_key,
            handle,
            attempt.intent_required,
            attempt.payload_identity,
        )

    def bind_payload_identity(
        self, attempt: MaterializationAttempt, identity: PayloadIdentity
    ) -> MaterializationAttempt:
        """Persist one exact complete body under its durable key before sending."""
        if identity.document_id != UUID(attempt.document_id):
            raise ValueError("payload identity target mismatch")
        if attempt.payload_identity is not None and attempt.payload_identity != identity:
            raise ValueError("payload identity is immutable")
        with self._transaction() as cur:
            row = cur.execute(
                _SELECT_VISIBILITY_ATTEMPT_SQL,
                (attempt.episode_id, attempt.document_id),
            ).fetchone()
            if row is None or self._materialization_attempt(row) != attempt:
                raise ValueError("materialization attempt changed concurrently")
            cur.execute(
                "UPDATE visibility_attempt SET payload_identity_json = ? "
                "WHERE episode_id = ? AND document_id = ?",
                (identity.model_dump_json(), attempt.episode_id, attempt.document_id),
            )
        return MaterializationAttempt(
            attempt.episode_id,
            attempt.document_id,
            attempt.library_id,
            attempt.phase,
            attempt.request,
            attempt.receipt,
            attempt.payload_key,
            attempt.payload_receipt,
            attempt.intent_required,
            identity,
        )

    def materialization_attempts_for_document(
        self, document_id: str
    ) -> list[MaterializationAttempt]:
        """Retained per-copy retirement/replay evidence across source episodes.

        Rows have no chronological authority; only the server's current head
        can prove that a returned intent is still current.
        """
        UUID(document_id)
        with self._cursor() as cur:
            rows = cur.execute(
                "SELECT * FROM visibility_attempt WHERE document_id = ?", (document_id,)
            ).fetchall()
        return [self._materialization_attempt(row) for row in rows]

    def get(self, kind: str, item_id: str) -> CursorRow | None:
        with self._cursor() as cur:
            row = cur.execute(
                "SELECT kind, item_id, change_token, external_key, "
                "current_library_id, last_pushed_at "
                "FROM cursor WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            ).fetchone()
        if row is None:
            return None
        return CursorRow(*row)

    def get_by_external_key(self, external_key: str) -> CursorRow | None:
        with self._cursor() as cur:
            row = cur.execute(
                "SELECT kind, item_id, change_token, external_key, "
                "current_library_id, last_pushed_at "
                "FROM cursor WHERE external_key = ?",
                (external_key,),
            ).fetchone()
        if row is None:
            return None
        return CursorRow(*row)

    def get_location(
        self,
        kind: str,
        item_id: str,
        ironrag_library_id: str,
    ) -> DocumentLocation | None:
        """Return the known copy for one source identity and library."""
        with self._cursor() as cur:
            row = cur.execute(
                "SELECT kind, item_id, external_key, ironrag_library_id, "
                "ironrag_document_id, observed_at FROM document_location "
                "WHERE kind = ? AND item_id = ? AND ironrag_library_id = ?",
                (kind, item_id, ironrag_library_id),
            ).fetchone()
        return None if row is None else DocumentLocation(*row)

    def locations_for(self, kind: str, item_id: str) -> list[DocumentLocation]:
        """Return every known materialized copy of one source identity."""
        with self._cursor() as cur:
            rows = cur.execute(
                "SELECT kind, item_id, external_key, ironrag_library_id, "
                "ironrag_document_id, observed_at FROM document_location "
                "WHERE kind = ? AND item_id = ? ORDER BY ironrag_library_id",
                (kind, item_id),
            ).fetchall()
        return [DocumentLocation(*row) for row in rows]

    def locations_of_kind(self, kind: str) -> list[DocumentLocation]:
        """Return all known materialized copies for one adapter kind."""
        with self._cursor() as cur:
            rows = cur.execute(
                "SELECT kind, item_id, external_key, ironrag_library_id, "
                "ironrag_document_id, observed_at FROM document_location "
                "WHERE kind = ? ORDER BY item_id, ironrag_library_id",
                (kind,),
            ).fetchall()
        return [DocumentLocation(*row) for row in rows]

    @staticmethod
    def _record_location(
        cur: sqlite3.Cursor,
        *,
        kind: str,
        item_id: str,
        external_key: str,
        ironrag_document_id: str,
        ironrag_library_id: str,
        observed_at: str,
    ) -> None:
        cur.execute(
            """
            INSERT INTO document_location (
                kind, item_id, external_key, ironrag_library_id,
                ironrag_document_id, observed_at
            ) VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(kind, item_id, ironrag_library_id) DO UPDATE SET
                external_key = excluded.external_key,
                ironrag_document_id = excluded.ironrag_document_id,
                observed_at = excluded.observed_at
            """,
            (
                kind,
                item_id,
                external_key,
                ironrag_library_id,
                ironrag_document_id,
                observed_at,
            ),
        )

    def remove_location(
        self,
        *,
        kind: str,
        item_id: str,
        ironrag_library_id: str,
        ironrag_document_id: str,
    ) -> None:
        """Forget exactly the copy whose absence IronRAG established."""
        with self._cursor() as cur:
            cur.execute(
                "DELETE FROM document_location WHERE kind = ? AND item_id = ? "
                "AND ironrag_library_id = ? AND ironrag_document_id = ?",
                (kind, item_id, ironrag_library_id, ironrag_document_id),
            )

    @staticmethod
    def _validate_target_evidence(
        *,
        kind: str,
        item_id: str,
        ironrag_library_id: str,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> None:
        if expected_cursor is not None and (
            expected_cursor.kind != kind
            or expected_cursor.item_id != item_id
            or expected_cursor.current_library_id != ironrag_library_id
        ):
            raise ValueError("cursor absence evidence does not match source target")
        if expected_location is not None and (
            expected_location.kind != kind
            or expected_location.item_id != item_id
            or expected_location.ironrag_library_id != ironrag_library_id
        ):
            raise ValueError("location absence evidence does not match source target")

    @staticmethod
    def _target_state(
        cur: sqlite3.Cursor,
        *,
        kind: str,
        item_id: str,
        ironrag_library_id: str,
    ) -> tuple[CursorRow | None, DocumentLocation | None]:
        cursor_row = cur.execute(
            "SELECT kind, item_id, change_token, external_key, "
            "current_library_id, last_pushed_at FROM cursor "
            "WHERE kind = ? AND item_id = ?",
            (kind, item_id),
        ).fetchone()
        current_cursor = None if cursor_row is None else CursorRow(*cursor_row)
        current_target_cursor = (
            current_cursor
            if current_cursor is not None
            and current_cursor.current_library_id == ironrag_library_id
            else None
        )
        location_row = cur.execute(
            "SELECT kind, item_id, external_key, ironrag_library_id, "
            "ironrag_document_id, observed_at FROM document_location "
            "WHERE kind = ? AND item_id = ? AND ironrag_library_id = ?",
            (kind, item_id, ironrag_library_id),
        ).fetchone()
        current_location = None if location_row is None else DocumentLocation(*location_row)
        return current_target_cursor, current_location

    def target_state_matches(
        self,
        *,
        kind: str,
        item_id: str,
        ironrag_library_id: str,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> bool:
        """Compare one target with pre-scan evidence without mutating it."""
        self._validate_target_evidence(
            kind=kind,
            item_id=item_id,
            ironrag_library_id=ironrag_library_id,
            expected_cursor=expected_cursor,
            expected_location=expected_location,
        )
        with self._cursor() as cur:
            current = self._target_state(
                cur,
                kind=kind,
                item_id=item_id,
                ironrag_library_id=ironrag_library_id,
            )
        return current == (expected_cursor, expected_location)

    def confirm_library_absence(
        self,
        *,
        kind: str,
        item_id: str,
        ironrag_library_id: str,
        expected_cursor: CursorRow | None,
        expected_location: DocumentLocation | None,
    ) -> bool:
        """Forget state only after an exact scan proved this target absent.

        A zero-result reaper walk has no document id to pass through
        :meth:`remove_location`, but it is still authoritative for the one
        canonical source key in the scanned library. Clear that location and
        its current-route cursor atomically only when both rows still match
        the evidence captured before the scan. Ownership in other libraries
        is unaffected, and a concurrent fresh observation makes this a no-op.
        """
        self._validate_target_evidence(
            kind=kind,
            item_id=item_id,
            ironrag_library_id=ironrag_library_id,
            expected_cursor=expected_cursor,
            expected_location=expected_location,
        )
        with self._transaction() as cur:
            current = self._target_state(
                cur,
                kind=kind,
                item_id=item_id,
                ironrag_library_id=ironrag_library_id,
            )
            if current != (expected_cursor, expected_location):
                return False
            cur.execute(
                "DELETE FROM document_location WHERE kind = ? AND item_id = ? "
                "AND ironrag_library_id = ?",
                (kind, item_id, ironrag_library_id),
            )
            cur.execute(
                "DELETE FROM cursor WHERE kind = ? AND item_id = ? AND current_library_id = ?",
                (kind, item_id, ironrag_library_id),
            )
            cur.execute(
                _DELETE_ITEM_TIMEOUT_SQL,
                (kind, item_id),
            )
            return True

    def get_empty_fetch(self, kind: str, item_id: str) -> EmptyFetchRow | None:
        """Token whose source-authoritative absence was applied successfully.

        Returns ``None`` when no cacheable absence has been established.
        """
        with self._cursor() as cur:
            row = cur.execute(
                "SELECT change_token, scope_fingerprint FROM empty_fetch "
                "WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            ).fetchone()
        return None if row is None else EmptyFetchRow(*row)

    def record_empty_fetch(
        self,
        *,
        kind: str,
        item_id: str,
        change_token: str | None,
        external_key: str,
        scope_fingerprint: str,
    ) -> None:
        """Remember a successfully applied, source-authoritative absence."""
        with self._cursor() as cur:
            cur.execute(
                "INSERT INTO empty_fetch (kind, item_id, change_token, external_key, "
                "scope_fingerprint, observed_at) VALUES (?, ?, ?, ?, ?, ?) "
                "ON CONFLICT(kind, item_id) DO UPDATE SET "
                "change_token = excluded.change_token, "
                "external_key = excluded.external_key, "
                "scope_fingerprint = excluded.scope_fingerprint, "
                "observed_at = excluded.observed_at",
                (
                    kind,
                    item_id,
                    change_token,
                    external_key,
                    scope_fingerprint,
                    datetime.now(tz=UTC).isoformat(),
                ),
            )

    def clear_empty_fetch(self, kind: str, item_id: str) -> None:
        """Forget the absence — the item produced something after all."""
        with self._cursor() as cur:
            cur.execute(
                _DELETE_EMPTY_FETCH_SQL,
                (kind, item_id),
            )

    def item_timeout_attempts(self, kind: str, item_id: str) -> int:
        """Consecutive item timeouts for this ref; 0 when it never timed out."""
        with self._cursor() as cur:
            row = cur.execute(
                _SELECT_ITEM_TIMEOUT_ATTEMPTS_SQL,
                (kind, item_id),
            ).fetchone()
        return 0 if row is None else int(row[0])

    def record_item_timeout(self, *, kind: str, item_id: str) -> int:
        """Count one more item timeout and return the new consecutive count."""
        with self._transaction() as cur:
            cur.execute(
                "INSERT INTO item_timeout (kind, item_id, attempts, observed_at) "
                "VALUES (?, ?, 1, ?) "
                "ON CONFLICT(kind, item_id) DO UPDATE SET "
                "attempts = item_timeout.attempts + 1, observed_at = excluded.observed_at",
                (kind, item_id, datetime.now(tz=UTC).isoformat()),
            )
            row = cur.execute(
                _SELECT_ITEM_TIMEOUT_ATTEMPTS_SQL,
                (kind, item_id),
            ).fetchone()
        return 1 if row is None else int(row[0])

    def clear_item_timeout(self, *, kind: str, item_id: str) -> None:
        """Forget consecutive timeouts — the ref completed without exhausting its budget."""
        with self._cursor() as cur:
            cur.execute(
                _DELETE_ITEM_TIMEOUT_SQL,
                (kind, item_id),
            )

    def _validate_applied_attempt(
        self,
        cur: sqlite3.Cursor,
        expected: MaterializationAttempt,
        episode_id: str,
        document_id: str,
    ) -> None:
        """Prove one immutable accepted attempt under the applied-token transaction."""
        if expected.episode_id != episode_id:
            raise ValueError("visibility commit attempt belongs to another observation")
        row = cur.execute(
            _SELECT_VISIBILITY_ATTEMPT_SQL,
            (expected.episode_id, expected.document_id),
        ).fetchone()
        if row is None or self._materialization_attempt(row) != expected:
            raise ValueError("visibility attempt changed before applied-token commit")
        if (expected.intent_required and expected.receipt is None) or (
            expected.phase == "intent_superseded"
        ):
            raise ValueError("visibility commit requires accepted current intents")
        if expected.document_id == document_id and (
            expected.phase != "payload_submitting" or expected.payload_receipt is None
        ):
            raise ValueError("visibility commit requires the exact accepted payload receipt")

    def _validate_applied_observation(
        self,
        cur: sqlite3.Cursor,
        *,
        kind: str,
        item_id: str,
        library_id: str,
        document_id: str | None,
        change_token: str | None,
        episode_id: str | None,
        attempts: tuple[MaterializationAttempt, ...] | None,
        copy_membership: frozenset[tuple[str, str, str]] | None,
    ) -> None:
        """Check the source episode, its receipts and exact copy set atomically."""
        if episode_id is not None:
            observed = cur.execute(
                "SELECT episode_id, library_id, materialization_token "
                "FROM visibility_observation WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            ).fetchone()
            if observed != (episode_id, library_id, change_token):
                raise ValueError("source observation changed before applied-token commit")
        if attempts is not None:
            if episode_id is None or document_id is None:
                raise ValueError("visibility commit requires observation and current document")
            for expected in attempts:
                self._validate_applied_attempt(cur, expected, episode_id, document_id)
        if copy_membership is not None:
            if episode_id is None or attempts is None:
                raise ValueError("copy membership commit requires observation and attempts")
            actual_copies = set(
                cur.execute(
                    "SELECT ironrag_library_id, ironrag_document_id, external_key "
                    "FROM document_location WHERE kind = ? AND item_id = ?",
                    (kind, item_id),
                ).fetchall()
            )
            if actual_copies != copy_membership:
                raise ValueError("visibility copy membership changed before applied-token commit")

    def upsert(
        self,
        *,
        kind: str,
        item_id: str,
        change_token: str | None,
        external_key: str,
        ironrag_document_id: str | None,
        ironrag_library_id: str,
        expected_visibility_episode: str | None = None,
        expected_materialization_attempts: tuple[MaterializationAttempt, ...] | None = None,
        expected_copy_membership: frozenset[tuple[str, str, str]] | None = None,
    ) -> None:
        """Advance source/current-route state and optionally register its copy.

        ``ironrag_document_id=None`` means no copy was observed by this call;
        it never erases an older location. This is what preserves ownership
        across a route move until the old copy is actually deleted.
        """
        now = datetime.now(tz=UTC).isoformat()
        with self._transaction() as cur:
            self._validate_applied_observation(
                cur,
                kind=kind,
                item_id=item_id,
                library_id=ironrag_library_id,
                document_id=ironrag_document_id,
                change_token=change_token,
                episode_id=expected_visibility_episode,
                attempts=expected_materialization_attempts,
                copy_membership=expected_copy_membership,
            )
            cur.execute(
                """
                INSERT INTO cursor (
                    kind, item_id, change_token, external_key,
                    current_library_id, last_pushed_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(kind, item_id) DO UPDATE SET
                    change_token = excluded.change_token,
                    external_key = excluded.external_key,
                    current_library_id = excluded.current_library_id,
                    last_pushed_at = excluded.last_pushed_at
                """,
                (
                    kind,
                    item_id,
                    change_token,
                    external_key,
                    ironrag_library_id,
                    now,
                ),
            )
            if ironrag_document_id is not None:
                self._record_location(
                    cur,
                    kind=kind,
                    item_id=item_id,
                    external_key=external_key,
                    ironrag_document_id=ironrag_document_id,
                    ironrag_library_id=ironrag_library_id,
                    observed_at=now,
                )
                cur.execute(
                    _DELETE_EMPTY_FETCH_SQL,
                    (kind, item_id),
                )

    def record_document_location(
        self,
        *,
        kind: str,
        item_id: str,
        external_key: str,
        ironrag_document_id: str,
        ironrag_library_id: str,
    ) -> None:
        """Persist exact-key ownership without changing source/current route."""
        now = datetime.now(tz=UTC).isoformat()
        with self._transaction() as cur:
            self._record_location(
                cur,
                kind=kind,
                item_id=item_id,
                external_key=external_key,
                ironrag_document_id=ironrag_document_id,
                ironrag_library_id=ironrag_library_id,
                observed_at=now,
            )
            cur.execute(
                _DELETE_EMPTY_FETCH_SQL,
                (kind, item_id),
            )

    def delete_cursor(self, kind: str, item_id: str) -> None:
        """Delete source/current-route state without discarding ownership.

        A dropped cursor also drops the ref's consecutive-timeout count: a page
        deleted upstream and recreated under the same ``item_id`` must start on
        the full budget rather than inherit the old ref's capped one.
        """
        with self._transaction() as cur:
            cur.execute(
                "DELETE FROM cursor WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            )
            cur.execute(
                _DELETE_ITEM_TIMEOUT_SQL,
                (kind, item_id),
            )

    def restore(self, row: CursorRow | None, *, kind: str, item_id: str) -> None:
        """Restore one cursor row exactly, or delete it when ``row`` is absent."""
        if row is None:
            self.delete_cursor(kind, item_id)
            return
        if row.kind != kind or row.item_id != item_id:
            raise ValueError("cursor restore target does not match row identity")
        with self._cursor() as cur:
            cur.execute(
                """
                INSERT INTO cursor (
                    kind, item_id, change_token, external_key,
                    current_library_id, last_pushed_at
                ) VALUES (?, ?, ?, ?, ?, ?)
                ON CONFLICT(kind, item_id) DO UPDATE SET
                    change_token = excluded.change_token,
                    external_key = excluded.external_key,
                    current_library_id = excluded.current_library_id,
                    last_pushed_at = excluded.last_pushed_at
                """,
                (
                    row.kind,
                    row.item_id,
                    row.change_token,
                    row.external_key,
                    row.current_library_id,
                    row.last_pushed_at,
                ),
            )

    def delete_by_library(self, ironrag_library_id: str) -> int:
        """Drop state naming a library that IronRAG reports no longer exists.

        Used when the library itself is gone: its documents went with it, so
        locations there are already absent, dependent claims on copies there
        are moot, and a current-route cursor pointing there cannot describe
        applied state. Removing them prevents the registry from nominating the
        deleted library on every later sweep.

        Dropping a parent's claims narrows its proven set, so the census that
        proved that set is withdrawn together with them: the parent must fetch
        again before its remaining copies count as proven. Keeping the census
        would let one wrong library verdict present a parent as proven with a
        membership it never enumerated, and the dependent phase would reap the
        copies it still holds elsewhere.
        """
        with self._transaction() as cur:
            cur.execute(
                "DELETE FROM document_location WHERE ironrag_library_id = ?",
                (ironrag_library_id,),
            )
            cur.execute(
                """
                DELETE FROM parent_census WHERE EXISTS (
                    SELECT 1 FROM dependent_membership
                    WHERE dependent_membership.parent_kind = parent_census.kind
                      AND dependent_membership.parent_item_id = parent_census.item_id
                      AND dependent_membership.ironrag_library_id = ?
                )
                """,
                (ironrag_library_id,),
            )
            cur.execute(
                "DELETE FROM dependent_membership WHERE ironrag_library_id = ?",
                (ironrag_library_id,),
            )
            # Runs while the doomed cursor rows still exist, so a ref whose
            # current route was that library loses its timeout count with it.
            cur.execute(
                """
                DELETE FROM item_timeout WHERE EXISTS (
                    SELECT 1 FROM cursor
                    WHERE cursor.kind = item_timeout.kind
                      AND cursor.item_id = item_timeout.item_id
                      AND cursor.current_library_id = ?
                )
                """,
                (ironrag_library_id,),
            )
            cur.execute(
                "DELETE FROM cursor WHERE current_library_id = ?",
                (ironrag_library_id,),
            )
            return int(cur.rowcount)

    @staticmethod
    def _claim_member(
        cur: sqlite3.Cursor,
        *,
        parent_kind: str,
        parent_item_id: str,
        member: MemberTarget,
        observed_at: str,
    ) -> None:
        cur.execute(
            """
            INSERT INTO dependent_membership (
                parent_kind, parent_item_id, kind, item_id, external_key,
                ironrag_library_id, observed_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(parent_kind, parent_item_id, kind, item_id, ironrag_library_id)
            DO UPDATE SET
                external_key = excluded.external_key,
                observed_at = excluded.observed_at
            """,
            (
                parent_kind,
                parent_item_id,
                member.kind,
                member.item_id,
                member.external_key,
                member.ironrag_library_id,
                observed_at,
            ),
        )

    @staticmethod
    def _claims_of_parent(
        cur: sqlite3.Cursor, parent_kind: str, parent_item_id: str
    ) -> list[DependentClaim]:
        rows = cur.execute(
            "SELECT parent_kind, parent_item_id, kind, item_id, external_key, "
            "ironrag_library_id, observed_at FROM dependent_membership "
            "WHERE parent_kind = ? AND parent_item_id = ? "
            "ORDER BY kind, item_id, ironrag_library_id",
            (parent_kind, parent_item_id),
        ).fetchall()
        return [DependentClaim(*row) for row in rows]

    def claim_member(self, *, parent_kind: str, parent_item_id: str, member: MemberTarget) -> None:
        """Record, before its push, that a parent expects one dependent copy."""
        with self._transaction() as cur:
            self._claim_member(
                cur,
                parent_kind=parent_kind,
                parent_item_id=parent_item_id,
                member=member,
                observed_at=datetime.now(tz=UTC).isoformat(),
            )

    def record_parent_census(
        self,
        *,
        kind: str,
        item_id: str,
        change_token: str | None,
        dependents_token: str | None,
        members: Iterable[MemberTarget],
    ) -> list[DependentClaim]:
        """Make ``members`` the parent's complete dependent set at these tokens.

        Listed members are (re)asserted, every other claim of this parent is
        released and returned as membership changes, not proof of orphanhood.
        Physical cleanup belongs to the completeness-gated dependent reaper.
        Census and claims move in one transaction: a census row
        never describes a set the membership table does not hold.
        """
        now = datetime.now(tz=UTC).isoformat()
        listed = tuple(members)
        keep = {(member.kind, member.item_id, member.ironrag_library_id) for member in listed}
        with self._transaction() as cur:
            released = [
                claim
                for claim in self._claims_of_parent(cur, kind, item_id)
                if (claim.kind, claim.item_id, claim.ironrag_library_id) not in keep
            ]
            for claim in released:
                cur.execute(
                    "DELETE FROM dependent_membership WHERE parent_kind = ? "
                    "AND parent_item_id = ? AND kind = ? AND item_id = ? "
                    "AND ironrag_library_id = ?",
                    (kind, item_id, claim.kind, claim.item_id, claim.ironrag_library_id),
                )
            for member in listed:
                self._claim_member(
                    cur,
                    parent_kind=kind,
                    parent_item_id=item_id,
                    member=member,
                    observed_at=now,
                )
            cur.execute(
                """
                INSERT INTO parent_census (
                    kind, item_id, change_token, dependents_token, observed_at
                ) VALUES (?, ?, ?, ?, ?)
                ON CONFLICT(kind, item_id) DO UPDATE SET
                    change_token = excluded.change_token,
                    dependents_token = excluded.dependents_token,
                    observed_at = excluded.observed_at
                """,
                (kind, item_id, change_token, dependents_token, now),
            )
        return released

    def release_parent(self, kind: str, item_id: str) -> list[DependentClaim]:
        """Withdraw a parent's census and every claim it holds."""
        with self._transaction() as cur:
            released = self._claims_of_parent(cur, kind, item_id)
            cur.execute(
                "DELETE FROM dependent_membership WHERE parent_kind = ? AND parent_item_id = ?",
                (kind, item_id),
            )
            cur.execute(
                "DELETE FROM parent_census WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            )
        return released

    def has_claims(self, kind: str, item_id: str, ironrag_library_id: str) -> bool:
        """Whether any parent still expects this dependent copy in this library."""
        with self._cursor() as cur:
            row = cur.execute(
                "SELECT 1 FROM dependent_membership WHERE kind = ? AND item_id = ? "
                "AND ironrag_library_id = ? LIMIT 1",
                (kind, item_id, ironrag_library_id),
            ).fetchone()
        return row is not None

    def claims_of_parent(self, kind: str, item_id: str) -> list[DependentClaim]:
        with self._cursor() as cur:
            return self._claims_of_parent(cur, kind, item_id)

    def parent_census(self, kind: str, item_id: str) -> ParentCensus | None:
        with self._cursor() as cur:
            row = cur.execute(
                "SELECT kind, item_id, change_token, dependents_token, observed_at "
                "FROM parent_census WHERE kind = ? AND item_id = ?",
                (kind, item_id),
            ).fetchone()
        return None if row is None else ParentCensus(*row)

    def parents_with_census(self, kind: str) -> list[ParentCensus]:
        with self._cursor() as cur:
            rows = cur.execute(
                "SELECT kind, item_id, change_token, dependents_token, observed_at "
                "FROM parent_census WHERE kind = ? ORDER BY item_id",
                (kind,),
            ).fetchall()
        return [ParentCensus(*row) for row in rows]

    def parents_with_claims(self, kind: str) -> set[str]:
        """Item ids of ``kind`` that still hold at least one dependent claim.

        A parent can hold claims without a census: the claim is written before
        the dependent's push, and a fetch that never completed leaves the claim
        without the proof. Such a parent owns copies, so it is neither
        forgettable in silence nor proven.
        """
        with self._cursor() as cur:
            rows = cur.execute(
                "SELECT DISTINCT parent_item_id FROM dependent_membership WHERE parent_kind = ?",
                (kind,),
            ).fetchall()
        return {row[0] for row in rows}

    def known_parents(self, kind: str) -> set[str]:
        """Item ids of ``kind`` this store knows as a parent, proven or not."""
        return {census.item_id for census in self.parents_with_census(kind)} | (
            self.parents_with_claims(kind)
        )

    def items_of_kind(self, kind: str) -> list[CursorRow]:
        with self._cursor() as cur:
            rows = cur.execute(
                "SELECT kind, item_id, change_token, external_key, "
                "current_library_id, last_pushed_at "
                "FROM cursor WHERE kind = ?",
                (kind,),
            ).fetchall()
        return [CursorRow(*r) for r in rows]
