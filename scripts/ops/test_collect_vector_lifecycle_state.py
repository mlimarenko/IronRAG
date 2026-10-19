import json
import os
import unittest
from unittest import mock

from apps.api.benchmarks.dependency_identity import database_endpoint_sha256

from scripts.ops.collect_vector_lifecycle_state import (
    QUERY_VISIBLE_RELATIONS,
    database_environment,
    normalized_inventory,
)


LIBRARY_ID = "11111111-1111-1111-1111-111111111111"


def inventory_rows(vector_identity: str = "vector-key-1") -> list[list[str]]:
    rows = [
        [
            "baseline_metadata",
            "2026-08-26T00:00:00+00:00",
            "ironrag",
            "16384",
            "7401234567890123456",
            "17.5",
            "10:20:",
            "off",
        ],
        [
            "relation_manifest_reconciliation",
            "knowledge_chunk_vector_d768",
            "chunk",
            "768",
            "768",
            "chunk_embedding",
            "1",
            "1",
            "aligned",
        ],
        ["physical_relation", "public", "knowledge_chunk_vector_d768", "chunk", "80", "100", "40", "140"],
        ["hnsw_index", "knowledge_chunk_vector_d768", "idx_hnsw", "30"],
        ["relation_actual_count", "knowledge_chunk_vector_d768", "1"],
        ["vector_identity_relation", "knowledge_chunk_vector_d768", "1", ""],
        ["vector_identity", "knowledge_chunk_vector_d768", vector_identity, "current_readable"],
    ]
    rows.extend(
        ["logical_identity", relation, "__query_visible_schema_v1__"]
        for relation in sorted(QUERY_VISIBLE_RELATIONS)
    )
    rows.append([
        "physical_configuration_identity",
        "vector_relation",
        '{"fixture":"physical-config"}',
    ])
    return rows


class VectorLifecycleStateCollectorTests(unittest.TestCase):
    def test_normalizes_one_snapshot_without_persisting_raw_database_identifier(self) -> None:
        document = normalized_inventory(
            inventory_rows(),
            role="baseline",
            library_id=LIBRARY_ID,
            source_sql_sha256="a" * 64,
        )

        inventory = document["inventory"]
        identity = document["dataStateIdentity"]
        self.assertEqual(identity["vectorRows"]["currentReadable"], 1)
        self.assertEqual(identity["physicalBytes"]["total"], 140)
        self.assertEqual(identity["physicalBytes"]["hnsw"], 30)
        self.assertEqual(identity["libraryId"], LIBRARY_ID)
        self.assertNotIn("systemIdentifier", inventory["databaseIdentity"])
        self.assertEqual(len(inventory["databaseIdentity"]["systemIdentifierSha256"]), 64)

    def test_exact_vector_identity_detects_replacement_with_unchanged_counts(self) -> None:
        first = normalized_inventory(
            inventory_rows("vector-key-1"),
            role="baseline",
            library_id=LIBRARY_ID,
            source_sql_sha256="a" * 64,
        )
        replaced = normalized_inventory(
            inventory_rows("vector-key-2"),
            role="baseline",
            library_id=LIBRARY_ID,
            source_sql_sha256="a" * 64,
        )

        self.assertEqual(
            first["dataStateIdentity"]["vectorRows"],
            replaced["dataStateIdentity"]["vectorRows"],
        )
        self.assertNotEqual(
            first["dataStateIdentity"]["vectorIdentitySha256"],
            replaced["dataStateIdentity"]["vectorIdentitySha256"],
        )
        self.assertNotEqual(
            first["dataStateIdentity"]["physicalVectorStateSha256"],
            replaced["dataStateIdentity"]["physicalVectorStateSha256"],
        )

    def test_database_url_is_mapped_to_pg_environment_without_url_argument(self) -> None:
        database_url = "postgresql://bench:p%40ss@db.example:5544/ironrag?sslmode=verify-full"
        mapping = {"database": {database_endpoint_sha256(database_url): 15432}}
        with mock.patch.dict(
            os.environ, {"IRONRAG_DEPENDENCY_PIN_PROXIES": json.dumps(mapping)}
        ):
            environment = database_environment(database_url)

        self.assertEqual(environment["PGHOST"], "127.0.0.1")
        self.assertEqual(environment["PGPORT"], "15432")
        self.assertEqual(environment["PGDATABASE"], "ironrag")
        self.assertEqual(environment["PGUSER"], "bench")
        self.assertEqual(environment["PGPASSWORD"], "p@ss")
        self.assertEqual(environment["PGSSLMODE"], "disable")

    def test_deployment_identity_distinguishes_databases_in_one_cluster(self) -> None:
        first_rows = inventory_rows()
        second_rows = inventory_rows()
        second_rows[0][2] = "ironrag_test"
        second_rows[0][3] = "16385"

        first = normalized_inventory(
            first_rows,
            role="baseline",
            library_id=LIBRARY_ID,
            source_sql_sha256="a" * 64,
        )
        second = normalized_inventory(
            second_rows,
            role="baseline",
            library_id=LIBRARY_ID,
            source_sql_sha256="a" * 64,
        )

        self.assertNotEqual(
            first["inventory"]["databaseIdentity"]["deploymentId"],
            second["inventory"]["databaseIdentity"]["deploymentId"],
        )

    def test_rejects_incomplete_or_mismatched_stream(self) -> None:
        # Both streams are built before their assertion block, so inside it only
        # the call under test can raise: a broken fixture would otherwise read
        # as the rejection being asserted.
        metadata_only = inventory_rows()[0:1]
        without_vector_identity = [row for row in inventory_rows() if row[0] != "vector_identity"]
        with self.assertRaisesRegex(ValueError, "no logical identity"):
            normalized_inventory(
                metadata_only,
                role="baseline",
                library_id=LIBRARY_ID,
                source_sql_sha256="a" * 64,
            )
        with self.assertRaisesRegex(ValueError, "identity stream is incomplete"):
            normalized_inventory(
                without_vector_identity,
                role="baseline",
                library_id=LIBRARY_ID,
                source_sql_sha256="a" * 64,
            )
    def test_rejects_a_section_the_collector_does_not_know(self) -> None:
        # The section table is the only place that decides what a row means, so
        # an unnamed section must be refused rather than silently dropped: a
        # dropped section would leave the evidence complete-looking and short.
        rows = inventory_rows()
        rows.append(["vector_identity_shadow", "knowledge_chunk_vector_d768", "x", "y"])

        with self.assertRaisesRegex(ValueError, "unknown section: vector_identity_shadow"):
            normalized_inventory(
                rows,
                role="baseline",
                library_id=LIBRARY_ID,
                source_sql_sha256="a" * 64,
            )


if __name__ == "__main__":
    unittest.main()
