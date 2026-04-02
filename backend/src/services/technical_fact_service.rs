use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use uuid::Uuid;

use crate::{
    domains::knowledge::TypedTechnicalFact,
    shared::{
        structured_document::{StructuredBlockData, StructuredBlockKind},
        technical_facts::{
            TechnicalFactConflict, TechnicalFactKind, TechnicalFactQualifier, TechnicalFactValue,
            collapse_literal_whitespace, normalize_technical_fact_value,
        },
    },
};

const TECHNICAL_FACT_NAMESPACE: Uuid = Uuid::from_u128(0x8c79_60e4_40fd_4ad8_b5d3_4d93_d93d_4021);
const TECHNICAL_CONFLICT_NAMESPACE: Uuid =
    Uuid::from_u128(0x5a73_4a11_83fd_4f3b_abcd_c03f_f8f8_b9f0);
const HTTP_METHODS: [&str; 8] =
    ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "CONNECT"];
const PROTOCOLS: [&str; 8] = ["http", "https", "tcp", "udp", "ws", "wss", "grpc", "soap"];

#[derive(Debug, Clone)]
pub struct ExtractTechnicalFactsCommand {
    pub revision_id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub blocks: Vec<StructuredBlockData>,
}

#[derive(Debug, Clone)]
pub struct ExtractTechnicalFactsResult {
    pub facts: Vec<TypedTechnicalFact>,
    pub conflicts: Vec<TechnicalFactConflict>,
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
                candidates.extend(extract_url_candidates(block, line));
                candidates.extend(extract_endpoint_candidates(block, line));
                candidates.extend(extract_port_candidates(block, line));
                candidates.extend(extract_status_code_candidates(block, line));
                candidates.extend(extract_protocol_candidates(block, line));
                candidates.extend(extract_parameter_candidates(block, line));
                candidates.extend(extract_auth_rule_candidates(block, line));
                candidates.extend(extract_catalog_link_identifier_candidates(block, line));
                candidates.extend(extract_branded_identifier_candidates(block, line));
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
}

fn preferred_block_text(block: &StructuredBlockData) -> String {
    if block.normalized_text.trim().is_empty() {
        block.text.clone()
    } else {
        block.normalized_text.clone()
    }
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
            let _ = ports.insert(port_literal);
        }
    }

    for window in tokens.windows(2) {
        if is_port_keyword(&window[0])
            && let Some(port_literal) = extract_port_literal(&window[1])
        {
            let _ = ports.insert(port_literal);
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
                let _ = ports.insert(port_literal);
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
            let _ = protocols.insert(scheme.to_string());
        }
        let normalized = trim_technical_token(&token).to_ascii_lowercase();
        if PROTOCOLS.iter().any(|protocol| protocol == &normalized) {
            let _ = protocols.insert(normalized);
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
                let _ = parameters.insert(key);
            }
        }
    }

    let cells = table_cells(line);
    if cells.len() >= 2
        && let Some(parameter_name) = leading_identifier(&cells[0])
        && is_parameter_name_like(&parameter_name)
    {
        let _ = parameters.insert(parameter_name);
    }

    let cleaned = strip_leading_marker(&collapse_literal_whitespace(line));
    for separator in [":", "="] {
        if let Some((left, _right)) = cleaned.split_once(separator)
            && let Some(parameter_name) = leading_identifier(left)
            && is_parameter_name_like(&parameter_name)
        {
            let _ = parameters.insert(parameter_name);
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

fn extract_catalog_link_identifier_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(block.block_kind, StructuredBlockKind::ListItem) {
        return Vec::new();
    }

    let Some(brand_prefix) = infer_catalog_brand_prefix(block) else {
        return Vec::new();
    };

    extract_markdown_link_labels(line)
        .into_iter()
        .filter_map(|label| normalize_catalog_link_label(&label))
        .filter_map(|label| {
            let display = if label
                .split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(&brand_prefix))
            {
                label
            } else {
                format!("{brand_prefix} {label}")
            };
            build_candidate(
                block,
                TechnicalFactKind::Identifier,
                &display,
                Vec::new(),
                line,
                "catalog_link_identifier",
            )
        })
        .collect()
}

