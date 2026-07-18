from __future__ import annotations

import importlib.util
import io
import tempfile
import unittest
from contextlib import redirect_stderr
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("lint_architecture.py")


def load_module():
    spec = importlib.util.spec_from_file_location("lint_architecture", MODULE_PATH)
    if spec is None or spec.loader is None:
        raise RuntimeError("failed to load architecture linter")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ArchitectureLintTests(unittest.TestCase):
    def setUp(self) -> None:
        self.module = load_module()
        self.tempdir = tempfile.TemporaryDirectory()
        self.root = Path(self.tempdir.name)

    def tearDown(self) -> None:
        self.tempdir.cleanup()

    def write(self, relative_path: str, content: str) -> None:
        path = self.root / relative_path
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content, encoding="utf-8")

    def test_clean_tree_passes(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/mod.rs",
            "pub(crate) use types::RuntimeAnswerVerification;\n",
        )
        self.write(
            "apps/api/src/services/content/service.rs",
            "let result = replace_chunks_with_projection().await?;\nuse_value(result);\n",
        )

        self.assertEqual(self.module.scan_repository(self.root), [])

    def test_rejects_contract_documentation_that_only_restates_identifiers(self) -> None:
        self.write(
            "crates/contracts/src/sample.rs",
            "/// Transport contract for `Sample`.\n"
            "pub struct Sample {\n"
            "    /// Value carried by the `id` transport field.\n"
            "    pub id: String,\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["meaningful-contract-documentation"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [1, 3])

    def test_detects_blanket_clippy_allow_in_production_source(self) -> None:
        self.write(
            "apps/api/src/services/query/service/mod.rs",
            "#![allow(\n    clippy::all,\n    clippy::too_many_lines\n)]\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-blanket-clippy-all")
        self.assertEqual(violations[0].line, 2)

    def test_rejects_hidden_serde_wire_aliases_in_production(self) -> None:
        self.write(
            "apps/api/src/interfaces/http/search.rs",
            "#[derive(serde::Deserialize)]\n"
            "struct Search {\n"
            "    #[serde(rename = \"query\", alias = \"q\")]\n"
            "    query: Option<String>,\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-serde-compatibility-aliases")
        self.assertEqual(violations[0].line, 3)

    def test_rejects_stateless_mcp_compatibility_scopes(self) -> None:
        self.write(
            "apps/api/src/interfaces/http/mcp.rs",
            "enum McpSessionScope { Session([u8; 32]), TokenScoped }\n"
            "const KEY: &str = \"legacy-session-terminated\";\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-stateless-mcp-compatibility-path"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2])

    def test_detects_query_execution_glob_reexport_only_in_guarded_boundary(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/mod.rs",
            "pub(crate) use answer::*;\n",
        )
        self.write(
            "apps/api/src/infra/repositories/mod.rs",
            "pub use catalog::*;\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-query-execution-glob-reexport")

    def test_detects_production_glob_import_but_allows_test_modules(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer.rs",
            "use super::types::*;\n",
        )
        self.write(
            "apps/api/src/services/query/execution/tests/answer_tests.rs",
            "use super::*;\n",
        )
        self.write(
            "apps/api/src/services/query/execution/retrieval_plan_tests.rs",
            "use super::*;\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-query-execution-glob-import")
        self.assertEqual(
            violations[0].path,
            Path("apps/api/src/services/query/execution/answer.rs"),
        )

    def test_detects_new_oversized_query_execution_module(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/new_stage.rs",
            "\n".join("fn focused() {}" for _ in range(801)),
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "query-execution-file-line-budget")
        self.assertEqual(violations[0].line, 801)

    def test_legacy_query_module_line_budget_is_a_strict_ratchet(self) -> None:
        budget = self.module.LEGACY_QUERY_EXECUTION_LINE_BUDGETS["retrieve.rs"]
        self.write(
            "apps/api/src/services/query/execution/retrieve.rs",
            "\n".join("const VALUE: usize = 1;" for _ in range(budget + 1)),
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "query-execution-file-line-budget")
        self.assertEqual(violations[0].line, budget + 1)

    def test_detects_discarded_atomic_projection_result_across_lines(self) -> None:
        self.write(
            "apps/api/src/services/content/service.rs",
            "let _ = content_repository::promote_document_head_with_projection(\n"
            "    postgres,\n"
            "    &head,\n"
            ").await;\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-discarded-atomic-projection-result")
        self.assertEqual(violations[0].line, 1)

    def test_detects_answer_stage_dependency_on_retrieval_implementation(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer_pipeline.rs",
            "let chunk = super::retrieve::map_chunk_hit(row);\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-answer-to-retrieve-dependency")
        self.assertEqual(violations[0].line, 1)

    def test_detects_neutral_chunk_support_dependency_on_retrieval_implementation(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/chunk_support.rs",
            "use crate::services::query::execution::retrieve::map_chunk_hit;\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-chunk-support-to-retrieve-dependency")
        self.assertEqual(violations[0].line, 1)

    def test_detects_raw_question_completion_evaluation_outside_contract_boundary(self) -> None:
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "let assessment = evaluate_answer_completion(question, answer);\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-raw-question-completion-evaluation")
        self.assertEqual(violations[0].line, 1)

    def test_detects_provider_free_ir_reclassification_outside_compiler_boundary(self) -> None:
        self.write(
            "apps/api/src/interfaces/http/mcp/tools/grounded.rs",
            "let query_ir = provider_free_fallback_query_ir(question);\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-downstream-provider-free-query-ir")
        self.assertEqual(violations[0].line, 1)

    def test_detects_semantic_routing_dictionary_constants_and_statics(self) -> None:
        self.write(
            "apps/api/src/services/policy.rs",
            "const ROUTING_KEYWORDS: &[&str] = &[];\n"
            "static FAILURE_PHRASES: &[&str] = &[];\n"
            "const COMMON_STOPWORDS: &[&str] = &[];\n"
            "static SEARCH_VOCABULARY: &[&str] = &[];\n"
            "const PROVIDER_ALIASES: &[&str] = &[];\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-semantic-routing-dictionary"] * 5,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3, 4, 5])

    def test_detects_historical_projection_state_writes_but_ignores_tests_and_comments(self) -> None:
        self.write(
            "apps/api/src/services/content/projection.rs",
            "let sql = r#\"select 'ready'::text as text_state\"#;\n"
            "let row = Projection { vector_state: \"vector_ready\".to_string() };\n"
            "graph_state = \"graph_ready\";\n"
            "// text_state = \"readable\";\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    const LEGACY: &str = r#\"select 'readable'::text as text_state\"#;\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["canonical-projection-state-vocabulary"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3])

    def test_detects_arbitrarily_named_string_tables_in_query_semantic_code(self) -> None:
        self.write(
            "apps/api/src/services/query/router.rs",
            "const MANUAL_ROUTER: &[&str] = &[\"opaque phrase\"];\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-query-semantic-string-table")
        self.assertEqual(violations[0].line, 1)

    def test_detects_local_string_tables_in_query_semantic_code(self) -> None:
        self.write(
            "apps/api/src/services/query/router.rs",
            "fn classify() {\n"
            "    let manual_router = &[\"opaque phrase\", \"another phrase\"];\n"
            "    let fallback_terms = vec![\"third phrase\", \"fourth phrase\"];\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-query-semantic-string-table"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [2, 3])

    def test_detects_raw_short_token_semantic_promotion(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/preflight.rs",
            "fn promote(question: &str, query_ir: &QueryIR) -> bool {\n"
            "    technical_literal_focus_keywords(question, Some(query_ir))\n"
            "        .iter()\n"
            "        .any(|keyword| keyword.chars().count() < 4)\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-raw-short-token-semantic-promotion")
        self.assertEqual(violations[0].line, 2)

    def test_detects_raw_procedure_semantic_term_models_only_in_production_query_code(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer.rs",
            "struct Focus { procedure_terms: Vec<String>, subject_acronym_terms: Vec<String> }\n"
            "fn procedure_terms_match() {}\n"
            "#[cfg(test)]\n"
            "mod tests { fn fixture() { let action_candidate_terms = Vec::new(); } }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-procedure-control-only"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [1, 1, 2])

    def test_ignores_local_string_tables_in_query_tests_and_comments(self) -> None:
        self.write(
            "apps/api/src/services/query/router.rs",
            "// let commented = &[\"opaque phrase\", \"another phrase\"];\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn fixture() {\n"
            "        let examples = vec![\"first example\", \"second example\"];\n"
            "    }\n"
            "}\n",
        )

        self.assertEqual(self.module.scan_repository(self.root), [])

    def test_allows_formal_extension_tables_in_query_code(self) -> None:
        self.write(
            "apps/api/src/services/query/parser.rs",
            "const CONFIG_EXTENSIONS: &[&str] = &[\"cfg\", \"ini\"];\n",
        )

        self.assertEqual(self.module.scan_repository(self.root), [])

    def test_detects_raw_user_fragment_output_mutation_helpers(self) -> None:
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "fn ensure_user_fragments_visible(answer: String) -> String { answer }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-raw-user-fragment-output-mutation")
        self.assertEqual(violations[0].line, 1)

    def test_detects_unfenced_vector_deletes_only_in_production(self) -> None:
        self.write(
            "apps/api/src/services/content/service.rs",
            "store.delete_chunk_vectors_by_revision(revision_id).await?;\n"
            "store.delete_chunk_vectors_by_ids_fenced(library_id, ids, source_version).await?;\n"
            "store.delete_entity_vectors_by_library(library_id).await?;\n"
            "store.delete_chunk_vectors_by_revision_fenced(\n"
            "    library_id, revision_id, source_version\n"
            ").await?;\n"
            "#[cfg(test)]\n"
            "fn fixture_cleanup() {\n"
            "    store.delete_entity_vectors_by_library(library_id);\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["source-fenced-vector-deletes-only"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3])

    def test_detects_post_generation_grounded_answer_splicing(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer_pipeline.rs",
            "fn append_missing_grounded_requested_labels(answer: String) -> String { answer }\n"
            "fn append_missing_focus_aligned_exact_literals(answer: String) -> String { answer }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-post-generation-answer-splicing"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2])

    def test_detects_untyped_clarification_routing_helpers(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer_pipeline.rs",
            "fn structural_clarify_allowed_without_compiler() -> bool { true }\n"
            "fn question_is_terse_variant_selector() -> bool { true }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-query-ir-clarification-only"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2])

    def test_detects_raw_history_query_reclassification_helpers(self) -> None:
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "fn history_added_query_token_overlap() -> usize { 0 }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-raw-history-query-reclassification")
        self.assertEqual(violations[0].line, 1)

    def test_detects_raw_question_language_and_repair_prose_helpers(self) -> None:
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "fn query_message_language(question: &str) {}\n"
            "fn focused_completion_requirement() {}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-query-language-only", "no-repair-query-prose-mutation"],
        )
        self.assertEqual([violation.line for violation in violations], [1, 2])

    def test_detects_typed_enum_labels_reintroduced_as_lexical_search_terms(self) -> None:
        self.write(
            "apps/api/src/services/query/planner.rs",
            "fn collect(query_ir: &QueryIR) {\n"
            "    for target_type in &query_ir.target_types {\n"
            "        push_seed(&mut seeds.high, target_type.as_str());\n"
            "    }\n"
            "}\n"
            "fn source_slice_direction_seed() -> &'static str { \"tail\" }\n"
            "fn score(target_type: QueryTargetKind) { terms.extend(label_terms(target_type.as_str(), 2)); }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-language-specific-enum-lexical-seed"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [3, 6, 7])

    def test_detects_clarification_classified_as_grounded_answer_repair(self) -> None:
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "let reason = GroundedAnswerRepairReason::Clarification;\n"
            "let typed_reason = GroundedAnswerRepairReason::ClarificationRequired;\n"
            "let metadata = RuntimeGroundedRepairKind::Clarification;\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-clarification-is-terminal"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3])

    def test_detects_raw_verification_state_gate_on_typed_grounded_readiness(self) -> None:
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "let blocked = grounded_answer_verification_state(result)\n"
            "    != Some(GROUNDED_ANSWER_VERIFICATION_VERIFIED);\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "typed-grounded-readiness-only")
        self.assertEqual(violations[0].line, 1)

    def test_detects_answer_disposition_reconstructed_from_warnings_or_verifier_state(self) -> None:
        self.write(
            "apps/api/src/interfaces/http/mcp.rs",
            "fn grounded_answer_final_answer_ready(detail: &Detail) -> bool {\n"
            "    detail.verification_state == State::Verified\n"
            "}\n"
            "fn grounded_answer_verification_warning_blocks_finalization(_: &str) {}\n",
        )
        self.write(
            "apps/api/src/services/query/agent_loop.rs",
            "fn grounded_answer_warning_requires_follow_up() {}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-answer-disposition-only"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [1, 4, 1])

    def test_detects_raw_procedure_identity_reclassification_helpers(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer.rs",
            "fn update_procedure_raw_target_identity_token_sequences() {}\n"
            "fn update_procedure_raw_identity_sequence_has_distinctive_surface() {}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-procedure-identity-only"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2])

    def test_detects_raw_question_casing_focus_helpers(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer_pipeline.rs",
            "fn token_has_fallback_entity_signal() {}\n"
            "fn token_is_plain_titlecase() {}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-technical-focus-only"] * 2,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2])

    def test_detects_untyped_graph_evidence_literal_lane_data_flow(self) -> None:
        self.write(
            "apps/api/src/infra/repositories/runtime_graph_repository.rs",
            "pub async fn search_runtime_graph_evidence_by_text(\n"
            "    query_texts: &[String],\n"
            ") {}\n"
            "fn runtime_graph_evidence_literal_search_queries(query_texts: &[String]) {\n"
            "    let _ = query_texts.iter().any(|text| text.chars().any(char::is_uppercase));\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "typed-graph-evidence-query-lanes")

    def test_detects_raw_graph_evidence_queries_promoted_to_literal_lane(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/retrieve.rs",
            "fn graph_evidence_db_text_queries(\n"
            "    text_queries: &[String],\n"
            "    query_ir: Option<&QueryIR>,\n"
            ") {\n"
            "    let _ = query_ir.into_iter().flat_map(|query_ir| {\n"
            "        query_ir.literal_constraints.iter()\n"
            "            .chain(query_ir.target_entities.iter())\n"
            "            .chain(query_ir.document_focus.iter())\n"
            "    });\n"
            "    let _ = text_queries.iter().map(|query| {\n"
            "        RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(query.clone())\n"
            "    });\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "typed-graph-evidence-query-lanes")

    def test_detects_identifier_literal_parameter_intent_reclassification(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/question_intent.rs",
            "fn classify_query_ir_intents(ir: &QueryIR) {\n"
            "    for literal in &ir.literal_constraints {\n"
            "        let _ = match literal.kind {\n"
            "            LiteralKind::Identifier => Some(QuestionIntent::Parameter),\n"
            "            _ => None,\n"
            "        };\n"
            "    }\n"
            "}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "typed-question-intent-only")

    def test_detects_prose_classifier_helper_identifiers(self) -> None:
        self.write(
            "apps/api/src/services/policy.rs",
            "fn matches_any_substring() {}\n"
            "fn contains_any_phrase() {}\n"
            "fn classify_provider_message() {}\n"
            "fn status_from_message() {}\n"
            "fn is_transport_error_message() {}\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-prose-classifier-helper"] * 5,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3, 4, 5])

    def test_detects_raw_verification_claim_inference(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/verification_claims.rs",
            "fn extract_high_confidence_named_claim_literals() {}\n"
            "fn extract_high_confidence_canonical_claim_literals() {}\n"
            "fn named_claim_token_shape() {}\n"
            "fn named_claim_is_strong_at_sentence_start() {}\n"
            "fn has_single_letter_unit_suffix() {}\n"
            "fn legacy(kind: CanonicalClaimKind) { let _ = CanonicalClaimKind::Named; }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["typed-verification-claims-only"] * 6,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3, 4, 5, 6])

    def test_detects_direct_error_display_string_routing(self) -> None:
        self.write(
            "apps/api/src/services/policy.rs",
            "let first = error.to_string().contains(\"opaque\");\n"
            "let second = err.to_string().starts_with(\"opaque\");\n"
            "let third = failure.to_string().ends_with(\"opaque\");\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            ["no-error-display-string-routing"] * 3,
        )
        self.assertEqual([violation.line for violation in violations], [1, 2, 3])

    def test_detects_free_string_query_target_control_surface(self) -> None:
        self.write(
            "apps/api/src/domains/query.rs",
            "struct QueryIr { target_types: Vec<String> }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "typed-query-target-kinds")

    def test_detects_provider_or_model_name_capability_routing(self) -> None:
        self.write(
            "apps/api/src/integrations/gateway.rs",
            "let provider = provider_kind.eq_ignore_ascii_case(\"opaque\");\n"
            "let model = model_name.to_ascii_lowercase();\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(
            [violation.rule for violation in violations],
            [
                "no-provider-model-name-capability-routing",
                "no-provider-model-name-capability-routing",
            ],
        )

    def test_allows_formal_protocol_schema_constants(self) -> None:
        self.write(
            "apps/api/src/services/protocol.rs",
            "const RPC_METHOD_NAMES_ALIASES: &[&str] = &[];\n"
            "const ACCEPTED_MIME_ALIASES: &[&str] = &[];\n"
            "const RETRYABLE_SQLSTATE_ALIASES: &[&str] = &[];\n",
        )

        self.assertEqual(self.module.scan_repository(self.root), [])

    def test_prose_routing_guards_ignore_cfg_test_bodies_and_test_sources(self) -> None:
        forbidden_test_body = (
            "const ROUTING_KEYWORDS: &[&str] = &[];\n"
            "fn classify_provider_message() {}\n"
            "let matched = error.to_string().contains(\"opaque\");\n"
        )
        self.write(
            "apps/api/src/services/policy.rs",
            "fn production_code() {}\n"
            "#[cfg(test)]\n"
            "mod tests {\n"
            f"{forbidden_test_body}"
            "}\n",
        )
        self.write(
            "apps/api/src/services/tests/policy.rs",
            forbidden_test_body,
        )
        self.write(
            "apps/api/src/services/policy_tests.rs",
            forbidden_test_body,
        )

        self.assertEqual(self.module.scan_repository(self.root), [])

    def test_rust_character_literals_do_not_hide_following_production_code(self) -> None:
        self.write(
            "apps/api/src/services/policy.rs",
            "fn quote_closer(value: char) -> bool { value == '\"' }\n"
            "fn borrow<'a>(value: &'a str) -> &'a str { value }\n"
            "const ROUTING_KEYWORDS: &[&str] = &[];\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-semantic-routing-dictionary")
        self.assertEqual(violations[0].line, 3)

    def test_query_target_mutation_is_forbidden_after_ir_construction(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/retrieve.rs",
            "fn reclassify(ir: &mut QueryIR) { ir.target_types.push(QueryTargetKind::Procedure); }\n",
        )
        self.write(
            "apps/api/src/services/query/compiler.rs",
            "fn normalize(ir: &mut QueryIR) { ir.target_types.clear(); }\n",
        )
        self.write(
            "apps/api/src/services/query/execution/answer_pipeline.rs",
            "fn normalize(ir: &mut QueryIR) { ir.target_types = Vec::new(); }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 3)
        self.assertEqual(
            [violation.rule for violation in violations],
            [
                "canonical-query-target-mutation-boundary",
                "canonical-query-target-mutation-boundary",
                "canonical-query-target-mutation-boundary",
            ],
        )
        self.assertEqual(
            [violation.path for violation in violations],
            [
                Path("apps/api/src/services/query/compiler.rs"),
                Path("apps/api/src/services/query/execution/answer_pipeline.rs"),
                Path("apps/api/src/services/query/execution/retrieve.rs"),
            ],
        )

    def test_detects_raw_question_query_ir_reclassification_including_compiler(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/answer_pipeline.rs",
            "fn guard_current_question_constraint_focus(\n"
            "    question: &str,\n"
            "    mut ir: QueryIR,\n"
            ") -> QueryIR {\n"
            "    ir.source_slice = None;\n"
            "    ir\n"
            "}\n",
        )
        self.write(
            "apps/api/src/services/query/turn.rs",
            "fn normalize_raw_question_ir(\n"
            "    raw_question: &str,\n"
            "    ir: QueryIR,\n"
            ") -> Result<QueryIR, Error> {\n"
            "    Ok(ir)\n"
            "}\n",
        )
        self.write(
            "apps/api/src/services/query/compiler.rs",
            "fn validate_request(question: &str, ir: QueryIR) -> QueryIR { ir }\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 3)
        self.assertEqual(
            [violation.rule for violation in violations],
            [
                "query-compiler-sole-semantic-boundary",
                "query-compiler-sole-semantic-boundary",
                "query-compiler-sole-semantic-boundary",
            ],
        )
        self.assertEqual(
            [violation.path for violation in violations],
            [
                Path("apps/api/src/services/query/compiler.rs"),
                Path("apps/api/src/services/query/execution/answer_pipeline.rs"),
                Path("apps/api/src/services/query/turn.rs"),
            ],
        )
        self.assertEqual([violation.line for violation in violations], [1, 1, 1])

    def test_cfg_test_item_does_not_hide_later_production_code(self) -> None:
        self.write(
            "apps/api/src/services/policy.rs",
            "#[cfg(test)]\n"
            "mod tests { const TEST_KEYWORDS: &[&str] = &[]; }\n"
            "const ROUTING_KEYWORDS: &[&str] = &[];\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-semantic-routing-dictionary")
        self.assertEqual(violations[0].line, 3)

    def test_cfg_test_function_parameters_do_not_hide_later_production_code(self) -> None:
        self.write(
            "apps/api/src/services/policy.rs",
            "#[cfg(test)]\n"
            "fn fixture(value: Option<&str>) -> bool { value.is_some() }\n"
            "const ROUTING_KEYWORDS: &[&str] = &[];\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "no-semantic-routing-dictionary")
        self.assertEqual(violations[0].line, 3)

    def test_detects_frontend_build_args_before_cached_dependency_install(self) -> None:
        self.write(
            "apps/web/Dockerfile",
            "FROM node:26-alpine AS builder\n"
            "WORKDIR /app\n"
            "ARG APP_VERSION\n"
            "ENV VITE_APP_VERSION=${APP_VERSION}\n"
            "COPY package.json package-lock.json ./\n"
            "RUN npm ci --legacy-peer-deps\n"
            "COPY . .\n"
            "RUN npx vite build\n",
        )

        violations = self.module.scan_repository(self.root)

        self.assertEqual(len(violations), 1)
        self.assertEqual(violations[0].rule, "frontend-dependencies-before-build-args")
        self.assertEqual(violations[0].line, 3)

    def test_cli_fails_with_stable_relative_diagnostic(self) -> None:
        self.write(
            "apps/api/src/services/query/execution/mod.rs",
            "pub(crate) use verification::*;\n",
        )
        stderr = io.StringIO()

        with redirect_stderr(stderr):
            exit_code = self.module.main(["--root", str(self.root)])

        self.assertEqual(exit_code, 1)
        self.assertIn(
            "apps/api/src/services/query/execution/mod.rs:1 "
            "[no-query-execution-glob-reexport]",
            stderr.getvalue(),
        )


if __name__ == "__main__":
    unittest.main()
