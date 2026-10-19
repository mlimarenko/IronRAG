#!/usr/bin/env python3
"""Collect one normalized, MVCC-consistent vector lifecycle evidence artifact."""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import pathlib
import subprocess
import sys
import urllib.parse
import uuid
from collections import Counter
from typing import Any, Iterable, Sequence


REPOSITORY_ROOT = pathlib.Path(__file__).resolve().parents[2]
if str(REPOSITORY_ROOT) not in sys.path:
    sys.path.insert(0, str(REPOSITORY_ROOT))

from apps.api.benchmarks.lifecycle_state import (  # noqa: E402
    SCHEMA_VERSION,
    build_data_state_identity,
    stable_json_sha256,
)
from apps.api.benchmarks.dependency_identity import database_endpoint_sha256  # noqa: E402


DEPLOYMENT_ID_NAMESPACE = uuid.UUID("a3f1c2b4-5d6e-4f80-9a1b-2c3d4e5f6071")
DEFAULT_SQL_PATH = pathlib.Path(__file__).with_name("vector-lifecycle-inventory.sql")
CATEGORY_NAMES = {
    "current_readable": "currentReadable",
    "protected_active_ingest": "protectedActiveIngest",
    "superseded_readable_history": "supersededReadableHistory",
    "inconsistent_or_orphan": "inconsistentOrOrphan",
}
QUERY_VISIBLE_RELATIONS = {
    "catalog_library", "content_document", "content_document_head", "content_revision",
    "content_chunk", "knowledge_document", "knowledge_revision", "knowledge_chunk",
    "knowledge_structured_revision", "knowledge_structured_block", "knowledge_technical_fact",
    "knowledge_entity", "knowledge_chunk_entity_mention", "knowledge_evidence",
    "knowledge_evidence_entity_support", "knowledge_evidence_relation_support",
    "knowledge_relation", "runtime_graph_snapshot", "runtime_graph_node",
    "runtime_graph_edge", "runtime_graph_evidence", "runtime_graph_community",
    "runtime_graph_canonical_summary", "knowledge_vector_relation_manifest",
    "ai_binding", "ai_account", "ai_model_catalog",
    "ai_provider_catalog",
}


def framed_hash_update(hasher: Any, *values: str) -> None:
    for value in values:
        encoded = value.encode("utf-8")
        hasher.update(len(encoded).to_bytes(8, "big"))
        hasher.update(encoded)


def database_environment(database_url: str) -> dict[str, str]:
    parsed = urllib.parse.urlsplit(database_url)
    if parsed.scheme not in {"postgres", "postgresql"}:
        raise ValueError("database URL must use postgres or postgresql scheme")
    if not parsed.hostname or not parsed.path.strip("/"):
        raise ValueError("database URL must include host and database")
    environment = dict(os.environ)
    endpoint_sha256 = database_endpoint_sha256(database_url)
    try:
        proxy_mapping = json.loads(os.environ.get("IRONRAG_DEPENDENCY_PIN_PROXIES", "{}"))
        local_port = proxy_mapping["database"][endpoint_sha256]
    except (json.JSONDecodeError, KeyError, TypeError) as error:
        raise ValueError("PostgreSQL exact leaf pin proxy is not active") from error
    environment.update(
        PGHOST="127.0.0.1",
        PGPORT=str(local_port),
        PGDATABASE=urllib.parse.unquote(parsed.path.strip("/")),
    )
    if parsed.username:
        environment["PGUSER"] = urllib.parse.unquote(parsed.username)
    if parsed.password:
        environment["PGPASSWORD"] = urllib.parse.unquote(parsed.password)
    query = urllib.parse.parse_qs(parsed.query)
    if query.get("sslmode") != ["verify-full"]:
        raise ValueError("database URL must require exactly one sslmode=verify-full")
    environment["PGSSLMODE"] = "disable"
    environment.pop("PGSSLROOTCERT", None)
    environment["PGCONNECT_TIMEOUT"] = "10"
    return environment


