#!/usr/bin/env python3
"""Contract tests for the content-duplicate repair script.

These checks intentionally focus on safety properties that are easy to lose in
an operator SQL script: atomic projection updates, generation fencing, and a
dry-run default. Behavioural SQL execution is covered by the normal PostgreSQL
smoke invocation used when changing the script.
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("heal-content-duplicates.sql")


def normalized_sql() -> str:
    return re.sub(r"\s+", " ", SCRIPT.read_text(encoding="utf-8").lower()).strip()


class HealContentDuplicatesContractTests(unittest.TestCase):
    def test_defaults_to_dry_run_and_owns_its_transaction(self) -> None:
        sql = normalized_sql()

        self.assertRegex(sql, r"\\set\s+apply\s+false")
        self.assertIn("begin;", sql)
        self.assertRegex(sql, r"\\if\s+:apply\s+commit;.*?\\else\s+rollback;")

    def test_knowledge_tombstone_is_scoped_to_selected_duplicate_targets(self) -> None:
        sql = normalized_sql()

        self.assertRegex(sql, r"selected_tombstones\s+as\s+materialized")
        self.assertRegex(
            sql,
            r"tombstoned_knowledge\s+as\s*\(\s*update\s+(?:public\.)?knowledge_document",
        )
        self.assertRegex(sql, r"from\s+knowledge_targets\s+as\s+target")
        self.assertRegex(sql, r"knowledge\.document_id\s*=\s*target\.document_id")
        self.assertRegex(sql, r"knowledge\.library_id\s*=\s*target\.library_id")
        self.assertRegex(sql, r"document_state\s*=\s*'deleted'")
        self.assertRegex(sql, r"active_revision_id\s*=\s*null")
        self.assertRegex(sql, r"deleted_at\s*=\s*target\.deleted_at")

    def test_selection_can_repair_residue_from_an_earlier_script_run(self) -> None:
        sql = normalized_sql()

        self.assertRegex(sql, r"canonical_is_live")
        self.assertRegex(
            sql,
            r"selected_duplicates\s+as\s+materialized.*?not\s+duplicate\.canonical_is_live\s+or\s+duplicate\.rank_within_group\s*>\s*1",
        )

    def test_generation_bump_is_derived_only_from_real_visibility_changes(self) -> None:
        sql = normalized_sql()

        self.assertRegex(
            sql,
            r"changed_libraries\s+as\s+materialized\s*\(\s*select\s+library_id\s+from\s+canonical_answer_changes\s+union\s+select\s+library_id\s+from\s+knowledge_answer_changes",
        )
        self.assertRegex(
            sql,
            r"canonical_answer_changes\s+as\s+materialized.*?where\s+target\.canonical_was_visible",
        )
        self.assertRegex(
            sql,
            r"knowledge_answer_changes\s+as\s+materialized.*?where\s+target\.knowledge_was_visible",
        )
        self.assertRegex(
            sql,
            r"document\.document_state\s*=\s*'active'\s+and\s+document\.deleted_at\s+is\s+null\s+and\s+head\.readable_revision_id\s+is\s+not\s+null\s*\)\s+as\s+canonical_was_visible",
        )
        self.assertRegex(
            sql,
            r"knowledge\.document_state\s*=\s*'active'\s+and\s+knowledge\.deleted_at\s+is\s+null\s+and\s+knowledge\.readable_revision_id\s+is\s+not\s+null\s*\)\s+as\s+knowledge_was_visible",
        )
        self.assertRegex(
            sql,
            r"source_truth_version\s*=\s*greatest\s*\(\s*coalesce\(library\.source_truth_version,\s*0\)\s*\+\s*1,\s*\(extract\(epoch\s+from\s+clock_timestamp\(\)\)\s*\*\s*1000000\)::bigint",
        )

    def test_operator_report_is_aggregate_only(self) -> None:
        sql = normalized_sql()

        self.assertIn("content_documents_tombstoned", sql)
        self.assertIn("knowledge_documents_tombstoned", sql)
        self.assertIn("libraries_generation_bumped", sql)
        self.assertNotRegex(sql, r"returning\s+(?:content_document\.)?id\s*,")


if __name__ == "__main__":
    unittest.main()
