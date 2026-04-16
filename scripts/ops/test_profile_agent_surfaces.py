import importlib.util
import pathlib
import sys
import unittest


SCRIPT_PATH = pathlib.Path(__file__).resolve().parent / "profile-agent-surfaces.py"
SPEC = importlib.util.spec_from_file_location("profile_agent_surfaces", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AgentSurfaceProfileTests(unittest.TestCase):
    def test_summarize_graph_quality_detects_document_coverage_and_duplicates(self) -> None:
        summary = MODULE.summarize_graph_quality(
            {
                "documents": [
                    {"documentId": "doc-1", "title": "Primary"},
                    {"documentId": "doc-2", "title": "Secondary"},
                ],
                "entities": [
                    {"entityId": "entity-1", "label": "Orion", "supportCount": 10},
                    {"entityId": "entity-2", "label": "orion", "supportCount": 8},
                ],
                "relations": [
                    {
                        "relationId": "rel-1",
                        "sourceEntityId": "entity-1",
                        "targetEntityId": "entity-2",
                        "relationType": "depends_on",
                        "supportCount": 5,
                    },
                    {
                        "relationId": "rel-2",
                        "sourceEntityId": "entity-1",
                        "targetEntityId": "entity-2",
                        "relationType": "depends_on",
                        "supportCount": 4,
                    },
                ],
                "documentLinks": [
                    {
                        "documentId": "doc-1",
                        "targetNodeId": "entity-1",
                        "targetNodeType": "entity",
                        "relationType": "supports",
                        "supportCount": 3,
                    },
                    {
                        "documentId": "doc-2",
                        "targetNodeId": "rel-1",
                        "targetNodeType": "relation",
                        "relationType": "supports",
                        "supportCount": 1,
                    },
                    {
                        "documentId": "doc-missing",
                        "targetNodeId": "entity-1",
                        "targetNodeType": "entity",
                        "relationType": "supports",
                        "supportCount": 1,
                    },
                ],
            }
        )

        self.assertEqual(summary.orphan_document_count, 1)
        self.assertTrue(summary.document_rank_monotonic)
        self.assertEqual(summary.duplicate_entity_label_count, 1)
        self.assertEqual(summary.duplicate_relation_signature_count, 1)
        self.assertEqual(summary.quality_status, "broken")

    def test_summarize_relation_list_detects_unknown_and_duplicate_signatures(self) -> None:
        summary = MODULE.summarize_relation_list(
            [
                {
                    "relationId": "rel-1",
                    "sourceLabel": "Orion",
                    "targetLabel": "Atlas",
                    "relationType": "depends_on",
                },
                {
                    "relationId": "rel-2",
                    "sourceLabel": "orion",
                    "targetLabel": "atlas",
                    "relationType": "depends_on",
                },
                {
                    "relationId": "rel-3",
                    "sourceLabel": "unknown",
                    "targetLabel": "Atlas",
                    "relationType": "mentions",
                },
            ]
        )

        self.assertEqual(summary.row_count, 3)
        self.assertEqual(summary.unknown_label_count, 1)
        self.assertEqual(summary.duplicate_signature_count, 1)

    def test_summarize_relation_list_accepts_structured_content_wrapper(self) -> None:
        summary = MODULE.summarize_relation_list(
            {
                "relations": [
                    {
                        "relationId": "rel-1",
                        "sourceLabel": "System Information Endpoint",
                        "targetLabel": "GET",
                        "relationType": "configures",
                    }
                ]
            }
        )

        self.assertEqual(summary.row_count, 1)
        self.assertEqual(summary.unknown_label_count, 0)
        self.assertEqual(summary.duplicate_signature_count, 0)

    def test_gate_checks_fail_on_graph_and_document_alignment_regressions(self) -> None:
        checks = MODULE.build_gate_checks(
            entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=2,
                top_label="Orion",
                top_score=10.0,
            ),
            document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=128,
                top_excerpt_length=240,
                top_chunk_reference_count=2,
                top_score=5.0,
            ),
            document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-2",
                document_title="Secondary",
                readability_state="readable",
                content_length=512,
                total_reference_count=3,
                has_more=False,
                slice_start_offset=64,
                slice_end_offset=576,
            ),
            graph_quality=MODULE.McpQualitySummary(
                entity_count=2,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=1,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=False,
                duplicate_entity_label_count=1,
                duplicate_relation_signature_count=0,
                top_entity_label="Orion",
                visible_entity_labels_normalized=("orion",),
            ),
            relation_list_summary=MODULE.RelationListSummary(
                row_count=2,
                unknown_label_count=1,
                duplicate_signature_count=0,
            ),
            community_summary=MODULE.CommunitySummary(
                count=0,
                communities_with_summary=0,
                top_entity_count=0,
            ),
            assistant_summaries=[],
            runtime_execution_summary=None,
            runtime_trace_summary=None,
            legacy_runtime_execution_error=None,
            graph_min_entities=1,
            graph_min_relations=1,
            graph_min_documents=1,
            community_min_count=1,
            entity_search_min_hits=1,
            search_min_hits=1,
            search_min_readable_hits=1,
            read_min_content_chars=100,
            read_min_references=1,
            assistant_min_references=1,
            assistant_expected_verification="verified",
            assistant_require_all=[],
            assistant_forbid_any=[],
            assistant_max_tool_starts=None,
            expected_search_top_label=None,
            max_tool_latency_ms=None,
            max_first_delta_ms=None,
            max_completed_ms=None,
            tool_samples=[],
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["graph.document_links_visible_documents"].status, "fail")
        self.assertEqual(by_label["graph.documents_ranked_by_support"].status, "fail")
        self.assertEqual(by_label["graph.duplicate_entity_labels"].status, "fail")
        self.assertEqual(by_label["graph.list_relations_labels"].status, "fail")
        self.assertEqual(by_label["mcp.read_document_alignment"].status, "fail")
        self.assertEqual(by_label["mcp.read_document_offset_alignment"].status, "fail")

    def test_graph_search_alignment_passes_when_top_hit_is_visible_not_top_ranked(self) -> None:
        checks = MODULE.build_gate_checks(
            entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=1,
                top_label="checkout server",
                top_score=10.0,
            ),
            document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=0,
                top_excerpt_length=240,
                top_chunk_reference_count=2,
                top_score=5.0,
            ),
            document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-1",
                document_title="Primary",
                readability_state="readable",
                content_length=512,
                total_reference_count=3,
                has_more=False,
                slice_start_offset=0,
                slice_end_offset=512,
            ),
            graph_quality=MODULE.McpQualitySummary(
                entity_count=3,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=0,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=True,
                duplicate_entity_label_count=0,
                duplicate_relation_signature_count=0,
                top_entity_label="HTTP",
                visible_entity_labels_normalized=("http", "checkout server", "system information endpoint"),
            ),
            relation_list_summary=MODULE.RelationListSummary(
                row_count=1,
                unknown_label_count=0,
                duplicate_signature_count=0,
            ),
            community_summary=MODULE.CommunitySummary(
                count=1,
                communities_with_summary=1,
                top_entity_count=2,
            ),
            assistant_summaries=[],
            runtime_execution_summary=None,
            runtime_trace_summary=None,
            legacy_runtime_execution_error=None,
            graph_min_entities=1,
            graph_min_relations=1,
            graph_min_documents=1,
            community_min_count=0,
            entity_search_min_hits=1,
            search_min_hits=1,
            search_min_readable_hits=1,
            read_min_content_chars=100,
            read_min_references=1,
            assistant_min_references=1,
            assistant_expected_verification="verified",
            assistant_require_all=[],
            assistant_forbid_any=[],
            assistant_max_tool_starts=None,
            expected_search_top_label=None,
            max_tool_latency_ms=None,
            max_first_delta_ms=None,
            max_completed_ms=None,
            tool_samples=[],
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["graph.search_alignment"].status, "pass")

    def test_runtime_and_community_summaries_capture_canonical_fields(self) -> None:
        communities = MODULE.summarize_communities(
            {
                "communities": [
                    {
                        "communityId": 1,
                        "summary": "Checkout services",
                        "topEntities": ["Checkout", "Inventory"],
                    }
                ]
            }
        )
        runtime_execution = MODULE.summarize_runtime_execution(
            {
                "runtimeExecutionId": "runtime-1",
                "lifecycleState": "completed",
                "activeStage": "verification",
            }
        )
        runtime_trace = MODULE.summarize_runtime_trace(
            {
                "execution": {"runtimeExecutionId": "runtime-1"},
                "stages": [{"stageKind": "retrieve"}],
                "actions": [{"actionKind": "tool"}],
                "policyDecisions": [{"decisionKind": "allow"}],
            }
        )

        self.assertEqual(communities.count, 1)
        self.assertEqual(communities.communities_with_summary, 1)
        self.assertEqual(communities.top_entity_count, 2)
        self.assertEqual(runtime_execution.runtime_execution_id, "runtime-1")
        self.assertEqual(runtime_execution.lifecycle_state, "completed")
        self.assertEqual(runtime_trace.runtime_execution_id, "runtime-1")
        self.assertEqual(runtime_trace.stage_count, 1)

    def test_gate_checks_require_runtime_alignment_when_assistant_returns_runtime_id(self) -> None:
        checks = MODULE.build_gate_checks(
            entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=1,
                top_label="Orion",
                top_score=10.0,
            ),
            document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=0,
                top_excerpt_length=120,
                top_chunk_reference_count=1,
                top_score=5.0,
            ),
            document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-1",
                document_title="Primary",
                readability_state="readable",
                content_length=256,
                total_reference_count=2,
                has_more=False,
                slice_start_offset=0,
                slice_end_offset=256,
            ),
            graph_quality=MODULE.McpQualitySummary(
                entity_count=1,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=0,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=True,
                duplicate_entity_label_count=0,
                duplicate_relation_signature_count=0,
                top_entity_label="Orion",
                visible_entity_labels_normalized=("orion",),
            ),
            relation_list_summary=MODULE.RelationListSummary(
                row_count=1,
                unknown_label_count=0,
                duplicate_signature_count=0,
            ),
            community_summary=MODULE.CommunitySummary(
                count=1,
                communities_with_summary=1,
                top_entity_count=1,
            ),
            assistant_summaries=[
                MODULE.SseSummary(
                    time_to_first_frame_s=0.1,
                    time_to_first_delta_s=0.2,
                    time_to_first_tool_call_s=None,
                    time_to_completed_s=0.5,
                    delta_event_count=2,
                    tool_call_started_count=0,
                    tool_call_completed_count=0,
                    answer_length=42,
                    answer_text="GET /system/info",
                    total_reference_count=2,
                    verification_state="verified",
                    completion_state="completed",
                    query_execution_id="query-1",
                    runtime_execution_id="runtime-1",
                )
            ],
            runtime_execution_summary=MODULE.RuntimeExecutionProbeSummary(
                runtime_execution_id="runtime-1",
                lifecycle_state="completed",
                active_stage="verification",
            ),
            runtime_trace_summary=MODULE.RuntimeTraceProbeSummary(
                runtime_execution_id="runtime-1",
                stage_count=2,
                action_count=1,
                policy_decision_count=0,
            ),
            legacy_runtime_execution_error=MODULE.ToolErrorSummary(
                error_kind="invalid_mcp_tool_call",
                message="bad request: invalid MCP tool arguments: unknown field `executionId`, expected `runtimeExecutionId`",
            ),
            graph_min_entities=1,
            graph_min_relations=1,
            graph_min_documents=1,
            community_min_count=1,
            entity_search_min_hits=1,
            search_min_hits=1,
            search_min_readable_hits=1,
            read_min_content_chars=100,
            read_min_references=1,
            assistant_min_references=1,
            assistant_expected_verification="verified",
            assistant_require_all=["/system/info"],
            assistant_forbid_any=["/serverinfo"],
            assistant_max_tool_starts=0,
            expected_search_top_label=None,
            max_tool_latency_ms=None,
            max_first_delta_ms=None,
            max_completed_ms=None,
            tool_samples=[],
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["graph.communities"].status, "pass")
        self.assertEqual(by_label["assistant.runtime_execution_id"].status, "pass")
        self.assertEqual(by_label["assistant.run_1.tool_start_budget"].status, "pass")
        self.assertEqual(by_label["mcp.get_runtime_execution_alignment"].status, "pass")
        self.assertEqual(by_label["mcp.get_runtime_execution_trace_stages"].status, "pass")
        self.assertEqual(
            by_label["mcp.get_runtime_execution_legacy_field_rejected"].status, "pass"
        )


if __name__ == "__main__":
    unittest.main()
