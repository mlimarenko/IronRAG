#!/usr/bin/env python3
"""Fail-fast structural guards for high-risk repository architecture boundaries."""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path
from typing import NamedTuple, Sequence


class Violation(NamedTuple):
    path: Path
    line: int
    rule: str
    message: str


GLOB_REEXPORT = re.compile(
    r"^\s*pub(?:\s*\([^)]*\))?\s+use\s+[A-Za-z_][A-Za-z0-9_:]*::\*\s*;",
    re.MULTILINE,
)

GLOB_IMPORT = re.compile(
    r"^use\s+[A-Za-z_][A-Za-z0-9_:]*::\*\s*;",
    re.MULTILINE,
)

BLANKET_CLIPPY_ALLOW = re.compile(
    r"#!?\[allow\((?:(?!\)\]).)*?(?P<lint>\bclippy::all\b)(?:(?!\)\]).)*?\)\]",
    re.DOTALL,
)

ATOMIC_PROJECTION_WRITES = (
    "create_document_with_projection",
    "create_revision_with_projection",
    "materialize_knowledge_document_from_canonical_head",
    "promote_document_head_with_projection",
    "replace_chunks_with_projection",
)

DISCARDED_ATOMIC_PROJECTION_RESULT = re.compile(
    r"\blet\s+_\s*=\s*[^;]*?(?:"
    + "|".join(re.escape(name) for name in ATOMIC_PROJECTION_WRITES)
    + r")\s*\([^;]*?\)\s*\.await\s*;",
    re.DOTALL,
)

ANSWER_STAGE_RETRIEVAL_DEPENDENCY = re.compile(r"\b(?:super::)?retrieve::")
CHUNK_SUPPORT_RETRIEVAL_DEPENDENCY = re.compile(r"\bretrieve::")
RAW_QUESTION_COMPLETION_EVALUATION = re.compile(r"\bevaluate_answer_completion\s*\(")
PROVIDER_FREE_FALLBACK_QUERY_IR = re.compile(r"\bprovider_free_fallback_query_ir\b")
FRONTEND_BUILD_CONFIGURATION = re.compile(
    r"^(?:ARG\s+(?:APP_VERSION|VITE_[A-Z0-9_]+)(?:=|$)|ENV\s+VITE_[A-Z0-9_]+=)"
)