class InventoryStream:
    """One collector output stream, accumulated section by section.

    Every section owns a method and the dispatch table below is the only place
    that decides what a row means, so an unknown section is rejected in exactly
    one spot and a new section is one table entry rather than another branch in
    a single pass over the rows.
    """

    def __init__(self, *, library_id: str) -> None:
        self.library_id = library_id
        self.metadata: list[str] | None = None
        self.identity_categories: Counter[str] = Counter()
        self.physical_table_bytes = 0
        self.physical_index_bytes = 0
        self.hnsw_bytes = 0
        self.logical_hasher = hashlib.sha256()
        self.provider_configuration_hasher = hashlib.sha256()
        self.physical_configuration_hasher = hashlib.sha256()
        self.vector_hasher = hashlib.sha256()
        self.category_hashers = {name: hashlib.sha256() for name in CATEGORY_NAMES.values()}
        self.head_bound_hasher = hashlib.sha256()
        self.entity_vector_hasher = hashlib.sha256()
        self.logical_count = 0
        self.provider_configuration_rows = 0
        self.physical_configuration_rows = 0
        self.logical_relation_rows: Counter[str] = Counter()
        self.head_bound_count = 0
        self.entity_vector_count = 0
        self.manifest_claimed_rows: Counter[str] = Counter()
        self.actual_relation_rows: dict[str, int] = {}
        self.physical_relations: set[str] = set()
        self.physical_chunk_relations: set[str] = set()
        self.vector_identity_expected: dict[str, int] = {}
        self.vector_identity_observed: Counter[str] = Counter()
        self.reconciliation_states: list[str] = []
        self.entity_inconsistent_rows = 0
        self._sections = {
            "baseline_metadata": self._baseline_metadata,
            "physical_relation": self._physical_relation,
            "relation_manifest_reconciliation": self._relation_manifest_reconciliation,
            "relation_actual_count": self._relation_actual_count,
            "hnsw_index": self._hnsw_index,
            "entity_consistency_library": self._entity_consistency_library,
            "logical_identity": self._logical_identity,
            "physical_configuration_identity": self._physical_configuration_identity,
            "vector_identity": self._vector_identity,
            "vector_identity_relation": self._vector_identity_relation,
            "entity_vector_identity": self._entity_vector_identity,
        }

    def consume(self, rows: Iterable[list[str]]) -> None:
        for row in rows:
            if not row:
                continue
            section = self._sections.get(row[0])
            if section is None:
                raise ValueError(f"collector output contains unknown section: {row[0]}")
            section(row)

    def _baseline_metadata(self, row: list[str]) -> None:
        if self.metadata is not None or len(row) != 8:
            raise ValueError("invalid or duplicate baseline_metadata row")
        self.metadata = row

    def _physical_relation(self, row: list[str]) -> None:
        if len(row) < 8:
            raise ValueError("invalid physical_relation row")
        self.physical_table_bytes += int(row[5])
        self.physical_index_bytes += int(row[6])
        self.physical_relations.add(row[2])
        if row[3] == "chunk":
            self.physical_chunk_relations.add(row[2])

    def _relation_manifest_reconciliation(self, row: list[str]) -> None:
        if len(row) != 9:
            raise ValueError("invalid relation_manifest_reconciliation row")
        self.reconciliation_states.append(row[8])
        self.manifest_claimed_rows[row[1]] += int(row[7])

    def _relation_actual_count(self, row: list[str]) -> None:
        if len(row) != 3 or row[1] in self.actual_relation_rows:
            raise ValueError("invalid or duplicate relation_actual_count row")
        self.actual_relation_rows[row[1]] = int(row[2])

    def _hnsw_index(self, row: list[str]) -> None:
        if len(row) != 4:
            raise ValueError("invalid hnsw_index row")
        self.hnsw_bytes += int(row[3])

    def _entity_consistency_library(self, row: list[str]) -> None:
        if len(row) < 7:
            raise ValueError("invalid entity_consistency_library row")
        if row[3] == self.library_id and row[5] == "inconsistent_or_orphan":
            self.entity_inconsistent_rows += int(row[6])

    def _logical_identity(self, row: list[str]) -> None:
        if len(row) != 3:
            raise ValueError("invalid logical_identity row")
        framed_hash_update(self.logical_hasher, row[1], row[2])
        self.logical_count += 1
        self.logical_relation_rows[row[1]] += 1
        if row[1].startswith("ai_"):
            framed_hash_update(self.provider_configuration_hasher, row[1], row[2])
            self.provider_configuration_rows += 1

    def _physical_configuration_identity(self, row: list[str]) -> None:
        if len(row) != 3:
            raise ValueError("invalid physical_configuration_identity row")
        framed_hash_update(self.physical_configuration_hasher, row[1], row[2])
        self.physical_configuration_rows += 1

    def _vector_identity(self, row: list[str]) -> None:
        if len(row) != 4:
            raise ValueError("invalid vector_identity row")
        framed_hash_update(self.vector_hasher, row[1], row[2], row[3])
        category = CATEGORY_NAMES.get(row[3])
        if category is None:
            raise ValueError(f"unknown vector identity lifecycle category: {row[3]}")
        framed_hash_update(self.category_hashers[category], row[1], row[2], row[3])
        self.identity_categories[category] += 1
        self.vector_identity_observed[row[1]] += 1
        if row[3] in {"current_readable", "protected_active_ingest"}:
            framed_hash_update(self.head_bound_hasher, row[1], row[2], row[3])
            self.head_bound_count += 1

    def _vector_identity_relation(self, row: list[str]) -> None:
        if len(row) != 4 or row[1] in self.vector_identity_expected:
            raise ValueError("invalid or duplicate vector_identity_relation row")
        self.vector_identity_expected[row[1]] = int(row[2])

    def _entity_vector_identity(self, row: list[str]) -> None:
        if len(row) != 4:
            raise ValueError("invalid entity_vector_identity row")
        if row[3] != "consistent":
            raise ValueError("entity vector identity is inconsistent or orphaned")
        framed_hash_update(self.entity_vector_hasher, row[1], row[2], row[3])
        self.entity_vector_count += 1

    def validated_metadata(self) -> list[str]:
        """Return the metadata row, rejecting a stream that describes no one state.

        Order matters and is the order of the original single pass: the tests
        assert on which complaint a truncated stream raises.
        """
        if self.metadata is None:
            raise ValueError("collector output is missing baseline_metadata")
        if self.logical_count == 0:
            raise ValueError("collector output has no logical identity rows")
        if self.physical_configuration_rows == 0:
            raise ValueError("collector output has no physical configuration identity rows")
        if set(self.logical_relation_rows) != QUERY_VISIBLE_RELATIONS:
            raise ValueError("collector query-visible relation manifest is incomplete")
        if self.entity_inconsistent_rows != 0:
            raise ValueError("collector found inconsistent or orphaned entity vectors")
        self._validate_vector_relations()
        return self.metadata

    def _validate_vector_relations(self) -> None:
        if (
            not self.physical_relations
            or set(self.actual_relation_rows) != self.physical_relations
        ):
            raise ValueError("actual relation counts do not cover every physical vector relation")
        if set(self.manifest_claimed_rows) != self.physical_relations:
            raise ValueError("manifest does not cover every physical vector relation")
        mismatched_relations = sorted(
            relation
            for relation in self.physical_relations
            if self.manifest_claimed_rows[relation] != self.actual_relation_rows[relation]
        )
        if any(state != "aligned" for state in self.reconciliation_states):
            raise ValueError("vector relation manifest is not aligned with physical relations")
        if mismatched_relations:
            raise ValueError("manifest claimed row counts do not match physical rows")
        if self._vector_identity_stream_is_partial():
            raise ValueError("exact vector identity stream is incomplete")

    def _vector_identity_stream_is_partial(self) -> bool:
        if set(self.vector_identity_expected) != self.physical_chunk_relations:
            return True
        return any(
            self.vector_identity_expected[relation] != self.vector_identity_observed[relation]
            for relation in self.physical_chunk_relations
        )

    def inventory(self, metadata: list[str], *, source_sql_sha256: str) -> dict[str, object]:
        # The exact identity stream is the canonical machine state. Aggregate SQL
        # is independently checked above and cannot silently weaken the result.
        vector_rows = {key: self.identity_categories[key] for key in CATEGORY_NAMES.values()}
        vector_rows["total"] = sum(vector_rows.values())
        system_identifier = metadata[4]
        deployment_seed = "\0".join((system_identifier, metadata[3], metadata[2]))
        return {
            "collector": {
                "name": "ironrag-vector-lifecycle-collector",
                "schemaVersion": SCHEMA_VERSION,
                "sourceSqlSha256": source_sql_sha256,
            },
            "databaseIdentity": {
                "deploymentId": str(uuid.uuid5(DEPLOYMENT_ID_NAMESPACE, deployment_seed)),
                "systemIdentifierSha256": hashlib.sha256(
                    system_identifier.encode("utf-8")
                ).hexdigest(),
                "databaseName": metadata[2],
                "databaseOid": metadata[3],
            },
            "transactionSnapshotSha256": hashlib.sha256(metadata[6].encode("utf-8")).hexdigest(),
            "serverVersion": metadata[5],
            "libraryId": self.library_id,
            "logicalDataSha256": self.logical_hasher.hexdigest(),
            "providerConfigurationSha256": self.provider_configuration_hasher.hexdigest(),
            "providerConfigurationRows": self.provider_configuration_rows,
            "physicalConfigurationSha256": self.physical_configuration_hasher.hexdigest(),
            "physicalConfigurationRows": self.physical_configuration_rows,
            "logicalIdentityRows": self.logical_count,
            "queryVisibleSchemaVersion": 1,
            "queryVisibleRelationRows": dict(sorted(self.logical_relation_rows.items())),
            "vectorRows": vector_rows,
            "vectorIdentitySha256": self.vector_hasher.hexdigest(),
            "vectorCategoryIdentitySha256": {
                category: hasher.hexdigest()
                for category, hasher in self.category_hashers.items()
            },
            "headBoundVectorIdentitySha256": self.head_bound_hasher.hexdigest(),
            "headBoundVectorRows": self.head_bound_count,
            "entityVectorIdentitySha256": self.entity_vector_hasher.hexdigest(),
            "entityVectorRows": self.entity_vector_count,
            "physicalBytes": {
                "table": self.physical_table_bytes,
                "indexes": self.physical_index_bytes,
                "hnsw": self.hnsw_bytes,
                "total": self.physical_table_bytes + self.physical_index_bytes,
            },
        }


