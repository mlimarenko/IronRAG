mod identifiers;
mod literals;

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::{
    domains::knowledge::TypedTechnicalFact,
    services::ingest::cancellation::{StageError, ensure_not_cancelled},
    shared::extraction::{
        structured_document::{StructuredBlockData, StructuredBlockKind},
        table_summary::is_table_summary_text,
        technical_facts::{
            TechnicalFactConflict, TechnicalFactKind, TechnicalFactQualifier, TechnicalFactValue,
            collapse_literal_whitespace, normalize_technical_fact_value,
        },
    },
};

use self::identifiers::{
    extract_code_identifier_candidates, extract_config_key_candidates,
    extract_environment_variable_candidates, extract_error_code_candidates,
    extract_version_candidates,
};

use self::literals::{
    extract_auth_rule_candidates, extract_endpoint_candidates,
    extract_http_response_status_literals, extract_parameter_candidates, extract_path_like_token,
    extract_port_candidates, extract_port_from_url, extract_protocol_candidates,
    extract_status_code_candidates, extract_url_candidates, extract_url_like_token,
    is_prefixed_semver_literal, normalize_http_method, technical_tokens, trim_technical_token,
};

const TECHNICAL_FACT_NAMESPACE: Uuid = Uuid::from_u128(0x8c79_60e4_40fd_4ad8_b5d3_4d93_d93d_4021);
const TECHNICAL_CONFLICT_NAMESPACE: Uuid =
    Uuid::from_u128(0x5a73_4a11_83fd_4f3b_abcd_c03f_f8f8_b9f0);
const HTTP_METHODS: [&str; 8] =
    ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT"];
