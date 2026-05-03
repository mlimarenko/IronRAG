use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query::{QueryVerificationState, QueryVerificationWarning},
    infra::arangodb::document_store::KnowledgeTechnicalFactRow,
    services::query::assistant_grounding::AssistantGroundingEvidence,
    services::query::planner::QueryIntentProfile,
};

use super::types::{CanonicalAnswerEvidence, RuntimeAnswerVerification, RuntimeMatchedChunk};

pub(crate) fn verify_answer_against_canonical_evidence(
    _question: &str,
    answer: &str,
    intent_profile: &QueryIntentProfile,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    prompt_context: &str,
    assistant_grounding: &AssistantGroundingEvidence,
) -> RuntimeAnswerVerification {
    if answer.trim().is_empty() {
        return RuntimeAnswerVerification {
            state: QueryVerificationState::Failed,
            warnings: vec![QueryVerificationWarning {
                code: "empty_answer".to_string(),
                message: "Answer generation returned empty output.".to_string(),
                related_segment_id: None,
                related_fact_id: None,
            }],
            unsupported_literals: Vec::new(),
        };
    }

    let (inline_literals, fenced_line_literals) = extract_answer_literals(answer);
    let mut normalized_corpus = build_verification_corpus(evidence, chunks, assistant_grounding);
    // Library summary, document file names, document titles and other prompt
    // metadata are part of what the LLM saw — include the whole rendered
    // prompt context so file-name backticks like `customers.csv` are not
    // marked as hallucinations.
    let normalized_prompt_context = normalize_verification_literal(prompt_context);
    if !normalized_prompt_context.is_empty() {
        normalized_corpus.push(normalized_prompt_context);
    }
    let mut warnings = Vec::<QueryVerificationWarning>::new();
    let mut unsupported_literals = Vec::<String>::new();
    for literal in inline_literals.iter().chain(fenced_line_literals.iter()) {
        let normalized_literal = normalize_verification_literal(literal);
        if normalized_literal.is_empty() {
            continue;
        }
        if !literal_is_supported_by_canonical_corpus(literal, &normalized_corpus) {
            unsupported_literals.push(literal.clone());
            warnings.push(QueryVerificationWarning {
                code: "unsupported_literal".to_string(),
                message: format!("Literal `{literal}` is not grounded in selected evidence."),
                related_segment_id: None,
                related_fact_id: None,
            });
        }
    }
    let has_unsupported_literals =
        warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let has_any_backticked_literals =
        !inline_literals.is_empty() || !fenced_line_literals.is_empty();
    let has_grounded_backticked_literals = has_any_backticked_literals && !has_unsupported_literals;
    let should_check_conflicting_evidence =
        intent_profile.exact_literal_technical && !has_grounded_backticked_literals;
    let conflicting_groups = if should_check_conflicting_evidence {
        collect_conflicting_fact_groups(&evidence.technical_facts)
    } else {
        HashMap::new()
    };
    if !conflicting_groups.is_empty() {
        warnings.push(QueryVerificationWarning {
            code: "conflicting_evidence".to_string(),
            message: format!(
                "Selected evidence contains {} conflicting technical fact group(s).",
                conflicting_groups.len()
            ),
            related_segment_id: None,
            related_fact_id: None,
        });
    }

    let lower_answer = answer.to_ascii_lowercase();
    let insufficient = lower_answer.contains("no grounded evidence")
        || lower_answer.contains("exact value is not grounded");
    let has_conflicting_evidence =
        warnings.iter().any(|warning| warning.code == "conflicting_evidence");
    let has_unsupported_canonical_claim =
        warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");
    let state = if insufficient || has_unsupported_literals || has_unsupported_canonical_claim {
        QueryVerificationState::InsufficientEvidence
    } else if has_conflicting_evidence {
        QueryVerificationState::Conflicting
    } else {
        QueryVerificationState::Verified
    };

    RuntimeAnswerVerification { state, warnings, unsupported_literals }
}