CFG_TEST_ATTRIBUTE = re.compile(r"(?m)^[ \t]*#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
RUST_RAW_STRING_START = re.compile(r'(?:br|cr|r)(?P<hashes>#{0,255})"')
SEMANTIC_DICTIONARY_DECLARATION = re.compile(
    r"\b(?:const|static)\s+(?:mut\s+|ref\s+)?(?P<name>(?:r#)?[A-Za-z_](?a:\w)*)\s*:"
)
QUERY_SEMANTIC_STRING_TABLE = re.compile(
    r"\b(?:const|static)\s+(?:mut\s+|ref\s+)?"
    r"(?P<name>(?:r#)?[A-Za-z_][A-Za-z0-9_]*)\s*:\s*"
    r"(?:&\s*)?\[\s*(?:&\s*)?(?:'static\s+)?str\b"
)
QUERY_LOCAL_SEMANTIC_STRING_TABLE = re.compile(
    r"\blet\s+(?:mut\s+)?(?P<name>(?:r#)?[A-Za-z_][A-Za-z0-9_]*)"
    r"\s*(?::\s*[^=;]+)?=\s*(?:&\s*)?"
    r"(?:\[\s*\"|vec!\s*\[\s*\")"
)
QUERY_FORMAL_STRING_TABLE_SUFFIXES = ("EXTENSIONS",)
SEMANTIC_DICTIONARY_SUFFIXES = (
    "KEYWORDS",
    "PHRASES",
    "STOPWORDS",
    "VOCABULARY",
    "ALIASES",
)
FORMAL_PROTOCOL_IDENTIFIER_MARKERS = ("METHOD_NAMES", "MIME", "SQLSTATE")
PROSE_CLASSIFIER_HELPER = re.compile(
    r"\b(?P<name>"
    r"matches_any_substring|"
    r"contains_any_phrase|"
    r"classify_[A-Za-z0-9_]+_message|"
    r"[A-Za-z_][A-Za-z0-9_]*_from_message|"
    r"is_[A-Za-z0-9_]+_error_message"
    r")\b"
)
RAW_VERIFICATION_CLAIM_INFERENCE = re.compile(
    r"\b(?:"
    r"extract_high_confidence_named_claim_literals|"
    r"extract_high_confidence_canonical_claim_literals|"
    r"named_claim_token_shape|"
    r"named_claim_is_strong_at_sentence_start|"
    r"has_single_letter_unit_suffix|"
    r"CanonicalClaimKind\s*::\s*Named"
    r")\b"
)
ERROR_DISPLAY_STRING_ROUTING = re.compile(
    r"\b(?:error|err|failure)\s*\.\s*to_string\s*\(\s*\)\s*\.\s*"
    r"(?:contains|starts_with|ends_with)\s*\("
)
HISTORICAL_PROJECTION_STATE = re.compile(
    r"(?:"
    r"text_state\s*(?:=|:)\s*['\"](?:ready|readable)['\"]"
    r"|['\"](?:ready|readable)['\"](?:::text)?\s+as\s+text_state\b"
    r"|vector_state\s*(?:=|:)\s*['\"]vector_ready['\"]"
    r"|['\"]vector_ready['\"](?:::text)?\s+as\s+vector_state\b"
    r"|graph_state\s*(?:=|:)\s*['\"]graph_ready['\"]"
    r"|['\"]graph_ready['\"](?:::text)?\s+as\s+graph_state\b"
    r")"
)
PLACEHOLDER_CONTRACT_DOC = re.compile(
    r"^\s*///\s+(?:"
    r"Wire variants? for|Wire variant|Transport contract for|Value carried by the|"
    r"Returns the stable transport value produced by"
    r")",
    re.MULTILINE,
)
SERDE_COMPATIBILITY_ALIAS = re.compile(
    r"#\s*\[\s*serde\s*\([^\]]*\balias\s*=",
    re.DOTALL,
)
MCP_STATELESS_COMPATIBILITY_PATH = re.compile(
    r"\bTokenScoped\b|legacy-session-terminated"
)
UNFENCED_VECTOR_DELETE_CALL = re.compile(
    r"\.\s*(?P<name>delete_chunk_vectors_by_revision|"
    r"delete_chunk_vectors_by_ids_fenced|delete_entity_vectors_by_library)\s*\("
)
RAW_QUERY_TARGET_STRING_COLLECTION = re.compile(
    r"\btarget_types\s*:\s*(?:std::vec::)?Vec\s*<\s*(?:std::string::)?String\s*>"
)
PROVIDER_MODEL_IDENTITY_CAPABILITY_ROUTING = re.compile(
    r"\b(?:provider_kind|model_name)\s*\.\s*"
    r"(?:eq_ignore_ascii_case|to_ascii_lowercase|starts_with|ends_with|contains)\s*\("
)
QUERY_TARGET_KIND_MUTATION = re.compile(
    r"\.\s*target_types\s*(?:=(?!=)|\.\s*(?:"
    r"push|clear|extend|retain|truncate|append|insert|remove|swap_remove|"
    r"sort|sort_by|sort_by_key|dedup"
    r")\s*\()"
)
RAW_USER_FRAGMENT_OUTPUT_MUTATOR = re.compile(
    r"\b(?P<name>"
    r"latest_user_verbatim_fragment_reminder|"
    r"ensure_user_fragments_visible|"
    r"extract_required_visible_user_fragments"
    r")\b"
)
POST_GENERATION_ANSWER_SPLICER = re.compile(
    r"\b(?P<name>"
    r"append_missing_grounded_requested_labels(?:_for_prepared)?|"
    r"append_missing_focus_aligned_exact_literals"
    r")\b"
)
UNTYPED_CLARIFICATION_ROUTER = re.compile(
    r"\b(?P<name>"
    r"structural_clarify_allowed_without_compiler|"
    r"question_is_terse_variant_selector"
    r")\b"
)
RAW_HISTORY_QUERY_RECLASSIFIER = re.compile(
    r"\b(?P<name>"
    r"compact_history_padded_grounded_answer_query|"
    r"compact_non_contextual_history_scoped_grounded_answer_query|"
    r"compact_standalone_single_grounded_answer_query|"
    r"contextual_search_documents_query|"
    r"history_added_query_token_overlap|"
    r"query_mentions_user_question"
    r")\b"
)
RAW_QUESTION_LANGUAGE_INFERENCE = re.compile(r"\bquery_message_language\b")
TYPED_ENUM_LABEL_LEXICAL_SEED = re.compile(
    r"\b(?:"
    r"push_seed\s*\([^;]{0,300}\btarget_type\s*\.\s*as_str\s*\(\s*\)|"
    r"target_type\s*\.\s*as_str\s*\(\s*\)|"
    r"fn\s+(?:source_slice_direction_seed|source_slice_filter_seed)\b"
    r")",
    re.DOTALL,
)
REPAIR_QUERY_PROSE_MUTATOR = re.compile(r"\bfocused_completion_requirement\b")
CLARIFICATION_REPAIR_CLASSIFICATION = re.compile(
    r"\b(?:GroundedAnswerRepairReason|RuntimeGroundedRepairKind)\s*::\s*"
    r"Clarification(?:Required)?\b"
)
RAW_GROUNDED_READINESS_VERIFICATION_GATE = re.compile(
    r"\bgrounded_answer_verification_state\s*\([^)]*\)\s*"
    r"!=\s*Some\s*\(\s*GROUNDED_ANSWER_VERIFICATION_VERIFIED\s*\)"
)
RAW_ANSWER_DISPOSITION_RECONSTRUCTOR = re.compile(
    r"\b(?P<name>"
    r"grounded_answer_verification_warning_blocks_finalization|"
    r"grounded_answer_warning_requires_follow_up"
    r")\b"
)
MCP_RAW_FINAL_READINESS = re.compile(
    r"\bfn\s+grounded_answer_final_answer_ready\s*\([^)]*\)\s*->\s*bool\s*\{"
    r"(?:(?!\n\}).){0,2000}\b(?:verification_state|verification_warnings)\b",
    re.DOTALL,
)
RAW_PROCEDURE_IDENTITY_RECLASSIFIER = re.compile(
    r"\b(?P<name>"
    r"update_procedure_raw_target_identity_token_sequences|"
    r"update_procedure_raw_identity_sequence_has_distinctive_surface"
    r")\b"
)
RAW_PROCEDURE_SEMANTIC_TERM_MODEL = re.compile(
    r"\b(?P<name>"
    r"procedure_terms|subject_acronym_terms|action_candidate_terms|procedure_terms_match"
    r")\b"
)
RAW_TECHNICAL_FOCUS_RECLASSIFIER = re.compile(
    r"\b(?P<name>token_has_fallback_entity_signal|token_is_plain_titlecase)\b"
)
RAW_SHORT_TOKEN_SEMANTIC_PROMOTION = re.compile(
    r"\btechnical_literal_focus_keywords\s*\("
    r"[^;}]{0,800}?\bchars\s*\(\s*\)\s*\.\s*count\s*\(\s*\)\s*<\s*4",
    re.DOTALL,
)
RAW_SHORT_TOKEN_SEMANTIC_PROMOTION_FILES = frozenset(
    {
        "apps/api/src/services/query/execution/preflight.rs",
        "apps/api/src/services/query/execution/source_context.rs",
        "apps/api/src/services/query/execution/structured_query_pipeline.rs",
        "apps/api/src/services/query/execution/technical_literal_focus.rs",
    }
)
QUERY_TARGET_MUTATION_BOUNDARIES: frozenset[str] = frozenset()
RAW_QUESTION_QUERY_IR_RECLASSIFIER = re.compile(
    r"\bfn\s+(?P<name>(?:r#)?[A-Za-z_][A-Za-z0-9_]*)\s*\("
    r"(?P<arguments>[^{};]{0,2000})\)\s*"
    r"(?P<return_type>->\s*[^{};]{0,500})?",
    re.DOTALL,
)
RAW_QUESTION_PARAMETER = re.compile(
    r"\b(?:raw_|current_|user_)?question\s*:\s*&(?:'(?a:\w)+\s+)?str\b"
)
MUTABLE_QUERY_IR_PARAMETER = re.compile(
    r"(?:&\s*mut\s*(?:(?:crate|super|self)::[A-Za-z0-9_:]+::)?QueryIR\b|"
    r"\bmut\s+[A-Za-z_][A-Za-z0-9_]*\s*:\s*"
    r"(?:(?:crate|super|self)::[A-Za-z0-9_:]+::)?QueryIR\b)"
)
QUERY_IR_PARAMETER = re.compile(
    r"\b[A-Za-z_][A-Za-z0-9_]*\s*:\s*(?:&\s*(?:mut\s*)?)?"
    r"(?:(?:crate|super|self)::[A-Za-z0-9_:]+::)?QueryIR\b"
)
QUERY_IR_RETURN_TYPE = re.compile(
    r"\b(?:(?:crate|super|self)::[A-Za-z0-9_:]+::)?QueryIR\b"
)
TYPED_GRAPH_EVIDENCE_QUERY_SLICE = re.compile(
    r"\bquery_texts\s*:\s*&\s*\[\s*RuntimeGraphEvidenceSearchQuery\s*\]"
)
TYPED_GRAPH_EVIDENCE_LITERAL_FILTER = re.compile(
    r"\bfilter_map\s*\(\s*"
    r"RuntimeGraphEvidenceSearchQuery\s*::\s*literal_or_formal_text\s*\)"
)
TYPED_GRAPH_EVIDENCE_QUERY_IR_ARGUMENT = re.compile(
    r"\bquery_ir\s*:\s*Option\s*<\s*&\s*QueryIR\s*>"
)
TYPED_GRAPH_EVIDENCE_LITERAL_SOURCE = re.compile(
    r"\bquery_ir\s*\.\s*literal_constraints\s*\.\s*iter\s*\(\s*\)"
)
TYPED_GRAPH_EVIDENCE_ENTITY_SOURCE = re.compile(
    r"\bquery_ir\s*\.\s*target_entities\s*\.\s*iter\s*\(\s*\)"
)
TYPED_GRAPH_EVIDENCE_DOCUMENT_SOURCE = re.compile(
    r"\bquery_ir\s*\.\s*document_focus\s*\.\s*iter\s*\(\s*\)"
)
TYPED_GRAPH_EVIDENCE_LITERAL_MEMBERSHIP = re.compile(
    r"\bliteral_or_formal_keys\s*\.\s*contains\s*\("
)
LITERAL_CONSTRAINT_LOOP = re.compile(
    r"\bfor\s+literal\s+in\s+&\s*ir\s*\.\s*literal_constraints\s*\{"
)
PARAMETER_QUESTION_INTENT = re.compile(
    r"\bQuestionIntent\s*::\s*Parameter\b"
)

QUERY_EXECUTION_DEFAULT_LINE_BUDGET = 800
# Ratchet only: these legacy modules already exceed the focused-file target.
# Their exact current sizes are the ceiling, so every change must hold or
# reduce the debt until the code is extracted behind explicit stage APIs.
LEGACY_QUERY_EXECUTION_LINE_BUDGETS = {
    "answer.rs": 12_628,
    "answer_pipeline.rs": 10_518,
    "canonical_answer_context.rs": 1_894,
    "consolidation.rs": 2_868,
    "context.rs": 2_655,
    "document_target.rs": 2_056,
    "graph_retrieval.rs": 2_763,
    "preflight.rs": 1_816,
    "retrieve.rs": 20_586,
    "semantic_rerank.rs": 1_454,
    "source_context.rs": 4_681,
    "structured_query_pipeline.rs": 1_182,
    "technical_answer.rs": 1_977,
}


def line_number(text: str, offset: int) -> int:
    return text.count("\n", 0, offset) + 1


def rust_sources(root: Path) -> list[Path]:
    source_root = root / "apps/api/src"
    if not source_root.is_dir():
        return []
    return sorted(source_root.rglob("*.rs"))


def contract_sources(root: Path) -> list[Path]:
    source_root = root / "crates/contracts/src"
    if not source_root.is_dir():
        return []
    return sorted(source_root.rglob("*.rs"))


def _mask_non_newlines(characters: list[str], start: int, end: int) -> None:
    for index in range(start, end):
        if characters[index] != "\n":
            characters[index] = " "


def _plain_character_literal_end(text: str, content: int) -> int | None:
    closing = content + 1
    return closing + 1 if closing < len(text) and text[closing] == "'" else None


def _hex_escape_end(text: str, escape: int) -> int | None:
    closing = escape + 3
    digits = text[escape + 1 : closing]
    if len(digits) != 2 or any(
        character not in "0123456789abcdefABCDEF" for character in digits
    ):
        return None
    return closing


def _unicode_escape_end(text: str, escape: int) -> int | None:
    if escape + 1 >= len(text) or text[escape + 1] != "{":
        return None
    brace = text.find("}", escape + 2)
    if brace < 0 or any(character in "\n\r" for character in text[escape + 2 : brace]):
        return None
    digits = text[escape + 2 : brace].replace("_", "")
    if not digits or any(
        character not in "0123456789abcdefABCDEF" for character in digits
    ):
        return None
    return brace + 1


def _escaped_character_literal_end(text: str, escape: int) -> int | None:
    if escape >= len(text) or text[escape] in {"\n", "\r"}:
        return None
    if text[escape] == "x":
        closing = _hex_escape_end(text, escape)
    elif text[escape] == "u":
        closing = _unicode_escape_end(text, escape)
    else:
        closing = escape + 1
    if closing is None:
        return None
    return closing + 1 if closing < len(text) and text[closing] == "'" else None


def _rust_character_literal_end(text: str, index: int) -> int | None:
    """Return the exclusive end of a Rust char/byte-char literal, if present."""
    quote = index
    if text.startswith("b'", index) and (
        index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_")
    ):
        quote += 1
    elif index >= len(text) or text[index] != "'":
        return None
    content = quote + 1
    if content >= len(text) or text[content] in {"'", "\n", "\r"}:
        return None
    if text[content] != "\\":
        return _plain_character_literal_end(text, content)
    return _escaped_character_literal_end(text, content + 1)


def _comment_end(text: str, index: int) -> int | None:
    if text.startswith("//", index):
        end = text.find("\n", index + 2)
        return len(text) if end < 0 else end
    if not text.startswith("/*", index):
        return None
    depth = 1
    end = index + 2
    while end < len(text) and depth:
        if text.startswith("/*", end):
            depth += 1
            end += 2
        elif text.startswith("*/", end):
            depth -= 1
            end += 2
        else:
            end += 1
    return end


def _raw_string_end(text: str, index: int) -> int | None:
    raw_match = RUST_RAW_STRING_START.match(text, index)
    if raw_match is None or (
        index > 0 and (text[index - 1].isalnum() or text[index - 1] == "_")
    ):
        return None
    delimiter = '"' + raw_match.group("hashes")
    end = text.find(delimiter, raw_match.end())
    return len(text) if end < 0 else end + len(delimiter)


def _quoted_string_end(text: str, index: int) -> int | None:
    if text[index] == '"':
        content_start = index + 1
    elif (
        index + 1 < len(text)
        and text[index] in {"b", "c"}
        and text[index + 1] == '"'
        and (index == 0 or not (text[index - 1].isalnum() or text[index - 1] == "_"))
    ):
        content_start = index + 2
    else:
        return None
    end = content_start
    escaped = False
    while end < len(text):
        character = text[end]
        end += 1
        if escaped:
            escaped = False
        elif character == "\\":
            escaped = True
        elif character == '"':
            break
    return end


def _rust_source_view(text: str, *, preserve_literals: bool) -> str:
    """Mask Rust comments and optionally literals while preserving offsets."""
    characters = list(text)
    index = 0
    while index < len(text):
        comment_end = _comment_end(text, index)
        if comment_end is not None:
            _mask_non_newlines(characters, index, comment_end)
            index = comment_end
            continue
        literal_end = _rust_character_literal_end(text, index)
        if literal_end is None:
            literal_end = _raw_string_end(text, index)
        if literal_end is None:
            literal_end = _quoted_string_end(text, index)
        if literal_end is None:
            index += 1
            continue
        if not preserve_literals:
            _mask_non_newlines(characters, index, literal_end)
        index = literal_end
    return "".join(characters)


def rust_code_without_comments_and_literals(text: str) -> str:
    """Mask Rust comments and literals while preserving offsets and line numbers."""
    return _rust_source_view(text, preserve_literals=False)


def rust_text_without_comments(text: str) -> str:
    """Mask comments while retaining string payloads for wire-literal checks."""
    return _rust_source_view(text, preserve_literals=True)


def is_rust_test_source(relative_path: Path) -> bool:
    return (
        "tests" in relative_path.parts
        or relative_path.stem == "tests"
        or relative_path.stem.endswith("_tests")
    )


def _rust_attribute_end(code: str, start: int) -> int | None:
    if not code.startswith("#[", start):
        return None
    depth = 1
    index = start + 2
    while index < len(code):
        if code[index] == "[":
            depth += 1
        elif code[index] == "]":
            depth -= 1
            if depth == 0:
                return index + 1
        index += 1
    return None


def _rust_nesting_after(
    character: str,
    parenthesis_depth: int,
    bracket_depth: int,
    brace_depth: int,
) -> tuple[int, int, int]:
    if character == "(":
        parenthesis_depth += 1
    elif character == ")":
        parenthesis_depth = max(0, parenthesis_depth - 1)
    elif character == "[":
        bracket_depth += 1
    elif character == "]":
        bracket_depth = max(0, bracket_depth - 1)
    elif character == "{":
        brace_depth += 1
    elif character == "}":
        brace_depth = max(0, brace_depth - 1)
    return parenthesis_depth, bracket_depth, brace_depth


def _rust_attributed_item_end(code: str, start: int) -> int:
    """Find a cfg-gated Rust item/statement boundary in literal-free code."""
    parenthesis_depth = bracket_depth = brace_depth = 0
    saw_top_level_brace = False
    for index in range(start, len(code)):
        character = code[index]
        was_top_level = parenthesis_depth == bracket_depth == brace_depth == 0
        saw_top_level_brace = saw_top_level_brace or (
            character == "{" and was_top_level
        )
        parenthesis_depth, bracket_depth, brace_depth = _rust_nesting_after(
            character,
            parenthesis_depth,
            bracket_depth,
            brace_depth,
        )
        is_top_level = parenthesis_depth == bracket_depth == brace_depth == 0
        if is_top_level and (
            (character == "}" and saw_top_level_brace) or character in {";", ","}
        ):
            return index + 1
    return len(code)


def _without_cfg_test_items(view: str, boundary_code: str) -> str:
    characters = list(view)
    search_from = 0
    while cfg_test := CFG_TEST_ATTRIBUTE.search(boundary_code, search_from):
        item_start = cfg_test.end()
        while True:
            item_start += len(boundary_code[item_start:]) - len(
                boundary_code[item_start:].lstrip()
            )
            attribute_end = _rust_attribute_end(boundary_code, item_start)
            if attribute_end is None:
                break
            item_start = attribute_end
        item_end = _rust_attributed_item_end(boundary_code, item_start)
        _mask_non_newlines(characters, cfg_test.start(), item_end)
        search_from = max(item_end, cfg_test.end())
    return "".join(characters)


def production_rust_code(text: str) -> str:
    code = rust_code_without_comments_and_literals(text)
    return _without_cfg_test_items(code, code)


def production_rust_text(text: str) -> str:
    """Return non-test Rust with comments masked and literal payloads intact."""
    boundary_code = rust_code_without_comments_and_literals(text)
    return _without_cfg_test_items(rust_text_without_comments(text), boundary_code)


def _matching_rust_brace_end(code: str, opening_brace: int) -> int | None:
    depth = 0
    for index in range(opening_brace, len(code)):
        if code[index] == "{":
            depth += 1
        elif code[index] == "}":
            depth -= 1
            if depth == 0:
                return index + 1
    return None


def _named_rust_function_span(
    code: str, function_name: str
) -> tuple[int, int, int] | None:
    declaration = re.search(rf"\bfn\s+{re.escape(function_name)}\s*\(", code)
    if declaration is None:
        return None
    opening_brace = code.find("{", declaration.end())
    if opening_brace < 0:
        return None
    function_end = _matching_rust_brace_end(code, opening_brace)
    if function_end is None:
        return None
    return declaration.start(), opening_brace, function_end


def _typed_graph_evidence_query_lane_violation(code: str) -> int | None:
    search_span = _named_rust_function_span(code, "search_runtime_graph_evidence_by_text")
    if search_span is None:
        return None
    search_start, search_body_start, _ = search_span
    if TYPED_GRAPH_EVIDENCE_QUERY_SLICE.search(code, search_start, search_body_start) is None:
        return search_start

    literal_span = _named_rust_function_span(
        code, "runtime_graph_evidence_literal_search_queries"
    )
    if literal_span is None:
        return search_start
    literal_start, literal_body_start, literal_end = literal_span
    if TYPED_GRAPH_EVIDENCE_QUERY_SLICE.search(
        code, literal_start, literal_body_start
    ) is None:
        return literal_start
    if TYPED_GRAPH_EVIDENCE_LITERAL_FILTER.search(
        code, literal_body_start, literal_end
    ) is None:
        return literal_start
    return None


def _typed_graph_evidence_retrieve_lane_violation(code: str) -> int | None:
    function_span = _named_rust_function_span(code, "graph_evidence_db_text_queries")
    if function_span is None:
        return None
    function_start, function_body_start, function_end = function_span
    if TYPED_GRAPH_EVIDENCE_QUERY_IR_ARGUMENT.search(
        code, function_start, function_body_start
    ) is None:
        return function_start
    for source_pattern in (
        TYPED_GRAPH_EVIDENCE_LITERAL_SOURCE,
        TYPED_GRAPH_EVIDENCE_ENTITY_SOURCE,
        TYPED_GRAPH_EVIDENCE_DOCUMENT_SOURCE,
    ):
        if source_pattern.search(code, function_body_start, function_end) is None:
            return function_start
    if TYPED_GRAPH_EVIDENCE_LITERAL_MEMBERSHIP.search(
        code, function_body_start, function_end
    ) is None:
        return function_start
    return None


def _literal_identifier_parameter_intent_violation(code: str) -> int | None:
    function_span = _named_rust_function_span(code, "classify_query_ir_intents")
    if function_span is None:
        return None
    _, function_body_start, function_end = function_span
    literal_loop = LITERAL_CONSTRAINT_LOOP.search(code, function_body_start, function_end)
    if literal_loop is None:
        return None
    loop_end = _matching_rust_brace_end(code, literal_loop.end() - 1)
    if loop_end is None:
        return literal_loop.start()
    parameter_intent = PARAMETER_QUESTION_INTENT.search(code, literal_loop.end(), loop_end)
    return parameter_intent.start() if parameter_intent is not None else None


def is_forbidden_semantic_dictionary_name(name: str) -> bool:
    normalized = name.removeprefix("r#").upper()
    padded = f"_{normalized}_"
    if any(f"_{marker}_" in padded for marker in FORMAL_PROTOCOL_IDENTIFIER_MARKERS):
        return False
    return any(
        normalized == suffix or normalized.endswith(f"_{suffix}")
        for suffix in SEMANTIC_DICTIONARY_SUFFIXES
    )


def _append_pattern_violations(
    violations: list[Violation],
    pattern: re.Pattern[str],
    code: str,
    text: str,
    relative_path: Path,
    rule: str,
    message: str,
    group: str | None = None,
) -> None:
    for match in pattern.finditer(code):
        offset = match.start(group) if group is not None else match.start()
        violations.append(Violation(relative_path, line_number(text, offset), rule, message))


def _scan_path_specific_production_rules(
    relative_path: Path,
    text: str,
    production_code: str,
    production_text: str,
) -> list[Violation]:
    violations: list[Violation] = []
    relative = relative_path.as_posix()
    if relative == "apps/api/src/interfaces/http/mcp.rs":
        _append_pattern_violations(
            violations, MCP_STATELESS_COMPATIBILITY_PATH, production_text, text,
            relative_path, "no-stateless-mcp-compatibility-path",
            "all non-initialize MCP transport requests must use the one canonical owned session scope",
        )
    typed_checks = {
        "apps/api/src/infra/repositories/runtime_graph_repository.rs": (
            _typed_graph_evidence_query_lane_violation,
            "typed-graph-evidence-query-lanes",
            "route raw questions through the lexical lane and allow literal matching only for explicitly typed compiler/formal queries",
        ),
        "apps/api/src/services/query/execution/retrieve.rs": (
            _typed_graph_evidence_retrieve_lane_violation,
            "typed-graph-evidence-query-lanes",
            "promote evidence queries to the literal lane only when they match compiler-typed QueryIR focus values",
        ),
        "apps/api/src/services/query/execution/question_intent.rs": (
            _literal_identifier_parameter_intent_violation,
            "typed-question-intent-only",
            "derive parameter intent only from QueryTargetKind::Parameter, never from identifier literal shape",
        ),
    }
    check = typed_checks.get(relative)
    if check is not None:
        offset = check[0](production_code)
        if offset is not None:
            violations.append(Violation(relative_path, line_number(text, offset), check[1], check[2]))
    if relative in RAW_SHORT_TOKEN_SEMANTIC_PROMOTION_FILES:
        _append_pattern_violations(
            violations, RAW_SHORT_TOKEN_SEMANTIC_PROMOTION, production_code, text,
            relative_path, "no-raw-short-token-semantic-promotion",
            "use typed QueryIR/planner intent; raw question token length must not promote retrieval behavior",
        )
    return violations


def _scan_semantic_dictionary_rules(
    relative_path: Path,
    text: str,
    production_code: str,
) -> list[Violation]:
    violations: list[Violation] = []
    for match in SEMANTIC_DICTIONARY_DECLARATION.finditer(production_code):
        if is_forbidden_semantic_dictionary_name(match.group("name")):
            violations.append(Violation(
                relative_path, line_number(text, match.start("name")),
                "no-semantic-routing-dictionary",
                "replace handwritten semantic dictionaries with a typed, validated policy boundary",
            ))
    if not relative_path.as_posix().startswith("apps/api/src/services/query/"):
        return violations
    local_tables = [
        match for match in QUERY_LOCAL_SEMANTIC_STRING_TABLE.finditer(text)
        if production_code[match.start():match.start() + 3] == "let"
    ]
    tables = list(QUERY_SEMANTIC_STRING_TABLE.finditer(production_code)) + local_tables
    for match in sorted(tables, key=lambda item: item.start()):
        name = match.group("name").removeprefix("r#").upper()
        is_formal = any(
            name.endswith(f"_{suffix}") or name == suffix
            for suffix in QUERY_FORMAL_STRING_TABLE_SUFFIXES
        )
        if is_forbidden_semantic_dictionary_name(name) or is_formal:
            continue
        violations.append(Violation(
            relative_path, line_number(text, match.start("name")),
            "no-query-semantic-string-table",
            "use typed QueryIR/provider policy; only formal syntax tables are permitted in query code",
        ))
    return violations


def _production_pattern_rules() -> tuple[tuple[re.Pattern[str], bool, str, str, str | None], ...]:
    return (
        (SERDE_COMPATIBILITY_ALIAS, False, "no-serde-compatibility-aliases", "pre-release wire contracts expose one canonical field name; migrate persisted data forward instead of accepting hidden aliases", None),
        (HISTORICAL_PROJECTION_STATE, True, "canonical-projection-state-vocabulary", "use text_readable/ready canonical projection states; historical spellings belong only in the forward migration", None),
        (RAW_USER_FRAGMENT_OUTPUT_MUTATOR, False, "no-raw-user-fragment-output-mutation", "never splice or elevate raw user fragments in a grounded answer; consume typed literals and verified evidence", "name"),
        (POST_GENERATION_ANSWER_SPLICER, False, "no-post-generation-answer-splicing", "never append raw question or retrieved-context fragments after answer generation; repair and verify through typed stages", "name"),
        (UNTYPED_CLARIFICATION_ROUTER, False, "typed-query-ir-clarification-only", "route clarification only from canonical QueryIR typed clarification metadata", "name"),
        (RAW_HISTORY_QUERY_RECLASSIFIER, False, "no-raw-history-query-reclassification", "pass canonical current text and typed server history to QueryCompiler instead of reconstructing intent from token overlap", "name"),
        (RAW_QUESTION_LANGUAGE_INFERENCE, False, "typed-query-language-only", "localize only from the canonical typed QueryIR language; never infer language from raw question text", None),
        (TYPED_ENUM_LABEL_LEXICAL_SEED, False, "no-language-specific-enum-lexical-seed", "use typed QueryIR control flow and evidence-bearing entity/literal surfaces; never turn English enum labels into retrieval terms", None),
        (REPAIR_QUERY_PROSE_MUTATOR, False, "no-repair-query-prose-mutation", "keep repair queries byte-exact and carry repair intent as typed internal metadata", None),
        (CLARIFICATION_REPAIR_CLASSIFICATION, False, "typed-clarification-is-terminal", "return typed grounded-answer clarifications directly; never classify them as retrieval repairs", None),
        (RAW_GROUNDED_READINESS_VERIFICATION_GATE, False, "typed-grounded-readiness-only", "consume the validated completion/readiness and repair-policy envelope instead of re-gating on a rendered verifier state", None),
        (RAW_ANSWER_DISPOSITION_RECONSTRUCTOR, False, "typed-answer-disposition-only", "consume finalizer-owned typed answer disposition instead of reconstructing terminal readiness from warning codes", "name"),
        (MCP_RAW_FINAL_READINESS, False, "typed-answer-disposition-only", "derive MCP readiness only from finalizer-owned typed answer disposition", None),
        (RAW_PROCEDURE_IDENTITY_RECLASSIFIER, False, "typed-procedure-identity-only", "consume typed QueryIR entities, document focus, and literals instead of inferring subject identity from raw text shape", "name"),
        (RAW_TECHNICAL_FOCUS_RECLASSIFIER, False, "typed-technical-focus-only", "build technical focus probes from typed QueryIR fields instead of raw-question casing or token shape", "name"),
        (PROSE_CLASSIFIER_HELPER, False, "no-prose-classifier-helper", "classify from typed error or policy data instead of rendered natural-language messages", "name"),
        (RAW_VERIFICATION_CLAIM_INFERENCE, False, "typed-verification-claims-only", "verify explicit literals or typed formal exact claims; never infer claims from prose, casing, or identifier suffixes", None),
        (ERROR_DISPLAY_STRING_ROUTING, False, "no-error-display-string-routing", "branch on typed error metadata instead of searching its Display representation", None),
        (UNFENCED_VECTOR_DELETE_CALL, False, "source-fenced-vector-deletes-only", "production vector deletes must use the atomic source/attempt-fenced API; raw delete methods are fixture/repair primitives only", "name"),
        (RAW_QUERY_TARGET_STRING_COLLECTION, False, "typed-query-target-kinds", "use a closed QueryTargetKind enum for behavioral routing; keep open ontology tags non-behavioral", None),
        (PROVIDER_MODEL_IDENTITY_CAPABILITY_ROUTING, False, "no-provider-model-name-capability-routing", "declare provider/model capabilities in typed catalog policy instead of inferring them from identifiers", None),
    )


def _scan_query_specific_production_rules(
    relative_path: Path,
    text: str,
    production_code: str,
) -> list[Violation]:
    violations: list[Violation] = []
    if relative_path.as_posix().startswith("apps/api/src/services/query/"):
        _append_pattern_violations(
            violations, RAW_PROCEDURE_SEMANTIC_TERM_MODEL, production_code, text,
            relative_path, "typed-procedure-control-only",
            "consume typed QueryIR intent/entities/literals and formal evidence structure; never derive an action or acronym term model from raw text",
            "name",
        )
    if relative_path.as_posix() not in QUERY_TARGET_MUTATION_BOUNDARIES:
        _append_pattern_violations(
            violations, QUERY_TARGET_KIND_MUTATION, production_code, text,
            relative_path, "canonical-query-target-mutation-boundary",
            "construct typed compiler output once; do not mutate semantic target kinds after construction",
        )
    for match in RAW_QUESTION_QUERY_IR_RECLASSIFIER.finditer(production_code):
        arguments = match.group("arguments")
        return_type = match.group("return_type") or ""
        has_raw_question = RAW_QUESTION_PARAMETER.search(arguments) is not None
        has_query_ir = QUERY_IR_PARAMETER.search(arguments) is not None
        mutates_or_returns_ir = (
            MUTABLE_QUERY_IR_PARAMETER.search(arguments) is not None
            or QUERY_IR_RETURN_TYPE.search(return_type) is not None
        )
        if has_raw_question and has_query_ir and mutates_or_returns_ir:
            violations.append(Violation(
                relative_path, line_number(text, match.start("name")),
                "query-compiler-sole-semantic-boundary",
                "treat typed compiler output as immutable; validate or reject it instead of reclassifying it from raw question text",
            ))
    return violations


def _scan_production_rust(
    relative_path: Path,
    text: str,
    production_code: str,
    production_text: str,
) -> list[Violation]:
    violations = _scan_path_specific_production_rules(
        relative_path, text, production_code, production_text
    )
    violations.extend(_scan_semantic_dictionary_rules(relative_path, text, production_code))
    for pattern, uses_text, rule, message, group in _production_pattern_rules():
        _append_pattern_violations(
            violations,
            pattern,
            production_text if uses_text else production_code,
            text,
            relative_path,
            rule,
            message,
            group,
        )
    violations.extend(_scan_query_specific_production_rules(relative_path, text, production_code))
    return violations


def _scan_execution_rules(
    path: Path,
    relative_path: Path,
    text: str,
    execution_root: Path,
) -> list[Violation]:
    if not path.is_relative_to(execution_root):
        return []
    violations: list[Violation] = []
    execution_relative = path.relative_to(execution_root)
    is_test_source = "tests" in execution_relative.parts or path.stem.endswith("_tests")
    if not is_test_source:
        line_budget = LEGACY_QUERY_EXECUTION_LINE_BUDGETS.get(
            execution_relative.as_posix(), QUERY_EXECUTION_DEFAULT_LINE_BUDGET
        )
        line_count = len(text.splitlines())
        if line_count > line_budget:
            violations.append(Violation(
                relative_path, line_budget + 1, "query-execution-file-line-budget",
                f"split the stage behind an explicit API; {line_count} lines exceed the {line_budget}-line ratchet",
            ))
        _append_pattern_violations(
            violations, GLOB_IMPORT, text, text, relative_path,
            "no-query-execution-glob-import",
            "import the exact query-stage dependencies; production glob imports hide architectural coupling",
        )
    _append_pattern_violations(
        violations, GLOB_REEXPORT, text, text, relative_path,
        "no-query-execution-glob-reexport",
        "export an explicit stage API instead of flattening query execution modules",
    )
    if path.name in {"answer.rs", "answer_pipeline.rs"}:
        _append_pattern_violations(
            violations, ANSWER_STAGE_RETRIEVAL_DEPENDENCY, text, text, relative_path,
            "no-answer-to-retrieve-dependency",
            "move shared policy/mapping into a neutral module; answer must not call retrieval implementation",
        )
    if path.name == "chunk_support.rs":
        _append_pattern_violations(
            violations, CHUNK_SUPPORT_RETRIEVAL_DEPENDENCY, text, text, relative_path,
            "no-chunk-support-to-retrieve-dependency",
            "chunk support is a neutral boundary and must not import or call retrieval implementation",
        )
    return violations


def _scan_common_rust_rules(
    path: Path,
    relative_path: Path,
    text: str,
) -> list[Violation]:
    violations: list[Violation] = []
    _append_pattern_violations(
        violations, BLANKET_CLIPPY_ALLOW, text, text, relative_path,
        "no-blanket-clippy-all",
        "replace blanket clippy suppression with the narrowest item-level allow",
        "lint",
    )
    _append_pattern_violations(
        violations, DISCARDED_ATOMIC_PROJECTION_RESULT, text, text, relative_path,
        "no-discarded-atomic-projection-result",
        "propagate or explicitly handle the atomic projection write result",
    )
    if path.name != "completion_policy.rs":
        _append_pattern_violations(
            violations, RAW_QUESTION_COMPLETION_EVALUATION, text, text, relative_path,
            "no-raw-question-completion-evaluation",
            "build AnswerCompletionContract from canonical QueryIR before evaluating an answer",
        )
    if relative_path.as_posix() not in {
        "apps/api/src/services/query/compiler.rs",
        "apps/api/src/services/query/execution/answer_pipeline.rs",
    }:
        _append_pattern_violations(
            violations, PROVIDER_FREE_FALLBACK_QUERY_IR, text, text, relative_path,
            "no-downstream-provider-free-query-ir",
            "propagate canonical typed QueryIR instead of reclassifying a raw question downstream",
        )
    return violations


def _scan_rust_file(path: Path, root: Path, execution_root: Path) -> list[Violation]:
    text = path.read_text(encoding="utf-8")
    relative_path = path.relative_to(root)
    violations: list[Violation] = []
    if not is_rust_test_source(relative_path):
        violations.extend(_scan_production_rust(
            relative_path,
            text,
            production_rust_code(text),
            production_rust_text(text),
        ))
    violations.extend(_scan_execution_rules(path, relative_path, text, execution_root))
    violations.extend(_scan_common_rust_rules(path, relative_path, text))
    return violations


def _scan_contract_rules(root: Path) -> list[Violation]:
    violations: list[Violation] = []
    for path in contract_sources(root):
        text = path.read_text(encoding="utf-8")
        _append_pattern_violations(
            violations, PLACEHOLDER_CONTRACT_DOC, text, text, path.relative_to(root),
            "meaningful-contract-documentation",
            "describe the field, variant, or invariant itself; lint-filler that restates the Rust identifier is not documentation",
        )
    return violations


def _scan_frontend_dockerfile(root: Path) -> list[Violation]:
    path = root / "apps/web/Dockerfile"
    if not path.is_file():
        return []
    lines = path.read_text(encoding="utf-8").splitlines()
    npm_ci_line = next(
        (index for index, content in enumerate(lines) if content.strip().startswith("RUN npm ci")),
        None,
    )
    if npm_ci_line is None:
        return []
    early_configuration = next(
        (
            index for index, content in enumerate(lines[:npm_ci_line])
            if FRONTEND_BUILD_CONFIGURATION.match(content.strip())
        ),
        None,
    )
    if early_configuration is None:
        return []
    return [Violation(
        path.relative_to(root), early_configuration + 1,
        "frontend-dependencies-before-build-args",
        "install locked dependencies before build-only ARG/ENV declarations so version changes reuse the dependency layer",
    )]


def scan_repository(root: Path) -> list[Violation]:
    root = root.resolve()
    execution_root = root / "apps/api/src/services/query/execution"
    violations = [
        violation
        for path in rust_sources(root)
        for violation in _scan_rust_file(path, root, execution_root)
    ]
    violations.extend(_scan_contract_rules(root))
    violations.extend(_scan_frontend_dockerfile(root))
    return sorted(violations, key=lambda item: (str(item.path), item.line, item.rule))


def parse_args(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[2],
        help="repository root (defaults to the root containing scripts/)",
    )
    return parser.parse_args(argv)


def main(argv: Sequence[str] | None = None) -> int:
    args = parse_args(argv)
    violations = scan_repository(args.root)
    for violation in violations:
        print(
            f"{violation.path}:{violation.line} [{violation.rule}] {violation.message}",
            file=sys.stderr,
        )
    if violations:
        print(f"architecture lint failed: {len(violations)} violation(s)", file=sys.stderr)
        return 1
    print("architecture lint passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