def normalized_inventory(
    rows: Iterable[list[str]],
    *,
    role: str,
    library_id: str,
    source_sql_sha256: str,
) -> dict[str, object]:
    stream = InventoryStream(library_id=library_id)
    stream.consume(rows)
    metadata = stream.validated_metadata()
    document: dict[str, object] = {
        "schemaVersion": SCHEMA_VERSION,
        "role": role,
        "observedAt": metadata[1],
        "inventory": stream.inventory(metadata, source_sql_sha256=source_sql_sha256),
    }
    document["dataStateIdentity"] = build_data_state_identity(document)
    document["collectorDocumentSha256"] = stable_json_sha256(document)
    return document


def psql_rows(
    sql_path: pathlib.Path,
    *,
    library_id: str,
    environment: dict[str, str],
) -> Iterable[list[str]]:
    command = [
        "psql",
        "-X",
        "--csv",
        "--tuples-only",
        "--set=ON_ERROR_STOP=1",
        "--set=machine_identity=1",
        f"--set=library_id={library_id}",
        f"--file={sql_path}",
    ]
    process = subprocess.Popen(
        command,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
        env=environment,
    )
    if process.stdout is None:
        raise RuntimeError("psql stdout pipe was not created")
    for line in process.stdout:
        parsed = next(csv.reader([line]))
        if parsed and parsed[0] not in {"BEGIN", "COMMIT", "SET"}:
            yield parsed
    stderr = process.stderr.read() if process.stderr is not None else ""
    return_code = process.wait()
    if return_code != 0:
        raise RuntimeError(f"psql inventory failed with exit {return_code}: {stderr.strip()}")