fn extract_answer_literals(answer: &str) -> (Vec<String>, Vec<String>) {
    let mut inline = Vec::<String>::new();
    let mut fenced_lines = Vec::<String>::new();
    let mut seen_inline = HashSet::<String>::new();
    let mut seen_fenced = HashSet::<String>::new();

    let bytes = answer.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes[cursor..].starts_with(b"```") {
            let body_start = cursor + 3;
            if let Some(relative) = find_subslice(&bytes[body_start..], b"```") {
                let body_end = body_start + relative;
                let body = &answer[body_start..body_end];
                for line in fenced_block_content_lines(body) {
                    if seen_fenced.insert(line.clone()) {
                        fenced_lines.push(line);
                    }
                }
                cursor = body_end + 3;
                continue;
            } else {
                break;
            }
        }
        if bytes[cursor] == b'`' {
            let literal_start = cursor + 1;
            if let Some(relative) = find_byte(&bytes[literal_start..], b'`') {
                let literal_end = literal_start + relative;
                let literal = answer[literal_start..literal_end].trim().to_string();
                if !literal.is_empty() && seen_inline.insert(literal.clone()) {
                    inline.push(literal);
                }
                cursor = literal_end + 1;
                continue;
            } else {
                break;
            }
        }
        cursor += utf8_char_len(bytes[cursor]);
    }

    (inline, fenced_lines)
}

fn fenced_block_content_lines(body: &str) -> Vec<String> {
    let trimmed = body.strip_prefix('\n').or_else(|| body.strip_prefix("\r\n")).unwrap_or(body);
    let mut raw_lines: Vec<&str> = trimmed.split('\n').collect();
    if let Some(first) = raw_lines.first() {
        if is_language_hint(first.trim()) {
            raw_lines.remove(0);
        }
    }
    raw_lines
        .into_iter()
        .map(|line| line.trim_end_matches('\r').trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn is_language_hint(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > 20 {
        return false;
    }
    candidate
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '+' || ch == '_' || ch == '.')
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|byte| *byte == needle)
}

fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        b if b < 0x80 => 1,
        b if b & 0xE0 == 0xC0 => 2,
        b if b & 0xF0 == 0xE0 => 3,
        b if b & 0xF8 == 0xF0 => 4,
        _ => 1,
    }
}

fn build_verification_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
    assistant_grounding: &AssistantGroundingEvidence,
) -> Vec<String> {
    let mut corpus = Vec::<String>::new();
    for fact in &evidence.technical_facts {
        corpus.push(normalize_verification_literal(&fact.display_value));
        corpus.push(normalize_verification_literal(&fact.canonical_value_text));
        if let Ok(qualifiers) = serde_json::from_value::<
            Vec<crate::shared::extraction::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        {
            for qualifier in qualifiers {
                corpus.push(normalize_verification_literal(&qualifier.key));
                corpus.push(normalize_verification_literal(&qualifier.value));
            }
        }
    }
    for block in &evidence.structured_blocks {
        corpus.push(normalize_verification_literal(&block.text));
        corpus.push(normalize_verification_literal(&block.normalized_text));
    }
    for chunk in &evidence.chunk_rows {
        corpus.push(normalize_verification_literal(&chunk.content_text));
        corpus.push(normalize_verification_literal(&chunk.normalized_text));
    }
    for chunk in chunks {
        corpus.push(normalize_verification_literal(&chunk.source_text));
        corpus.push(normalize_verification_literal(&chunk.excerpt));
    }
    for fragment in &assistant_grounding.verification_corpus {
        corpus.push(normalize_verification_literal(fragment));
    }
    corpus.retain(|value| !value.is_empty());
    corpus
}