fn extract_branded_identifier_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(
        block.block_kind,
        StructuredBlockKind::Heading | StructuredBlockKind::MetadataBlock
    ) {
        return Vec::new();
    }

    let mut identifiers = BTreeSet::<String>::new();
    if let Some(identifier) = extract_namespace_style_identifier(line) {
        let _ = identifiers.insert(identifier);
    }
    if let Some(identifier) = extract_branded_phrase_identifier(line) {
        let _ = identifiers.insert(identifier);
    }

    identifiers
        .into_iter()
        .filter_map(|identifier| {
            build_candidate(
                block,
                TechnicalFactKind::Identifier,
                &identifier,
                Vec::new(),
                line,
                "branded_identifier",
            )
        })
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
        confidence: confidence_for_block(block),
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

fn confidence_for_block(block: &StructuredBlockData) -> f64 {
    match block.block_kind {
        StructuredBlockKind::EndpointBlock => 0.97,
        StructuredBlockKind::CodeBlock => 0.96,
        StructuredBlockKind::Table | StructuredBlockKind::TableRow => 0.95,
        StructuredBlockKind::MetadataBlock => 0.93,
        StructuredBlockKind::ListItem => 0.92,
        _ => 0.90,
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

fn extract_url_like_token(token: &str) -> Option<String> {
    let trimmed = trim_technical_token(token);
    (trimmed.starts_with("http://") || trimmed.starts_with("https://")).then(|| trimmed.to_string())
}

fn extract_url_path(url: &str) -> Option<String> {
    let (_, remainder) = url.split_once("://")?;
    let path_start = remainder.find('/')?;
    let path_with_query = &remainder[path_start..];
    let path = path_with_query.split(['?', '#']).next().unwrap_or_default().trim();
    extract_path_like_token(path)
}

fn extract_query_parameter_keys(url: &str) -> Vec<String> {
    let Some((_, query_with_rest)) = url.split_once('?') else {
        return Vec::new();
    };
    let query = query_with_rest.split('#').next().unwrap_or_default();
    query
        .split('&')
        .filter_map(|pair| pair.split_once('=').map(|(left, _)| left).or(Some(pair)))
        .map(trim_technical_token)
        .filter(|token| is_parameter_name_like(token))
        .map(str::to_string)
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

fn infer_catalog_brand_prefix(block: &StructuredBlockData) -> Option<String> {
    let words = block
        .heading_trail
        .iter()
        .flat_map(|heading| heading.split(|character: char| !character.is_ascii_alphanumeric()))
        .filter(|word| !word.is_empty())
        .filter(|word| looks_like_brand_context_word(word))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if words.is_empty() {
        return None;
    }

    let mut counts = BTreeMap::<String, usize>::new();
    for word in &words {
        *counts.entry(word.clone()).or_default() += 1;
    }
    let max_count = counts.values().copied().max().unwrap_or(0);
    words.into_iter().find(|word| counts.get(word).copied().unwrap_or(0) == max_count)
}

fn extract_markdown_link_labels(line: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut remainder = line;
    while let Some(start) = remainder.find('[') {
        let after_start = &remainder[start + 1..];
        let Some(end) = after_start.find(']') else {
            break;
        };
        let label = after_start[..end].trim();
        let after_label = &after_start[end + 1..];
        if after_label.starts_with('(') && !label.is_empty() {
            labels.push(label.to_string());
        }
        remainder = after_label;
    }
    labels
}

fn normalize_catalog_link_label(label: &str) -> Option<String> {
    let collapsed = collapse_literal_whitespace(label);
    let normalized = collapsed
        .split_whitespace()
        .map(trim_technical_token)
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.is_empty() || normalized.len() > 64 || is_generic_ascii_heading(&normalized) {
        return None;
    }
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() || words.len() > 4 {
        return None;
    }
    words
        .iter()
        .all(|word| {
            word.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '+' | '.')
            }) && (is_ascii_titlecase_word(word)
                || is_ascii_uppercase_acronym(word)
                || has_ascii_camel_case(word))
        })
        .then_some(normalized)
}