def parse_args(argv: Sequence[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--role", required=True, choices=("baseline", "candidate"))
    parser.add_argument("--library-id", required=True, type=uuid.UUID)
    parser.add_argument("--database-url-env", default="DATABASE_URL")
    parser.add_argument("--output", type=pathlib.Path, required=True)
    return parser.parse_args(argv)


def write_private_json(path: pathlib.Path, document: dict[str, object]) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{uuid.uuid4().hex}.tmp")
    descriptor = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8") as handle:
            json.dump(document, handle, ensure_ascii=False, indent=2, allow_nan=False)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def collect_inventory(
    *,
    role: str,
    library_id: str,
    database_url: str,
) -> dict[str, object]:
    sql_path = DEFAULT_SQL_PATH.resolve()
    source_sql_sha256 = hashlib.sha256(sql_path.read_bytes()).hexdigest()
    raw_rows = list(
        psql_rows(
            sql_path, library_id=library_id, environment=database_environment(database_url)
        )
    )
    document = normalized_inventory(
        raw_rows,
        role=role,
        library_id=library_id,
        source_sql_sha256=source_sql_sha256,
    )
    document["rawEvidence"] = {
        "format": "psql-csv-decoded-rows-v1",
        "rows": raw_rows,
        "sha256": stable_json_sha256(raw_rows),
    }
    document["collectorDocumentSha256"] = stable_json_sha256(
        {key: value for key, value in document.items() if key != "collectorDocumentSha256"}
    )
    return document


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    database_url = os.environ.get(args.database_url_env, "")
    if not database_url:
        print(f"missing database URL environment variable: {args.database_url_env}", file=sys.stderr)
        return 1
    try:
        document = collect_inventory(
            role=args.role,
            library_id=str(args.library_id),
            database_url=database_url,
        )
    except (OSError, ValueError, RuntimeError) as error:
        print(f"vector lifecycle collection failed: {error}", file=sys.stderr)
        return 1
    write_private_json(args.output, document)
    print(f"wrote vector lifecycle evidence: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