fn literal_is_supported_by_canonical_corpus(literal: &str, corpus: &[String]) -> bool {
    let normalized_literal = normalize_verification_literal(literal);
    if normalized_literal.is_empty() {
        return true;
    }
    if corpus.iter().any(|candidate| candidate.contains(&normalized_literal)) {
        return true;
    }
    let Some((method, path)) = split_http_literal(literal) else {
        return false;
    };
    let normalized_method = normalize_verification_literal(method);
    let normalized_path = normalize_verification_literal(path);
    !normalized_method.is_empty()
        && !normalized_path.is_empty()
        && corpus.iter().any(|candidate| candidate.contains(&normalized_method))
        && corpus.iter().any(|candidate| candidate.contains(&normalized_path))
}

fn normalize_verification_literal(value: &str) -> String {
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&'
            && let Some(decoded) = decode_html_entity(&mut chars)
        {
            normalized.extend(decoded.to_lowercase());
            continue;
        }
        if ch.is_whitespace() {
            continue;
        }
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && is_markdown_escaped_literal_punctuation(next)
        {
            let escaped = chars.next().expect("peeked char must exist");
            normalized.extend(escaped.to_lowercase());
            continue;
        }
        normalized.extend(ch.to_lowercase());
    }
    normalized
}

fn decode_html_entity<I>(chars: &mut std::iter::Peekable<I>) -> Option<char>
where
    I: Iterator<Item = char> + Clone,
{
    let mut entity = String::new();
    let mut probe = chars.clone();
    let mut saw_semicolon = false;
    while let Some(next) = probe.next() {
        if next == ';' {
            saw_semicolon = true;
            break;
        }
        if entity.len() >= 16 || next.is_whitespace() {
            return None;
        }
        entity.push(next);
    }
    if !saw_semicolon {
        return None;
    }
    let decoded = match entity.as_str() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        value if value.starts_with("#x") || value.starts_with("#X") => {
            let codepoint = u32::from_str_radix(&value[2..], 16).ok()?;
            char::from_u32(codepoint)?
        }
        value if value.starts_with('#') => {
            let codepoint = value[1..].parse::<u32>().ok()?;
            char::from_u32(codepoint)?
        }
        _ => return None,
    };
    for _ in 0..entity.chars().count() {
        chars.next();
    }
    chars.next();
    Some(decoded)
}

fn is_markdown_escaped_literal_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation() && ch != '/' && ch != '\\'
}

fn split_http_literal(literal: &str) -> Option<(&str, &str)> {
    let trimmed = literal.trim();
    for method in ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"] {
        let Some(rest) = trimmed.strip_prefix(method) else {
            continue;
        };
        let path = rest.trim();
        if path.starts_with('/') || path.starts_with("http://") || path.starts_with("https://") {
            return Some((method, path));
        }
    }
    None
}

fn collect_conflicting_fact_groups(
    facts: &[KnowledgeTechnicalFactRow],
) -> HashMap<String, BTreeSet<String>> {
    let mut groups = HashMap::<String, BTreeSet<String>>::new();
    for fact in facts {
        let Some(group_id) = fact.conflict_group_id.as_ref() else {
            continue;
        };
        groups.entry(group_id.clone()).or_default().insert(fact.canonical_value_text.clone());
    }
    groups.into_iter().filter(|(_, values)| values.len() > 1).collect()
}

pub(crate) async fn persist_query_verification(
    state: &AppState,
    execution_id: Uuid,
    verification: &RuntimeAnswerVerification,
    canonical_evidence: &CanonicalAnswerEvidence,
    assistant_grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<()> {
    let Some(bundle) =
        state.arango_context_store.get_bundle_by_query_execution(execution_id).await.with_context(
            || format!("failed to load context bundle for verification {execution_id}"),
        )?
    else {
        return Ok(());
    };
    let warnings_json = serde_json::to_value(&verification.warnings)
        .context("failed to serialize verification warnings")?;
    let candidate_summary = enrich_query_candidate_summary(
        bundle.candidate_summary.clone(),
        canonical_evidence,
        assistant_grounding,
    );
    let assembly_diagnostics = enrich_query_assembly_diagnostics(
        bundle.assembly_diagnostics.clone(),
        verification,
        &candidate_summary,
        assistant_grounding,
    );
    let _ = state
        .arango_context_store
        .update_bundle_state(
            bundle.bundle_id,
            &bundle.bundle_state,
            &bundle.selected_fact_ids,
            verification_state_label(verification.state),
            warnings_json,
            bundle.freshness_snapshot,
            candidate_summary,
            assembly_diagnostics,
        )
        .await
        .context("failed to persist query verification state")?;
    Ok(())
}

fn verification_state_label(state: QueryVerificationState) -> &'static str {
    match state {
        QueryVerificationState::Verified => "verified",
        QueryVerificationState::PartiallySupported => "partially_supported",
        QueryVerificationState::Conflicting => "conflicting_evidence",
        QueryVerificationState::InsufficientEvidence => "insufficient_evidence",
        QueryVerificationState::Failed => "failed",
        QueryVerificationState::NotRun => "not_run",
    }
}