const PROTOCOLS: [&str; 8] = ["http", "https", "tcp", "udp", "ws", "wss", "grpc", "soap"];

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractTechnicalFactsCommand {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub blocks: Vec<StructuredBlockData>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtractTechnicalFactsResult {
    pub facts: Vec<TypedTechnicalFact>,
    pub conflicts: Vec<TechnicalFactConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TechnicalFactExtractionFailureCode {
    EmptyBlocks,
}

impl TechnicalFactExtractionFailureCode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::EmptyBlocks => "empty_blocks",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactExtractionFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
struct FactCandidate {
    fact_kind: TechnicalFactKind,
    normalized_value: TechnicalFactValue,
    display_value: String,
    qualifiers: Vec<TechnicalFactQualifier>,
    support_block_ids: BTreeSet<Uuid>,
    confidence: f64,
    extraction_kind: String,
    scope_signature: String,
    rank: u8,
}

#[derive(Debug, Clone)]
struct FactAggregate {
    fact_kind: TechnicalFactKind,
    normalized_value: TechnicalFactValue,
    display_value: String,
    qualifiers: Vec<TechnicalFactQualifier>,
    support_block_ids: BTreeSet<Uuid>,
    confidence: f64,
    extraction_kind: String,
    scope_signature: String,
    rank: u8,
}

#[derive(Clone, Default)]
pub struct TechnicalFactService;

impl TechnicalFactService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub fn extract_from_blocks(
        &self,
        command: ExtractTechnicalFactsCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<ExtractTechnicalFactsResult, StageError> {
        ensure_not_cancelled(cancellation_token)?;
        let mut candidates = Vec::new();
        for block in &command.blocks {
            ensure_not_cancelled(cancellation_token)?;
            let block_text = preferred_block_text(block);
            let lines = logical_lines(&block_text);
            for line in &lines {
                ensure_not_cancelled(cancellation_token)?;
                // Block-family dispatch: each block kind runs only the
                // extractors that are semantically relevant to it.
                // The old loop called all 14 extractors on every line,
                // producing false positives from cross-domain matching
                // (e.g. URL extractor on code comments, code identifier
                // extractor on prose text).
                match block.block_kind {
                    StructuredBlockKind::CodeBlock => {
                        // Code blocks: identifiers (AST or heuristic),
                        // env vars, config keys, URLs in comments, versions.
                        candidates.extend(extract_code_identifier_candidates(block, line));
                        candidates.extend(extract_environment_variable_candidates(block, line));
                        candidates.extend(extract_config_key_candidates(block, line));
                        candidates.extend(extract_url_candidates(block, line));
                        candidates.extend(extract_version_candidates(block, line));
                        candidates.extend(extract_error_code_candidates(block, line));
                    }
                    StructuredBlockKind::EndpointBlock => {
                        // Endpoint specs: full HTTP surface extraction.
                        candidates.extend(extract_url_candidates(block, line));
                        candidates.extend(extract_endpoint_candidates(block, line));
                        candidates.extend(extract_port_candidates(block, line));
                        candidates.extend(extract_status_code_candidates(block, line));
                        candidates.extend(extract_protocol_candidates(block, line));
                        candidates.extend(extract_parameter_candidates(block, line));
                        candidates.extend(extract_auth_rule_candidates(block, line));
                    }
                    StructuredBlockKind::Table | StructuredBlockKind::TableRow => {
                        // Tables: full endpoint surface + config keys.
                        // API reference docs commonly use tables for endpoint
                        // catalogs, so HTTP methods and endpoints must run here.
                        candidates.extend(extract_url_candidates(block, line));
                        candidates.extend(extract_endpoint_candidates(block, line));
                        candidates.extend(extract_parameter_candidates(block, line));
                        candidates.extend(extract_status_code_candidates(block, line));
                        candidates.extend(extract_port_candidates(block, line));
                        candidates.extend(extract_protocol_candidates(block, line));
                        candidates.extend(extract_config_key_candidates(block, line));
                        candidates.extend(extract_version_candidates(block, line));
                    }
                    StructuredBlockKind::MetadataBlock | StructuredBlockKind::SourceUnit => {
                        // Metadata-like source units: config keys, env vars, versioned identifiers.
                        candidates.extend(extract_config_key_candidates(block, line));
                        candidates.extend(extract_environment_variable_candidates(block, line));
                        // Branded identifier heuristics removed — they
                        // produced noisy entity guesses from heading phrases.
                        // Entity extraction is the LLM's job.
                        candidates.extend(extract_version_candidates(block, line));
                    }
                    StructuredBlockKind::Heading => {
                        // Headings: branded identifiers, versions.
                        // Branded identifier heuristics removed — they
                        // produced noisy entity guesses from heading phrases.
                        // Entity extraction is the LLM's job.
                        candidates.extend(extract_version_candidates(block, line));
                    }
                    StructuredBlockKind::ListItem => {
                        // List items: catalog links, URLs, parameters, versions,
                        // env vars, config keys, error codes.
                        // Catalog link identifiers removed — same reason
                        // as branded identifiers above.
                        candidates.extend(extract_url_candidates(block, line));
                        candidates.extend(extract_parameter_candidates(block, line));
                        candidates.extend(extract_environment_variable_candidates(block, line));
                        candidates.extend(extract_config_key_candidates(block, line));
                        candidates.extend(extract_version_candidates(block, line));
                        candidates.extend(extract_error_code_candidates(block, line));
                    }
                    _ => {
                        // Paragraphs and other prose: URLs, versions, error
                        // codes, protocols, env vars ($DATABASE_URL in docs).
                        // No code identifiers or config keys — those are too
                        // noisy on narrative text.
                        candidates.extend(extract_url_candidates(block, line));
                        candidates.extend(extract_version_candidates(block, line));
                        candidates.extend(extract_error_code_candidates(block, line));
                        candidates.extend(extract_protocol_candidates(block, line));
                        candidates.extend(extract_port_candidates(block, line));
                        candidates.extend(extract_environment_variable_candidates(block, line));
                    }
                }
            }
        }

        ensure_not_cancelled(cancellation_token)?;
        let mut facts_with_scope = finalize_candidates(&command, candidates);
        ensure_not_cancelled(cancellation_token)?;
        let conflicts = assign_conflicts(&mut facts_with_scope);
        let mut facts =
            facts_with_scope.into_iter().map(|(fact, _scope_signature)| fact).collect::<Vec<_>>();
        ensure_not_cancelled(cancellation_token)?;
        facts.sort_by(|left, right| {
            left.fact_kind
                .as_str()
                .cmp(right.fact_kind.as_str())
                .then_with(|| left.display_value.cmp(&right.display_value))
                .then_with(|| left.fact_id.cmp(&right.fact_id))
        });

        Ok(ExtractTechnicalFactsResult { facts, conflicts })
    }

    pub fn extract_runtime_stage(
        &self,
        command: ExtractTechnicalFactsCommand,
        cancellation_token: &CancellationToken,
    ) -> Result<ExtractTechnicalFactsResult, TechnicalFactExtractionFailure> {
        if command.blocks.is_empty() {
            return Err(TechnicalFactExtractionFailure {
                code: TechnicalFactExtractionFailureCode::EmptyBlocks.as_str().to_string(),
                summary: "technical fact extraction requires at least one structured block"
                    .to_string(),
            });
        }

        self.extract_from_blocks(command, cancellation_token).map_err(|error| {
            TechnicalFactExtractionFailure {
                code: "stage_cancelled".to_string(),
                summary: error.to_string(),
            }
        })
    }
}

fn preferred_block_text(block: &StructuredBlockData) -> String {
    if block.block_kind == StructuredBlockKind::Table {
        return String::new();
    }
    if block.block_kind == StructuredBlockKind::TableRow {
        return preferred_table_row_text(block);
    }
    if block.block_kind == StructuredBlockKind::MetadataBlock
        && is_table_summary_text(&block.normalized_text)
    {
        return String::new();
    }
    if block.normalized_text.trim().is_empty() {
        block.text.clone()
    } else {
        block.normalized_text.clone()
    }
}

fn preferred_table_row_text(block: &StructuredBlockData) -> String {
    let normalized_text = block.normalized_text.trim();
    if normalized_text.is_empty() {
        return block.text.clone();
    }
    if !normalized_text.starts_with("Sheet: ") && !normalized_text.contains(" | Row ") {
        return block.text.clone();
    }
    let segments = normalized_text
        .split(" | ")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .filter(|segment| {
            !segment.starts_with("Sheet: ")
                && !segment.starts_with("Table: ")
                && !segment.starts_with("Row ")
        })
        .collect::<Vec<_>>();
    if !table_row_has_formal_technical_signal(&segments) {
        return String::new();
    }
    segments.join(" | ")
}

fn table_row_has_formal_technical_signal(segments: &[&str]) -> bool {
    let mut has_url = false;
    let mut has_explicit_port = false;
    let mut has_http_method = false;
    let mut has_endpoint_path = false;
    let mut has_protocol = false;
    let mut has_prefixed_semver = false;

    for segment in segments {
        let value = segment.split_once(": ").map_or(*segment, |(_, value)| value);
        if !extract_http_response_status_literals(value).is_empty() {
            return true;
        }

        for token in technical_tokens(value) {
            if let Some(url) = extract_url_like_token(&token) {
                has_url = true;
                has_explicit_port |= extract_port_from_url(&url).is_some();
            }
            has_http_method |= normalize_http_method(&token).is_some();
            has_endpoint_path |= extract_path_like_token(&token).is_some();
            has_protocol |=
                PROTOCOLS.contains(&trim_technical_token(&token).to_ascii_lowercase().as_str());
            has_prefixed_semver |= is_prefixed_semver_literal(trim_technical_token(&token));
        }
    }

    // A lone URL in an arbitrary business table is not sufficient evidence.
    // Require two independent formal grammars; otherwise abstain.
    [
        has_url,
        has_explicit_port,
        has_http_method,
        has_endpoint_path,
        has_protocol,
        has_prefixed_semver,
    ]
    .into_iter()
    .filter(|matched| *matched)
    .count()
        >= 2
}

fn logical_lines(block_text: &str) -> Vec<String> {
    block_text
        .lines()
        .map(collapse_literal_whitespace)
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn finalize_candidates(
    command: &ExtractTechnicalFactsCommand,
    candidates: Vec<FactCandidate>,
) -> Vec<(TypedTechnicalFact, String)> {
    let mut aggregates =
        BTreeMap::<(TechnicalFactKind, String, String, String), FactAggregate>::new();

    for candidate in candidates {
        let normalized_string = candidate.normalized_value.normalized_string();
        let qualifier_key = qualifier_signature(&candidate.qualifiers);
        let scope_signature = candidate.scope_signature.clone();
        let key = (candidate.fact_kind, normalized_string, qualifier_key, scope_signature.clone());
        let aggregate = aggregates.entry(key).or_insert_with(|| FactAggregate {
            fact_kind: candidate.fact_kind,
            normalized_value: candidate.normalized_value.clone(),
            display_value: candidate.display_value.clone(),
            qualifiers: candidate.qualifiers.clone(),
            support_block_ids: BTreeSet::new(),
            confidence: candidate.confidence,
            extraction_kind: candidate.extraction_kind.clone(),
            scope_signature: scope_signature.clone(),
            rank: candidate.rank,
        });
        aggregate.support_block_ids.extend(candidate.support_block_ids);
        if candidate.rank >= aggregate.rank {
            aggregate.display_value = candidate.display_value;
            aggregate.extraction_kind = candidate.extraction_kind;
            aggregate.rank = candidate.rank;
        }
        if candidate.confidence > aggregate.confidence {
            aggregate.confidence = candidate.confidence;
        }
    }

    aggregates
        .into_values()
        .map(|aggregate| {
            let normalized_string = aggregate.normalized_value.normalized_string();
            let fact_id = Uuid::new_v5(
                &TECHNICAL_FACT_NAMESPACE,
                format!(
                    "{}:{}:{}:{}",
                    command.revision_id,
                    aggregate.fact_kind.as_str(),
                    normalized_string,
                    aggregate.scope_signature,
                )
                .as_bytes(),
            );
            (
                TypedTechnicalFact {
                    fact_id,
                    revision_id: command.revision_id,
                    document_id: command.document_id,
                    workspace_id: command.workspace_id,
                    library_id: command.library_id,
                    fact_kind: aggregate.fact_kind,
                    normalized_value: aggregate.normalized_value,
                    display_value: aggregate.display_value,
                    qualifiers: aggregate.qualifiers,
                    support_block_ids: aggregate.support_block_ids.into_iter().collect(),
                    support_chunk_ids: Vec::new(),
                    confidence: Some(aggregate.confidence),
                    extraction_kind: aggregate.extraction_kind,
                    conflict_group_id: None,
                    created_at: Utc::now(),
                },
                aggregate.scope_signature,
            )
        })
        .collect()
}

fn assign_conflicts(
    facts_with_scope: &mut [(TypedTechnicalFact, String)],
) -> Vec<TechnicalFactConflict> {
    let mut scope_groups = BTreeMap::<(TechnicalFactKind, String), Vec<usize>>::new();
    for (index, (fact, scope_signature)) in facts_with_scope.iter().enumerate() {
        scope_groups.entry((fact.fact_kind, scope_signature.clone())).or_default().push(index);
    }

    let mut conflicts = Vec::new();
    for ((fact_kind, scope_signature), indices) in scope_groups {
        let distinct_values = indices
            .iter()
            .map(|index| facts_with_scope[*index].0.normalized_value.normalized_string())
            .collect::<BTreeSet<_>>();
        if distinct_values.len() <= 1 {
            continue;
        }

        let conflict_group_uuid = Uuid::new_v5(
            &TECHNICAL_CONFLICT_NAMESPACE,
            format!("{fact_kind:?}:{scope_signature}").as_bytes(),
        );
        let conflict_group_id = format!("{}:{conflict_group_uuid}", fact_kind.as_str());
        let fact_ids =
            indices.iter().map(|index| facts_with_scope[*index].0.fact_id).collect::<Vec<_>>();
        for index in indices {
            facts_with_scope[index].0.conflict_group_id = Some(conflict_group_id.clone());
        }
        conflicts.push(TechnicalFactConflict {
            conflict_group_id,
            fact_kind,
            normalized_values: distinct_values.into_iter().collect(),
            fact_ids,
        });
    }

    conflicts
}

fn build_candidate(
    block: &StructuredBlockData,
    fact_kind: TechnicalFactKind,
    raw_value: &str,
    qualifiers: Vec<TechnicalFactQualifier>,
    anchor_line: &str,
    extraction_suffix: &str,
) -> Option<FactCandidate> {
    let normalized_value = normalize_technical_fact_value(fact_kind, raw_value)?;
    let qualifiers = normalize_qualifiers(qualifiers);
    Some(FactCandidate {
        fact_kind,
        normalized_value,
        display_value: raw_value.trim().to_string(),
        qualifiers: qualifiers.clone(),
        support_block_ids: BTreeSet::from([block.block_id]),
        confidence: confidence_for_extraction(block, extraction_suffix),
        extraction_kind: format!("{}_{}", extraction_kind_prefix(block), extraction_suffix),
        scope_signature: candidate_scope_signature(
            block,
            fact_kind,
            &qualifiers,
            anchor_line,
            raw_value,
        ),
        rank: candidate_rank(block),
    })
}

fn candidate_scope_signature(
    block: &StructuredBlockData,
    fact_kind: TechnicalFactKind,
    qualifiers: &[TechnicalFactQualifier],
    anchor_line: &str,
    normalized_value: &str,
) -> String {
    let ancestry = if block.section_path.is_empty() {
        block.heading_trail.join(" > ")
    } else {
        block.section_path.join(" > ")
    };
    let anchor = normalize_scope_anchor(anchor_line, fact_kind, normalized_value);
    format!(
        "{}|{}|{}|{}|{}",
        block.block_kind.as_str(),
        ancestry.trim().to_ascii_lowercase(),
        block.code_language.as_deref().unwrap_or_default().to_ascii_lowercase(),
        qualifier_signature(qualifiers),
        anchor,
    )
}

fn normalize_scope_anchor(
    anchor_line: &str,
    fact_kind: TechnicalFactKind,
    normalized_value: &str,
) -> String {
    let mut anchor = collapse_literal_whitespace(anchor_line).to_ascii_lowercase();
    let placeholder = match fact_kind {
        TechnicalFactKind::Url => "<url>",
        TechnicalFactKind::EndpointPath => "<path>",
        TechnicalFactKind::HttpMethod => "<method>",
        TechnicalFactKind::Port => "<port>",
        TechnicalFactKind::ParameterName => "<parameter>",
        TechnicalFactKind::StatusCode => "<status>",
        TechnicalFactKind::Protocol => "<protocol>",
        TechnicalFactKind::AuthRule => "<auth>",
        TechnicalFactKind::Identifier => "<identifier>",
        TechnicalFactKind::EnvironmentVariable => "<envvar>",
        TechnicalFactKind::VersionNumber => "<version>",
        TechnicalFactKind::DatabaseName => "<database>",
        TechnicalFactKind::ConfigurationKey => "<configkey>",
        TechnicalFactKind::ErrorCode => "<errorcode>",
        TechnicalFactKind::RateLimit => "<ratelimit>",
        TechnicalFactKind::DependencyDeclaration => "<dependency>",
        TechnicalFactKind::CodeIdentifier => "<codeident>",
    };
    let raw_lower = normalized_value.to_ascii_lowercase();
    if !raw_lower.is_empty() {
        anchor = anchor.replace(&raw_lower, placeholder);
    }
    anchor
}

fn normalize_qualifiers(
    mut qualifiers: Vec<TechnicalFactQualifier>,
) -> Vec<TechnicalFactQualifier> {
    qualifiers
        .sort_by(|left, right| left.key.cmp(&right.key).then_with(|| left.value.cmp(&right.value)));
    qualifiers.dedup_by(|left, right| left.key == right.key && left.value == right.value);
    qualifiers
}

fn qualifier_signature(qualifiers: &[TechnicalFactQualifier]) -> String {
    qualifiers
        .iter()
        .map(|qualifier| format!("{}={}", qualifier.key, qualifier.value))
        .collect::<Vec<_>>()
        .join("|")
}

const fn extraction_kind_prefix(block: &StructuredBlockData) -> &'static str {
    match block.block_kind {
        StructuredBlockKind::EndpointBlock => "parser_endpoint_block",
        StructuredBlockKind::CodeBlock => "parser_code_block",
        StructuredBlockKind::Table | StructuredBlockKind::TableRow => "parser_table_block",
        StructuredBlockKind::ListItem => "parser_list_block",
        StructuredBlockKind::MetadataBlock
        | StructuredBlockKind::SourceProfile
        | StructuredBlockKind::SourceUnit => "parser_metadata_block",
        _ => "parser_text_block",
    }
}

const fn candidate_rank(block: &StructuredBlockData) -> u8 {
    match block.block_kind {
        StructuredBlockKind::EndpointBlock => 6,
        StructuredBlockKind::CodeBlock => 5,
        StructuredBlockKind::Table | StructuredBlockKind::TableRow => 4,
        StructuredBlockKind::MetadataBlock
        | StructuredBlockKind::SourceProfile
        | StructuredBlockKind::SourceUnit => 3,
        StructuredBlockKind::ListItem => 2,
        _ => 1,
    }
}

/// Confidence derived from BOTH the block kind AND the extraction
/// method. AST-parsed or structurally-parsed facts get higher
/// confidence than keyword-heuristic guesses regardless of block kind.
fn confidence_for_extraction(block: &StructuredBlockData, extraction_suffix: &str) -> f64 {
    // Tier 1: structural parse — the parser proved the value exists.
    if matches!(
        extraction_suffix,
        "ast_node" | "parsed_json_key" | "literal_url" | "version_number"
    ) {
        return match block.block_kind {
            StructuredBlockKind::EndpointBlock => 0.99,
            StructuredBlockKind::CodeBlock => 0.98,
            StructuredBlockKind::Table | StructuredBlockKind::TableRow => 0.97,
            _ => 0.96,
        };
    }
    // Tier 2: block kind default (heuristic match).
    match block.block_kind {
        StructuredBlockKind::EndpointBlock => 0.95,
        StructuredBlockKind::CodeBlock => 0.94,
        StructuredBlockKind::Table | StructuredBlockKind::TableRow => 0.93,
        StructuredBlockKind::MetadataBlock | StructuredBlockKind::SourceUnit => 0.91,
        StructuredBlockKind::ListItem => 0.90,
        _ => 0.88,
    }
}

#[cfg(test)]
mod tests;
