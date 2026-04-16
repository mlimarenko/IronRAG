mod identifiers;

use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    domains::knowledge::TypedTechnicalFact,
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

const TECHNICAL_FACT_NAMESPACE: Uuid = Uuid::from_u128(0x8c79_60e4_40fd_4ad8_b5d3_4d93_d93d_4021);
const TECHNICAL_CONFLICT_NAMESPACE: Uuid =
    Uuid::from_u128(0x5a73_4a11_83fd_4f3b_abcd_c03f_f8f8_b9f0);
const HTTP_METHODS: [&str; 8] =
    ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT"];
const PROTOCOLS: [&str; 8] = ["http", "https", "tcp", "udp", "ws", "wss", "grpc", "soap"];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractTechnicalFactsCommand {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub blocks: Vec<StructuredBlockData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractTechnicalFactsResult {
    pub facts: Vec<TypedTechnicalFact>,
    pub conflicts: Vec<TechnicalFactConflict>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TechnicalFactExtractionFailure {
    pub code: String,
    pub summary: String,
}

#[derive(Debug, Clone)]
struct FactCandidate {
    fact_kind: TechnicalFactKind,
    canonical_value: TechnicalFactValue,
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
    canonical_value: TechnicalFactValue,
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

    #[must_use]
    pub fn extract_from_blocks(
        &self,
        command: ExtractTechnicalFactsCommand,
    ) -> ExtractTechnicalFactsResult {
        let mut candidates = Vec::new();
        for block in &command.blocks {
            let block_text = preferred_block_text(block);
            let lines = logical_lines(&block_text);
            for line in &lines {
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
                    StructuredBlockKind::MetadataBlock => {
                        // Metadata: config keys, env vars, branded identifiers.
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

        let mut facts_with_scope = finalize_candidates(&command, candidates);
        let conflicts = assign_conflicts(&mut facts_with_scope);
        let mut facts =
            facts_with_scope.into_iter().map(|(fact, _scope_signature)| fact).collect::<Vec<_>>();
        facts.sort_by(|left, right| {
            left.fact_kind
                .as_str()
                .cmp(right.fact_kind.as_str())
                .then_with(|| left.display_value.cmp(&right.display_value))
                .then_with(|| left.fact_id.cmp(&right.fact_id))
        });

        ExtractTechnicalFactsResult { facts, conflicts }
    }

    pub fn extract_runtime_stage(
        &self,
        command: ExtractTechnicalFactsCommand,
    ) -> Result<ExtractTechnicalFactsResult, TechnicalFactExtractionFailure> {
        if command.blocks.is_empty() {
            return Err(TechnicalFactExtractionFailure {
                code: TechnicalFactExtractionFailureCode::EmptyBlocks.as_str().to_string(),
                summary: "technical fact extraction requires at least one structured block"
                    .to_string(),
            });
        }

        Ok(self.extract_from_blocks(command))
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
        .filter(|segment| {
            !segment.split_once(": ").is_some_and(|(key, value)| {
                normalize_table_row_key(key) == "index"
                    && value.trim().chars().all(|character| character.is_ascii_digit())
            })
        })
        .collect::<Vec<_>>();
    if !table_row_has_technical_signal(&segments) {
        return String::new();
    }
    segments.join(" | ")
}

fn table_row_has_technical_signal(segments: &[&str]) -> bool {
    let mut strong_key_hits = 0usize;
    for segment in segments {
        let Some((key, value)) = segment.split_once(": ") else {
            continue;
        };
        let normalized_key = normalize_table_row_key(key);
        if is_strong_technical_table_key(&normalized_key) {
            strong_key_hits += 1;
        }
        let _ = value;
    }
    strong_key_hits > 0
}

fn normalize_table_row_key(key: &str) -> String {
    key.to_ascii_lowercase()
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_strong_technical_table_key(normalized_key: &str) -> bool {
    normalized_key.contains("method")
        || normalized_key.contains("endpoint")
        || normalized_key == "path"
        || normalized_key.ends_with(" path")
        || normalized_key.contains("route")
        || normalized_key.contains("status")
        || normalized_key.contains("port")
        || normalized_key.contains("parameter")
        || normalized_key.contains("query")
        || normalized_key.contains("header")
        || normalized_key.contains("auth")
        || normalized_key.contains("token")
        || normalized_key.contains("request")
        || normalized_key.contains("response")
        || normalized_key.contains("payload")
        || normalized_key.contains("env")
        || normalized_key.contains("variable")
        || normalized_key.contains("config")
}

fn logical_lines(block_text: &str) -> Vec<String> {
    block_text
        .lines()
        .map(collapse_literal_whitespace)
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn extract_url_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    technical_tokens(line)
        .into_iter()
        .filter_map(|token| {
            extract_url_like_token(&token).and_then(|url| {
                build_candidate(
                    block,
                    TechnicalFactKind::Url,
                    &url,
                    Vec::new(),
                    line,
                    "literal_url",
                )
            })
        })
        .collect()
}

fn extract_endpoint_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    let descriptors = line_descriptors(line);
    let methods = descriptors
        .iter()
        .filter_map(|descriptor| normalize_http_method(&descriptor.value))
        .collect::<Vec<_>>();
    let mut facts = methods
        .iter()
        .filter_map(|method| {
            build_candidate(
                block,
                TechnicalFactKind::HttpMethod,
                method,
                Vec::new(),
                line,
                "endpoint_method",
            )
        })
        .collect::<Vec<_>>();

    let method_qualifiers = if methods.len() == 1 {
        vec![TechnicalFactQualifier { key: "method".to_string(), value: methods[0].to_string() }]
    } else {
        Vec::new()
    };

    let path_descriptors = descriptors
        .iter()
        .filter_map(|descriptor| {
            extract_path_like_token(&descriptor.value)
                .or_else(|| {
                    extract_url_like_token(&descriptor.value).and_then(|url| extract_url_path(&url))
                })
                .map(|path| (descriptor.value.as_str(), path))
        })
        .collect::<Vec<_>>();

    for (_, path) in path_descriptors {
        if let Some(candidate) = build_candidate(
            block,
            TechnicalFactKind::EndpointPath,
            &path,
            method_qualifiers.clone(),
            line,
            "endpoint_path",
        ) {
            facts.push(candidate);
        }
    }

    facts
}

fn extract_port_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    let tokens = technical_tokens(line);
    let mut ports = BTreeSet::<String>::new();

    for token in &tokens {
        if let Some(url) = extract_url_like_token(token)
            && let Some(port_literal) = extract_port_from_url(&url)
        {
            ports.insert(port_literal);
        }
    }

    for window in tokens.windows(2) {
        if is_port_keyword(&window[0])
            && let Some(port_literal) = extract_port_literal(&window[1])
        {
            ports.insert(port_literal);
        }
    }

    let cleaned = strip_leading_marker(&collapse_literal_whitespace(line));
    for separator in [":", "="] {
        if let Some((left, right)) = cleaned.split_once(separator) {
            let key = trim_technical_token(left);
            let value = trim_technical_token(right);
            if is_port_keyword(key)
                && let Some(port_literal) = extract_port_literal(value)
            {
                ports.insert(port_literal);
            }
        }
    }

    ports
        .into_iter()
        .filter_map(|port| {
            build_candidate(block, TechnicalFactKind::Port, &port, Vec::new(), line, "network_port")
        })
        .collect()
}

fn extract_status_code_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    let lower = line.to_ascii_lowercase();
    if !matches_any_substring(&lower, &["status", "response", "http", "код", "статус"]) {
        return Vec::new();
    }

    line_descriptors(line)
        .into_iter()
        .filter_map(|descriptor| {
            let candidate = trim_technical_token(&descriptor.value);
            if candidate.len() != 3
                || !candidate.chars().all(|character| character.is_ascii_digit())
            {
                return None;
            }
            let parsed = candidate.parse::<u16>().ok()?;
            ((100..=599).contains(&parsed)).then_some(candidate.to_string())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|status| {
            build_candidate(
                block,
                TechnicalFactKind::StatusCode,
                &status,
                Vec::new(),
                line,
                "http_status",
            )
        })
        .collect()
}

fn extract_protocol_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    let mut protocols = BTreeSet::<String>::new();
    for token in technical_tokens(line) {
        if let Some(url) = extract_url_like_token(&token)
            && let Some((scheme, _)) = url.split_once("://")
            && PROTOCOLS.iter().any(|protocol| protocol == &scheme)
        {
            protocols.insert(scheme.to_string());
        }
        let normalized = trim_technical_token(&token).to_ascii_lowercase();
        if PROTOCOLS.iter().any(|protocol| protocol == &normalized) {
            protocols.insert(normalized);
        }
    }

    protocols
        .into_iter()
        .filter_map(|protocol| {
            build_candidate(
                block,
                TechnicalFactKind::Protocol,
                &protocol,
                Vec::new(),
                line,
                "protocol",
            )
        })
        .collect()
}

fn extract_parameter_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    let mut parameters = BTreeSet::<String>::new();

    for token in technical_tokens(line) {
        if let Some(url) = extract_url_like_token(&token) {
            for key in extract_query_parameter_keys(&url) {
                parameters.insert(key);
            }
        }
    }

    let cells = table_cells(line);
    if cells.len() >= 2
        && let Some(parameter_name) = leading_identifier(&cells[0])
        && is_parameter_name_like(&parameter_name)
    {
        parameters.insert(parameter_name);
    }

    let cleaned = strip_leading_marker(&collapse_literal_whitespace(line));
    for separator in [":", "="] {
        if let Some((left, _right)) = cleaned.split_once(separator)
            && let Some(parameter_name) = leading_identifier(left)
            && is_parameter_name_like(&parameter_name)
        {
            parameters.insert(parameter_name);
        }
    }

    parameters
        .into_iter()
        .filter_map(|parameter_name| {
            build_candidate(
                block,
                TechnicalFactKind::ParameterName,
                &parameter_name,
                Vec::new(),
                line,
                "parameter_name",
            )
        })
        .collect()
}

fn extract_auth_rule_candidates(block: &StructuredBlockData, line: &str) -> Vec<FactCandidate> {
    let lower = line.to_ascii_lowercase();
    let literal = if matches_any_substring(&lower, &["bearer token", "authorization: bearer"]) {
        Some("bearer_token")
    } else if matches_any_substring(&lower, &["basic auth", "authorization: basic"]) {
        Some("basic_auth")
    } else if lower.contains("oauth") {
        Some("oauth")
    } else {
        None
    };

    literal
        .and_then(|value| {
            build_candidate(
                block,
                TechnicalFactKind::AuthRule,
                value,
                Vec::new(),
                line,
                "auth_rule",
            )
        })
        .into_iter()
        .collect()
}

fn finalize_candidates(
    command: &ExtractTechnicalFactsCommand,
    candidates: Vec<FactCandidate>,
) -> Vec<(TypedTechnicalFact, String)> {
    let mut aggregates =
        BTreeMap::<(TechnicalFactKind, String, String, String), FactAggregate>::new();

    for candidate in candidates {
        let canonical_string = candidate.canonical_value.canonical_string();
        let qualifier_key = qualifier_signature(&candidate.qualifiers);
        let scope_signature = candidate.scope_signature.clone();
        let key = (candidate.fact_kind, canonical_string, qualifier_key, scope_signature.clone());
        let aggregate = aggregates.entry(key).or_insert_with(|| FactAggregate {
            fact_kind: candidate.fact_kind,
            canonical_value: candidate.canonical_value.clone(),
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
            let canonical_string = aggregate.canonical_value.canonical_string();
            let fact_id = Uuid::new_v5(
                &TECHNICAL_FACT_NAMESPACE,
                format!(
                    "{}:{}:{}:{}",
                    command.revision_id,
                    aggregate.fact_kind.as_str(),
                    canonical_string,
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
                    canonical_value: aggregate.canonical_value,
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
            .map(|index| facts_with_scope[*index].0.canonical_value.canonical_string())
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
            canonical_values: distinct_values.into_iter().collect(),
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
    let canonical_value = normalize_technical_fact_value(fact_kind, raw_value)?;
    let qualifiers = canonicalize_qualifiers(qualifiers);
    Some(FactCandidate {
        fact_kind,
        canonical_value,
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
    canonical_value: &str,
) -> String {
    let ancestry = if block.section_path.is_empty() {
        block.heading_trail.join(" > ")
    } else {
        block.section_path.join(" > ")
    };
    let anchor = normalize_scope_anchor(anchor_line, fact_kind, canonical_value);
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
    canonical_value: &str,
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
    let raw_lower = canonical_value.to_ascii_lowercase();
    if !raw_lower.is_empty() {
        anchor = anchor.replace(&raw_lower, placeholder);
    }
    anchor
}

fn canonicalize_qualifiers(
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

fn extraction_kind_prefix(block: &StructuredBlockData) -> &'static str {
    match block.block_kind {
        StructuredBlockKind::EndpointBlock => "parser_endpoint_block",
        StructuredBlockKind::CodeBlock => "parser_code_block",
        StructuredBlockKind::Table | StructuredBlockKind::TableRow => "parser_table_block",
        StructuredBlockKind::ListItem => "parser_list_block",
        StructuredBlockKind::MetadataBlock => "parser_metadata_block",
        _ => "parser_text_block",
    }
}

fn candidate_rank(block: &StructuredBlockData) -> u8 {
    match block.block_kind {
        StructuredBlockKind::EndpointBlock => 6,
        StructuredBlockKind::CodeBlock => 5,
        StructuredBlockKind::Table | StructuredBlockKind::TableRow => 4,
        StructuredBlockKind::MetadataBlock => 3,
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
        StructuredBlockKind::MetadataBlock => 0.91,
        StructuredBlockKind::ListItem => 0.90,
        _ => 0.88,
    }
}

fn technical_tokens(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(trim_technical_token)
        .map(str::to_string)
        .filter(|token| !token.is_empty())
        .collect()
}

#[derive(Debug, Clone)]
struct LineDescriptor {
    value: String,
}

fn line_descriptors(line: &str) -> Vec<LineDescriptor> {
    let mut descriptors = technical_tokens(line)
        .into_iter()
        .map(|value| LineDescriptor { value })
        .collect::<Vec<_>>();

    for cell in table_cells(line) {
        if !descriptors.iter().any(|descriptor| descriptor.value == cell) {
            descriptors.push(LineDescriptor { value: cell });
        }
    }

    descriptors
}

fn table_cells(line: &str) -> Vec<String> {
    if !line.contains('|') {
        return Vec::new();
    }
    line.split('|')
        .map(collapse_literal_whitespace)
        .map(|cell| strip_leading_marker(&cell))
        .filter(|cell| !cell.is_empty())
        .collect()
}

/// Validates a token as a well-formed URL using the `url` crate
/// (RFC 3986). Replaces the old `starts_with("http://")` heuristic
/// that couldn't distinguish a real URL from a comment that happens
/// to contain "http://" and missed edge cases like trailing parens,
/// angle brackets, and percent-encoded paths.
fn extract_url_like_token(token: &str) -> Option<String> {
    let trimmed = trim_technical_token(token);
    let parsed = url::Url::parse(trimmed).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| trimmed.to_string())
}

/// Extracts the path component from a URL via RFC-compliant parsing.
fn extract_url_path(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    let path = parsed.path();
    if path == "/" || path.is_empty() {
        return None;
    }
    extract_path_like_token(path)
}

/// Extracts query parameter keys from a URL via the `url` crate's
/// `query_pairs()` iterator. Replaces manual `split('&')` +
/// `split('=')` which silently broke on percent-encoded keys.
fn extract_query_parameter_keys(raw: &str) -> Vec<String> {
    let Ok(parsed) = url::Url::parse(raw) else {
        return Vec::new();
    };
    parsed
        .query_pairs()
        .map(|(key, _)| key.to_string())
        .filter(|key| is_parameter_name_like(key))
        .collect()
}

fn extract_path_like_token(token: &str) -> Option<String> {
    let trimmed = trim_technical_token(token);
    if !trimmed.starts_with('/') || trimmed.len() < 2 {
        return None;
    }
    let path = trimmed.split(['?', '#']).next().unwrap_or_default();
    path.chars()
        .all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '/' | '_' | '-' | '.' | '{' | '}')
        })
        .then(|| collapse_literal_whitespace(path).replace(' ', ""))
}

fn extract_port_from_url(url: &str) -> Option<String> {
    let (_, remainder) = url.split_once("://")?;
    let authority = remainder.split('/').next().unwrap_or_default();
    let (_, port) = authority.rsplit_once(':')?;
    extract_port_literal(port)
}

fn extract_port_literal(value: &str) -> Option<String> {
    let digits = value.chars().filter(char::is_ascii_digit).collect::<String>();
    let parsed = digits.parse::<u16>().ok()?;
    ((1..=65535).contains(&parsed)).then_some(parsed.to_string())
}

fn normalize_http_method(value: &str) -> Option<&'static str> {
    let upper = trim_technical_token(value).to_ascii_uppercase();
    HTTP_METHODS.into_iter().find(|method| method == &upper)
}

fn leading_identifier(value: &str) -> Option<String> {
    let trimmed = strip_leading_marker(value);
    let candidate = trim_technical_token(trimmed.split_whitespace().next().unwrap_or_default());
    is_parameter_name_like(candidate).then(|| candidate.to_string())
}

fn is_parameter_name_like(candidate: &str) -> bool {
    let compact = trim_technical_token(candidate);
    !compact.is_empty()
        && compact.len() <= 64
        && compact.chars().any(|character| character.is_ascii_alphabetic())
        && compact.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        && (compact.contains('_')
            || compact.contains('-')
            || compact
                .chars()
                .zip(compact.chars().skip(1))
                .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase())
            || (compact.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && compact.chars().skip(1).all(|c| c.is_ascii_lowercase())))
}

fn is_port_keyword(value: &str) -> bool {
    matches!(
        trim_technical_token(value).to_ascii_lowercase().as_str(),
        "port" | "ports" | "tcp_port" | "udp_port"
    )
}

fn matches_any_substring(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn strip_leading_marker(value: &str) -> String {
    value
        .trim_start_matches(|character: char| {
            matches!(character, '-' | '*' | '+' | '#' | '>' | '"' | '\'')
        })
        .trim()
        .to_string()
}

fn trim_technical_token(token: &str) -> &str {
    token.trim_matches(|character: char| {
        matches!(character, ',' | ';' | ':' | ')' | '(' | ']' | '[' | '"' | '\'' | '`' | '{' | '}')
    })
}

#[cfg(test)]
mod tests {
    use super::{ExtractTechnicalFactsCommand, TechnicalFactService};
    use crate::shared::extraction::{
        structured_document::{StructuredBlockData, StructuredBlockKind},
        table_markdown::parse_markdown_table_row,
    };
    use uuid::Uuid;

    // extracts_branded_identifiers_from_catalog_link_list_items — REMOVED.
    // Branded identifier heuristic was the noisiest extractor and is now
    // delegated to the LLM's entity extraction pipeline.

    #[test]
    fn branded_identifier_extraction_no_longer_produces_facts() {
        // Verify the branded heuristic is truly gone — no `Identifier`
        // facts from heading phrases or list item links.
        let service = TechnicalFactService::new();
        let result = service.extract_from_blocks(ExtractTechnicalFactsCommand {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            blocks: vec![
                StructuredBlockData {
                    block_id: Uuid::now_v7(),
                    ordinal: 0,
                    block_kind: StructuredBlockKind::Heading,
                    text: "Программные продукты Acme - Программные продукты Acme - Example"
                        .to_string(),
                    normalized_text:
                        "Программные продукты Acme - Программные продукты Acme - Example"
                            .to_string(),
                    heading_trail: vec![
                        "Программные продукты Acme - Программные продукты Acme - Example"
                            .to_string(),
                    ],
                    section_path: vec!["программные-продукты-acme".to_string()],
                    page_number: None,
                    source_span: None,
                    parent_block_id: None,
                    table_coordinates: None,
                    code_language: None,
                    is_boilerplate: false,
                },
                StructuredBlockData {
                    block_id: Uuid::now_v7(),
                    ordinal: 1,
                    block_kind: StructuredBlockKind::ListItem,
                    text: "- [Control Center](https://docs.example.test/control-center)"
                        .to_string(),
                    normalized_text: "- [Control Center](https://docs.example.test/control-center)"
                        .to_string(),
                    heading_trail: vec![
                        "Программные продукты Acme - Программные продукты Acme - Example"
                            .to_string(),
                    ],
                    section_path: vec!["программные-продукты-acme".to_string()],
                    page_number: None,
                    source_span: None,
                    parent_block_id: None,
                    table_coordinates: None,
                    code_language: None,
                    is_boilerplate: false,
                },
                StructuredBlockData {
                    block_id: Uuid::now_v7(),
                    ordinal: 2,
                    block_kind: StructuredBlockKind::ListItem,
                    text: "- [POS](https://docs.example.test/pos)".to_string(),
                    normalized_text: "- [POS](https://docs.example.test/pos)".to_string(),
                    heading_trail: vec![
                        "Программные продукты Acme - Программные продукты Acme - Example"
                            .to_string(),
                    ],
                    section_path: vec!["программные-продукты-acme".to_string()],
                    page_number: None,
                    source_span: None,
                    parent_block_id: None,
                    table_coordinates: None,
                    code_language: None,
                    is_boilerplate: false,
                },
            ],
        });

        let identifiers = result
            .facts
            .iter()
            .filter(|fact| fact.fact_kind.as_str() == "identifier")
            .map(|fact| fact.display_value.as_str())
            .collect::<Vec<_>>();

        assert!(
            identifiers.is_empty(),
            "branded identifier heuristic should no longer produce facts, got: {:?}",
            identifiers
        );
    }

    #[test]
    fn extracts_endpoint_methods_and_query_parameter_names() {
        let service = TechnicalFactService::new();
        let result = service.extract_from_blocks(ExtractTechnicalFactsCommand {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            blocks: vec![StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::EndpointBlock,
                text: "GET https://api.example.test/orders?pageNumber=1&pageSize=25".to_string(),
                normalized_text: "GET https://api.example.test/orders?pageNumber=1&pageSize=25"
                    .to_string(),
                heading_trail: vec!["Orders".to_string()],
                section_path: vec!["orders".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            }],
        });

        let fact_kinds = result
            .facts
            .iter()
            .map(|fact| (fact.fact_kind.as_str().to_string(), fact.display_value.clone()))
            .collect::<Vec<_>>();

        assert!(fact_kinds.contains(&("http_method".to_string(), "GET".to_string())));
        assert!(fact_kinds.contains(&(
            "url".to_string(),
            "https://api.example.test/orders?pageNumber=1&pageSize=25".to_string()
        )));
        assert!(fact_kinds.contains(&("endpoint_path".to_string(), "/orders".to_string())));
        assert!(fact_kinds.contains(&("parameter_name".to_string(), "pageNumber".to_string())));
        assert!(fact_kinds.contains(&("parameter_name".to_string(), "pageSize".to_string())));
    }

    #[test]
    fn table_rows_skip_business_table_url_noise_for_fact_extraction() {
        let service = TechnicalFactService::new();
        let raw_row = "| 1 | FAB0d41d5b5d22c | Ferrell LLC | https://price.net/ | Papua New Guinea | Plastics |";
        assert_eq!(parse_markdown_table_row(raw_row).len(), 6);
        let result = service.extract_from_blocks(ExtractTechnicalFactsCommand {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            blocks: vec![
                StructuredBlockData {
                    block_id: Uuid::now_v7(),
                    ordinal: 0,
                    block_kind: StructuredBlockKind::Table,
                    text: "| Index | Organization Id | Name | Website | Country | Industry |\n| --- | --- | --- | --- | --- | --- |\n| 1 | FAB0d41d5b5d22c | Ferrell LLC | https://price.net/ | Papua New Guinea | Plastics |".to_string(),
                    normalized_text: "| Index | Organization Id | Name | Website | Country | Industry |\n| --- | --- | --- | --- | --- | --- |\n| 1 | FAB0d41d5b5d22c | Ferrell LLC | https://price.net/ | Papua New Guinea | Plastics |".to_string(),
                    heading_trail: vec!["organizations-100".to_string()],
                    section_path: vec!["organizations_100".to_string()],
                    page_number: None,
                    source_span: None,
                    parent_block_id: None,
                    table_coordinates: None,
                    code_language: None,
                    is_boilerplate: false,
                },
                StructuredBlockData {
                    block_id: Uuid::now_v7(),
                    ordinal: 1,
                    block_kind: StructuredBlockKind::TableRow,
                    text: raw_row.to_string(),
                    normalized_text: "Sheet: organizations-100 | Row 1 | Index: 1 | Organization Id: FAB0d41d5b5d22c | Name: Ferrell LLC | Website: https://price.net/ | Country: Papua New Guinea | Industry: Plastics".to_string(),
                    heading_trail: vec!["organizations-100".to_string()],
                    section_path: vec!["organizations_100".to_string()],
                    page_number: None,
                    source_span: None,
                    parent_block_id: None,
                    table_coordinates: None,
                    code_language: None,
                    is_boilerplate: false,
                },
            ],
        });

        assert!(!result.facts.iter().any(|fact| {
            fact.fact_kind.as_str() == "parameter_name" && fact.display_value == "Sheet"
        }));
        assert!(!result.facts.iter().any(|fact| {
            fact.fact_kind.as_str() == "parameter_name" && fact.display_value == "Index"
        }));
        assert!(result.facts.is_empty());
    }

    #[test]
    fn table_rows_keep_technical_endpoint_facts_when_headers_are_technical() {
        let service = TechnicalFactService::new();
        let result = service.extract_from_blocks(ExtractTechnicalFactsCommand {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            blocks: vec![StructuredBlockData {
                block_id: Uuid::now_v7(),
                ordinal: 0,
                block_kind: StructuredBlockKind::TableRow,
                text: "| GET | /orders | https://api.example.test/orders | 200 |".to_string(),
                normalized_text: "Sheet: API | Row 1 | Method: GET | Endpoint: /orders | Base URL: https://api.example.test/orders | Status Code: 200".to_string(),
                heading_trail: vec!["api".to_string()],
                section_path: vec!["api".to_string()],
                page_number: None,
                source_span: None,
                parent_block_id: None,
                table_coordinates: None,
                code_language: None,
                is_boilerplate: false,
            }],
        });

        let fact_kinds = result
            .facts
            .iter()
            .map(|fact| (fact.fact_kind.as_str().to_string(), fact.display_value.clone()))
            .collect::<Vec<_>>();
        assert!(fact_kinds.contains(&("http_method".to_string(), "GET".to_string())));
        assert!(fact_kinds.contains(&("endpoint_path".to_string(), "/orders".to_string())));
        assert!(
            fact_kinds
                .contains(&("url".to_string(), "https://api.example.test/orders".to_string()))
        );
    }

    fn make_test_block(
        kind: StructuredBlockKind,
        text: &str,
        code_language: Option<&str>,
    ) -> StructuredBlockData {
        StructuredBlockData {
            block_id: Uuid::now_v7(),
            ordinal: 0,
            block_kind: kind,
            text: text.to_string(),
            normalized_text: text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            page_number: None,
            source_span: None,
            parent_block_id: None,
            table_coordinates: None,
            code_language: code_language.map(str::to_string),
            is_boilerplate: false,
        }
    }

    fn extract_facts(
        blocks: Vec<StructuredBlockData>,
    ) -> Vec<crate::domains::knowledge::TypedTechnicalFact> {
        TechnicalFactService::new()
            .extract_from_blocks(ExtractTechnicalFactsCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                blocks,
            })
            .facts
    }

    #[test]
    fn extracts_environment_variables() {
        let facts = extract_facts(vec![make_test_block(
            StructuredBlockKind::Paragraph,
            "Set $DATABASE_URL and process.env.API_KEY before starting",
            None,
        )]);

        let env_vars: Vec<_> =
            facts.iter().filter(|f| f.fact_kind.as_str() == "environment_variable").collect();

        assert!(
            env_vars.len() >= 2,
            "expected at least 2 environment variables, found {}: {:?}",
            env_vars.len(),
            env_vars.iter().map(|f| &f.display_value).collect::<Vec<_>>()
        );
    }

    #[test]
    fn extracts_version_numbers() {
        let facts = extract_facts(vec![make_test_block(
            StructuredBlockKind::Paragraph,
            "Requires version v2.3.1 or later",
            None,
        )]);

        let versions: Vec<_> =
            facts.iter().filter(|f| f.fact_kind.as_str() == "version_number").collect();

        assert!(!versions.is_empty(), "expected at least one VersionNumber fact");
        assert!(
            versions.iter().any(|f| f.display_value.contains("2.3.1")),
            "expected a version containing '2.3.1', got: {:?}",
            versions.iter().map(|f| &f.display_value).collect::<Vec<_>>()
        );
    }

    #[test]
    fn extracts_code_identifiers_from_python_code_blocks() {
        let facts = extract_facts(vec![make_test_block(
            StructuredBlockKind::CodeBlock,
            "def build_router(state):\n    pass\n\nclass AppRouter:\n    pass",
            Some("python"),
        )]);

        let code_idents: Vec<_> =
            facts.iter().filter(|f| f.fact_kind.as_str() == "code_identifier").collect();

        assert!(
            code_idents.iter().any(|f| f.display_value == "build_router"),
            "expected CodeIdentifier for 'build_router', got: {:?}",
            code_idents.iter().map(|f| &f.display_value).collect::<Vec<_>>()
        );
    }

    #[test]
    fn no_code_identifiers_from_unsupported_language() {
        // Rust is not yet supported by tree-sitter in our pipeline.
        // The heuristic fallback was removed — no guessing.
        let facts = extract_facts(vec![make_test_block(
            StructuredBlockKind::CodeBlock,
            "fn build_router(state: AppState) -> Router {",
            Some("rust"),
        )]);

        let code_idents: Vec<_> =
            facts.iter().filter(|f| f.fact_kind.as_str() == "code_identifier").collect();

        assert!(
            code_idents.is_empty(),
            "expected no CodeIdentifier for unsupported language, got: {:?}",
            code_idents.iter().map(|f| &f.display_value).collect::<Vec<_>>()
        );
    }

    #[test]
    fn extracts_config_keys_from_json() {
        let facts = extract_facts(vec![make_test_block(
            StructuredBlockKind::CodeBlock,
            "{\"max_connections\": 200, \"shared_buffers\": \"256MB\"}",
            Some("json"),
        )]);

        let config_keys: Vec<_> =
            facts.iter().filter(|f| f.fact_kind.as_str() == "configuration_key").collect();

        assert!(!config_keys.is_empty(), "expected at least one ConfigurationKey fact, got none");
        let key_names: Vec<_> = config_keys.iter().map(|f| f.display_value.as_str()).collect();
        assert!(
            key_names.contains(&"max_connections"),
            "expected 'max_connections', got: {:?}",
            key_names
        );
    }

    #[test]
    fn extracts_config_keys_from_yaml() {
        // YAML is now parsed structurally via serde_yaml.
        let facts = extract_facts(vec![make_test_block(
            StructuredBlockKind::CodeBlock,
            "max_connections: 200\nshared_buffers: 256MB",
            Some("yaml"),
        )]);

        let config_keys: Vec<_> =
            facts.iter().filter(|f| f.fact_kind.as_str() == "configuration_key").collect();

        assert!(!config_keys.is_empty(), "expected YAML config keys to be extracted, got none");
        let key_names: Vec<_> = config_keys.iter().map(|f| f.display_value.as_str()).collect();
        assert!(
            key_names.contains(&"max_connections"),
            "expected 'max_connections' from YAML, got: {:?}",
            key_names
        );
    }
}