fn extract_namespace_style_identifier(line: &str) -> Option<String> {
    let cleaned = strip_leading_marker(&collapse_literal_whitespace(line));
    let primary = split_primary_phrase(&cleaned);
    let (left, right) = primary.split_once(':')?;
    let left = trim_technical_token(left);
    let right = trim_technical_token(right);
    if branded_identifier_part(left) && branded_identifier_part(right) {
        Some(format!("{left}:{right}"))
    } else {
        None
    }
}

fn extract_branded_phrase_identifier(line: &str) -> Option<String> {
    let cleaned = strip_leading_marker(&collapse_literal_whitespace(line));
    let primary = split_primary_phrase(&cleaned);
    if primary.is_empty()
        || is_generic_ascii_heading(primary)
        || !looks_like_branded_product_phrase(primary)
    {
        return None;
    }
    Some(primary.to_string())
}

fn split_primary_phrase(value: &str) -> &str {
    [" - ", " — ", " – "]
        .into_iter()
        .find_map(|separator| value.split_once(separator).map(|(left, _)| left.trim()))
        .unwrap_or(value)
        .trim()
}

fn looks_like_branded_product_phrase(candidate: &str) -> bool {
    let words = candidate
        .split_whitespace()
        .map(trim_technical_token)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.is_empty() || words.len() > 6 {
        return false;
    }
    if !words.iter().all(|word| {
        word.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, ':' | '_' | '-' | '+' | '.')
        })
    }) {
        return false;
    }
    words.iter().filter(|word| looks_like_brand_context_word(word)).count() >= 1
        && words.iter().any(|word| {
            is_ascii_titlecase_word(word)
                || is_ascii_uppercase_acronym(word)
                || has_ascii_camel_case(word)
        })
}

fn branded_identifier_part(candidate: &str) -> bool {
    is_ascii_titlecase_word(candidate)
        || is_ascii_uppercase_acronym(candidate)
        || has_ascii_camel_case(candidate)
}

fn is_ascii_titlecase_word(word: &str) -> bool {
    let compact = trim_technical_token(word);
    let mut characters = compact.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && characters.all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
}

fn is_ascii_uppercase_acronym(word: &str) -> bool {
    let compact = trim_technical_token(word);
    compact.len() >= 2
        && compact.len() <= 8
        && compact.chars().any(|character| character.is_ascii_uppercase())
        && compact
            .chars()
            .all(|character| character.is_ascii_uppercase() || character.is_ascii_digit())
}

fn has_ascii_camel_case(word: &str) -> bool {
    word.chars()
        .zip(word.chars().skip(1))
        .any(|(left, right)| left.is_ascii_lowercase() && right.is_ascii_uppercase())
}

fn looks_like_brand_context_word(word: &str) -> bool {
    let lower = word.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "available"
            | "applications"
            | "catalog"
            | "catalogue"
            | "description"
            | "documentation"
            | "example"
            | "guide"
            | "manual"
            | "module"
            | "modules"
            | "overview"
            | "product"
            | "products"
            | "program"
            | "programs"
            | "service"
            | "services"
            | "solution"
            | "solutions"
            | "system"
            | "systems"
    ) {
        return false;
    }
    is_ascii_titlecase_word(word) || has_ascii_camel_case(word)
}

fn is_generic_ascii_heading(candidate: &str) -> bool {
    matches!(
        candidate.trim().to_ascii_lowercase().as_str(),
        "description"
            | "overview"
            | "how to"
            | "history"
            | "licensing"
            | "user manual"
            | "administrator guide"
            | "service engineers"
            | "administrators"
            | "example"
    )
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
        && (has_ascii_camel_case(compact)
            || compact.contains('_')
            || compact.contains('-')
            || is_ascii_titlecase_word(compact))
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
    use crate::shared::structured_document::{StructuredBlockData, StructuredBlockKind};
    use uuid::Uuid;

    #[test]
    fn extracts_branded_identifiers_from_catalog_link_list_items() {
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
                },
            ],
        });

        let identifiers = result
            .facts
            .iter()
            .filter(|fact| fact.fact_kind.as_str() == "identifier")
            .map(|fact| fact.display_value.as_str())
            .collect::<Vec<_>>();

        assert!(identifiers.contains(&"Acme Control Center"));
        assert!(identifiers.contains(&"Acme POS"));
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
}