pub(crate) fn enrich_query_candidate_summary(
    candidate_summary: serde_json::Value,
    canonical_evidence: &CanonicalAnswerEvidence,
    assistant_grounding: &AssistantGroundingEvidence,
) -> serde_json::Value {
    let mut summary = candidate_summary;
    let Some(object) = summary.as_object_mut() else {
        return summary;
    };
    object.insert(
        "finalPreparedSegmentReferences".to_string(),
        serde_json::json!(canonical_evidence.structured_blocks.len()),
    );
    object.insert(
        "finalTechnicalFactReferences".to_string(),
        serde_json::json!(canonical_evidence.technical_facts.len()),
    );
    object.insert(
        "finalChunkReferences".to_string(),
        serde_json::json!(canonical_evidence.chunk_rows.len()),
    );
    object.insert(
        "finalAssistantDocumentReferences".to_string(),
        serde_json::json!(assistant_grounding.document_references.len()),
    );
    summary
}

pub(crate) fn enrich_query_assembly_diagnostics(
    assembly_diagnostics: serde_json::Value,
    verification: &RuntimeAnswerVerification,
    candidate_summary: &serde_json::Value,
    assistant_grounding: &AssistantGroundingEvidence,
) -> serde_json::Value {
    let mut diagnostics = assembly_diagnostics;
    let Some(object) = diagnostics.as_object_mut() else {
        return diagnostics;
    };
    object.insert(
        "verificationState".to_string(),
        serde_json::Value::String(verification_state_label(verification.state).to_string()),
    );
    object.insert(
        "verificationWarnings".to_string(),
        serde_json::to_value(&verification.warnings).unwrap_or_else(|_| serde_json::json!([])),
    );
    object.insert(
        "graphParticipation".to_string(),
        serde_json::json!({
            "entityReferenceCount": json_count(candidate_summary, "finalEntityReferences"),
            "relationReferenceCount": json_count(candidate_summary, "finalRelationReferences"),
            "graphBacked": json_count(candidate_summary, "finalEntityReferences") > 0
                || json_count(candidate_summary, "finalRelationReferences") > 0,
        }),
    );
    object.insert(
        "structuredEvidence".to_string(),
        serde_json::json!({
            "preparedSegmentReferenceCount": json_count(candidate_summary, "finalPreparedSegmentReferences"),
            "technicalFactReferenceCount": json_count(candidate_summary, "finalTechnicalFactReferences"),
            "chunkReferenceCount": json_count(candidate_summary, "finalChunkReferences"),
            "assistantDocumentReferenceCount": json_count(candidate_summary, "finalAssistantDocumentReferences"),
        }),
    );
    if !assistant_grounding.document_references.is_empty() {
        object.insert(
            "assistantGrounding".to_string(),
            serde_json::json!({
                "documentReferenceCount": assistant_grounding.document_references.len(),
                "documentReferences": assistant_grounding.document_references,
            }),
        );
    }
    diagnostics
}

fn json_count(value: &serde_json::Value, key: &str) -> usize {
    value
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| usize::try_from(count).ok())
        .unwrap_or_default()
}
