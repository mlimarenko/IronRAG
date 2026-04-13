use super::types::{
    GraphExtractionPromptPlan, GraphExtractionPromptVariant, GraphExtractionRequest,
    GraphExtractionStructuredChunkContext, GraphExtractionSubTypeHints,
    GraphExtractionTechnicalFact,
};

pub(crate) const GRAPH_EXTRACTION_VERSION: &str = "graph_extract_v6";
pub(crate) const GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS: usize = 2;
pub(crate) const GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES: usize = 8 * 1024;
pub(crate) const GRAPH_EXTRACTION_MAX_SEGMENTS: usize = 3;
pub(crate) const GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL: usize = 2;

pub(crate) fn normalized_downgrade_level(request: &GraphExtractionRequest) -> usize {
    request
        .resume_hint
        .as_ref()
        .map(|hint| hint.downgrade_level.min(GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL))
        .unwrap_or(0)
}

pub(crate) fn downgraded_request_size_soft_limit_bytes(
    base_limit: usize,
    downgrade_level: usize,
) -> usize {
    match downgrade_level.min(GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL) {
        0 => base_limit,
        1 => (base_limit / 2).max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES + 1024),
        _ => (base_limit / 3).max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES + 1024),
    }
}

pub(crate) fn downgraded_max_segments(downgrade_level: usize) -> usize {
    match downgrade_level.min(GRAPH_EXTRACTION_MAX_DOWNGRADE_LEVEL) {
        0 => GRAPH_EXTRACTION_MAX_SEGMENTS,
        1 => 2,
        _ => 1,
    }
}

#[cfg(test)]
#[must_use]
pub fn build_graph_extraction_prompt(request: &GraphExtractionRequest) -> String {
    build_graph_extraction_prompt_plan(
        request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        usize::MAX,
    )
    .prompt
}

#[cfg(test)]
#[must_use]
pub(crate) fn build_graph_extraction_prompt_preview(
    request: &GraphExtractionRequest,
    request_size_soft_limit_bytes: usize,
) -> (String, String, usize) {
    let plan = build_graph_extraction_prompt_plan(
        request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        request_size_soft_limit_bytes,
    );
    (plan.prompt, plan.request_shape_key, plan.request_size_bytes)
}

pub(crate) fn build_graph_extraction_prompt_plan(
    request: &GraphExtractionRequest,
    variant: GraphExtractionPromptVariant,
    trigger_reason: Option<&str>,
    issue_summary: Option<&str>,
    previous_output: Option<&str>,
    request_size_soft_limit_bytes: usize,
) -> GraphExtractionPromptPlan {
    let downgrade_level = normalized_downgrade_level(request);
    let document_label = request
        .document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&request.document.external_key);
    let safe_limit = downgraded_request_size_soft_limit_bytes(
        request_size_soft_limit_bytes.max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES + 1024),
        downgrade_level,
    );
    let mut sections: Vec<(String, String)> = Vec::new();
    sections.push((
        "task".to_string(),
        "You are a knowledge graph extraction expert. Your job is to extract structured entities and relationships from a document chunk to build a rich, queryable knowledge graph.\n\n\
Extract ALL meaningful entities: named things (people, organizations, artifacts, natural phenomena), typed concepts (algorithms, patterns, paradigms), processes (methods, workflows), and measurable attributes (metrics, parameters, configuration values) that appear in the text.\n\n\
For each entity, determine the single best type from the entity type reference below.\n\n\
Extract ALL relationships between entities. Use the most specific relation type from the catalog. NEVER use \"mentions\" when a more specific type exists. For example:\n\
- \"X uses Y\" → uses (not mentions)\n\
- \"X depends on Y\" → depends_on (not mentions)\n\
- \"X is built with Y\" → uses or builds_on (not mentions)\n\
- \"X authenticates via Y\" → authenticates (not mentions)\n\
- \"X returns Y\" → returns (not mentions)\n\
- \"X implements Y\" → implements (not mentions)\n\
- \"X contains Y\" → contains (not mentions)\n\
- \"X is a type of Y\" → is_a (not mentions)\n\
Only use \"mentions\" for truly tangential references where the text names something without describing any functional relationship.\n\n\
Resolve coreferences: when the text says \"it\", \"this system\", \"the API\", \"the framework\", or uses abbreviations, resolve them to the full canonical entity name. Do not extract pronouns or abbreviations as separate entities.".to_string(),
    ));
    sections.push((
        "entity_types".to_string(),
        "Entity type reference (choose the single best type for each entity):\n\
- person: A named individual human being (Linus Torvalds, Marie Curie, Warren Buffett, Hippocrates)\n\
- organization: A company, institution, government body, team, or standards body (Google, WHO, SEC, IETF, Supreme Court, Red Cross)\n\
- location: A named geographic place, region, facility, or site (Silicon Valley, Wall Street, Chernobyl, Amazon rainforest)\n\
- event: A named occurrence, incident, milestone, or time-bounded happening (COVID-19 pandemic, Log4Shell, 2008 financial crisis, Roe v. Wade, Apollo 11)\n\
- artifact: Anything created, built, or designed by humans — software, tools, products, drugs, devices, standards, protocols, laws, licenses, code functions, APIs, documents (PostgreSQL, Aspirin, TCP/IP, GDPR, MIT License, build_router(), GET /api/users, Basel III, React, insulin pump)\n\
- natural: Anything existing in nature without human creation — species, organisms, diseases, genes, proteins, elements, minerals, natural phenomena (SARS-CoV-2, BRCA1 gene, malaria, silicon, photosynthesis, earthquake, DNA)\n\
- process: A named procedure, method, algorithm, workflow, or repeatable sequence of steps (Agile methodology, PCR testing, IPO process, judicial review, gradient descent, fermentation)\n\
- concept: An abstract idea, theory, principle, pattern, paradigm, theme, or field of study (dependency injection, herd immunity, supply and demand, due process, machine learning, oncology, relativity)\n\
- attribute: A named measurable property, metric, indicator, parameter, status, threshold, or configuration value (p99 latency, blood pressure, GDP, APP_PORT, credit score, HTTP 200, melting point, pH level)\n\
- entity: Catch-all for named things that do not fit any other type. Always prefer a more specific type above.".to_string(),
    ));
    sections.push((
        "examples".to_string(),
        "Example 1 - API documentation chunk:\n\
Input: \"FastAPI uses Pydantic for data validation. When you declare a parameter with a type annotation, FastAPI automatically validates the input and returns a 422 status code if validation fails.\"\n\
Output: {\"entities\":[{\"label\":\"FastAPI\",\"node_type\":\"artifact\",\"sub_type\":\"framework\",\"aliases\":[],\"summary\":\"Python web framework with automatic data validation\"},{\"label\":\"Pydantic\",\"node_type\":\"artifact\",\"sub_type\":\"library\",\"aliases\":[],\"summary\":\"Data validation library used by FastAPI\"},{\"label\":\"422\",\"node_type\":\"attribute\",\"sub_type\":\"http_status_code\",\"aliases\":[\"422 Unprocessable Entity\"],\"summary\":\"HTTP status code returned when validation fails\"}],\"relations\":[{\"source_label\":\"FastAPI\",\"target_label\":\"Pydantic\",\"relation_type\":\"uses\",\"summary\":\"FastAPI uses Pydantic for data validation\"},{\"source_label\":\"FastAPI\",\"target_label\":\"422\",\"relation_type\":\"returns\",\"summary\":\"Returns 422 when input validation fails\"}]}\n\n\
Example 2 - Infrastructure chunk:\n\
Input: \"The auth-service runs on port 8001 and depends on PostgreSQL for session storage. It authenticates users via JWT tokens signed with RS256.\"\n\
Output: {\"entities\":[{\"label\":\"auth-service\",\"node_type\":\"artifact\",\"sub_type\":\"microservice\",\"aliases\":[],\"summary\":\"Authentication service on port 8001\"},{\"label\":\"PostgreSQL\",\"node_type\":\"artifact\",\"sub_type\":\"database\",\"aliases\":[\"Postgres\"],\"summary\":\"Database used for session storage\"},{\"label\":\"JWT\",\"node_type\":\"artifact\",\"sub_type\":\"token_format\",\"aliases\":[\"JSON Web Token\"],\"summary\":\"Token format for authentication\"},{\"label\":\"RS256\",\"node_type\":\"artifact\",\"sub_type\":\"signing_algorithm\",\"aliases\":[],\"summary\":\"RSA-SHA256 signing algorithm\"}],\"relations\":[{\"source_label\":\"auth-service\",\"target_label\":\"PostgreSQL\",\"relation_type\":\"depends_on\",\"summary\":\"Uses PostgreSQL for session storage\"},{\"source_label\":\"auth-service\",\"target_label\":\"JWT\",\"relation_type\":\"authenticates\",\"summary\":\"Authenticates users via JWT tokens\"},{\"source_label\":\"JWT\",\"target_label\":\"RS256\",\"relation_type\":\"uses\",\"summary\":\"Tokens signed with RS256 algorithm\"}]}".to_string(),
    ));
    sections.push((
        "schema".to_string(),
        format!(
            "Return strict JSON with keys `entities` and `relations`. Each entity must include `label`, `node_type` (one of: `person`, `organization`, `location`, `event`, `artifact`, `natural`, `process`, `concept`, `attribute`, `entity`), `aliases`, `summary`, and optionally `sub_type` (a freeform specialization within the type, e.g. framework, database, algorithm, enzyme, microservice, protocol). Each relation must include `source_label`, `target_label`, `relation_type`, and `summary`. `relation_type` must be copied verbatim from this catalog: {}. Use lowercase ASCII snake_case only. Never translate, localize, paraphrase, or invent a new relation_type. If no concise summary is available, emit an empty string. If none fit exactly, omit the relation.",
            crate::services::graph::identity::canonical_relation_type_catalog().join(", ")
        ),
    ));
    sections.push((
        "rules".to_string(),
        "Do not include markdown fences or prose. If no grounded graph evidence exists, return {\"entities\":[],\"relations\":[]}.\n\
Critical rules:\n\
1. ALWAYS provide a non-empty summary for every entity and relation.\n\
2. NEVER use 'mentions' when any specific relation type fits. Audit each relation: could it be uses, depends_on, contains, implements, returns, configures, extends, calls, or another specific type?\n\
3. Extract the entity's PRIMARY role or purpose in the summary, not just its name.\n\
4. When the text describes a capability, feature, or behavior, model it as a relation (enables, provides, supports) not just a mention.".to_string(),
    ));
    sections.push((
        "document".to_string(),
        format!("Document: {document_label}\nChunk ordinal: {}", request.chunk.ordinal),
    ));
    {
        let section_path_text = if request.structured_chunk.section_path.is_empty() {
            String::new()
        } else {
            format!("\nSection: {}", request.structured_chunk.section_path.join(" > "))
        };
        sections.push((
            "domain_context".to_string(),
            format!("Document domain: {document_label}{section_path_text}"),
        ));
    }
    if let Some(library_prompt) = request.library_extraction_prompt.as_deref() {
        let trimmed = library_prompt.trim();
        if !trimmed.is_empty() {
            sections.push(("library_context".to_string(), trimmed.to_string()));
        }
    }
    if let Some(rendered) = render_sub_type_hints(&request.sub_type_hints) {
        sections.push(("sub_type_hints".to_string(), rendered));
    }
    sections.push((
        "structured_chunk".to_string(),
        render_structured_chunk_context(&request.structured_chunk),
    ));
    if let Some(technical_fact_section) =
        render_graph_extraction_technical_facts(&request.technical_facts, safe_limit / 5)
    {
        sections.push(("technical_facts".to_string(), technical_fact_section));
    }

    if downgrade_level > 0 {
        sections.push((
            "downgrade".to_string(),
            format!(
                "Adaptive downgrade level: {downgrade_level}\nReason: repeated recoverable extraction replay on this chunk."
            ),
        ));
    }

    if variant != GraphExtractionPromptVariant::Initial {
        sections.push((
            "recovery".to_string(),
            format!(
                "Recovery variant: {}\nTrigger: {}\nIssue: {}",
                match variant {
                    GraphExtractionPromptVariant::Initial => "initial",
                    GraphExtractionPromptVariant::ProviderRetry => "provider_retry",
                    GraphExtractionPromptVariant::SecondPass => "second_pass",
                },
                trigger_reason.unwrap_or("unknown"),
                issue_summary.unwrap_or("unspecified"),
            ),
        ));
    }

    if let Some(previous_output) = previous_output {
        sections.push((
            "previous_output".to_string(),
            format!("Previous extraction output:\n{previous_output}"),
        ));
    }

    let reserved_bytes = sections
        .iter()
        .map(|(title, body)| title.len().saturating_add(body.len()).saturating_add(8))
        .sum::<usize>();
    let chunk_text_budget =
        safe_limit.saturating_sub(reserved_bytes).max(GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES / 4);
    let chunk_segments = segment_chunk_text_for_prompt(
        &request.chunk.content,
        chunk_text_budget,
        downgraded_max_segments(downgrade_level),
    );
    for (index, segment) in chunk_segments.iter().enumerate() {
        sections.push((format!("chunk_segment_{}", index + 1), segment.clone()));
    }

    let prompt = sections
        .iter()
        .map(|(title, body)| format!("[{title}]\n{body}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let request_size_bytes = prompt.len();
    let request_shape_key = format!(
        "graph_extract_v6:{}:segments_{}:downgrade_{}:{}",
        match variant {
            GraphExtractionPromptVariant::Initial => "initial",
            GraphExtractionPromptVariant::ProviderRetry => "provider_retry",
            GraphExtractionPromptVariant::SecondPass => "second_pass",
        },
        chunk_segments.len(),
        downgrade_level,
        if request_size_bytes > request_size_soft_limit_bytes { "trimmed" } else { "full" }
    );

    GraphExtractionPromptPlan { prompt, request_shape_key, request_size_bytes }
}

fn segment_chunk_text_for_prompt(
    content: &str,
    max_total_bytes: usize,
    max_segments: usize,
) -> Vec<String> {
    if content.is_empty() {
        return vec!["Prepared chunk text:".to_string()];
    }

    if content.len() <= max_total_bytes {
        return vec![format!("Prepared chunk text:\n{content}")];
    }

    let segment_count = max_segments.max(1);
    let segment_budget = (max_total_bytes / segment_count).max(256);
    let chars = content.chars().collect::<Vec<_>>();
    let total_chars = chars.len();
    let approx_chars_per_segment = segment_budget / 4;
    let edge_chars = approx_chars_per_segment.min(total_chars);
    let head = chars[..edge_chars].iter().collect::<String>();
    if segment_count == 1 {
        return vec![format!("Prepared chunk text segment 1/1:\n{head}")];
    }

    if segment_count == 2 {
        let tail = chars[total_chars.saturating_sub(edge_chars)..].iter().collect::<String>();
        return vec![
            "Prepared chunk text segment 1/2:\n".to_string() + &head,
            "Prepared chunk text segment 2/2:\n".to_string() + &tail,
        ];
    }

    let middle_start = total_chars.saturating_sub(approx_chars_per_segment) / 2;
    let middle_end = (middle_start + approx_chars_per_segment).min(total_chars);
    let middle = chars[middle_start..middle_end].iter().collect::<String>();
    let tail = chars[total_chars.saturating_sub(edge_chars)..].iter().collect::<String>();

    vec![
        format!("Prepared chunk text segment 1/{segment_count}:\n{head}"),
        format!("Prepared chunk text segment 2/{segment_count}:\n{middle}"),
        format!("Prepared chunk text segment 3/{segment_count}:\n{tail}"),
    ]
}

fn render_sub_type_hints(hints: &GraphExtractionSubTypeHints) -> Option<String> {
    if hints.is_empty() {
        return None;
    }
    let mut lines = Vec::with_capacity(hints.by_node_type.len() + 2);
    lines.push(
        "Observed sub_types in this library (prefer one of these if it fits an extracted entity; \
         create a new sub_type only if none of these match):"
            .to_string(),
    );
    for group in &hints.by_node_type {
        if group.entries.is_empty() {
            continue;
        }
        let entries = group
            .entries
            .iter()
            .map(|entry| format!("{} ({})", entry.sub_type, entry.occurrences))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- {}: {}", group.node_type, entries));
    }
    if lines.len() <= 1 {
        return None;
    }
    Some(lines.join("\n"))
}

fn render_structured_chunk_context(context: &GraphExtractionStructuredChunkContext) -> String {
    let mut lines = Vec::new();
    lines.push(format!("Chunk kind: {}", context.chunk_kind.as_deref().unwrap_or("unknown")));
    if !context.section_path.is_empty() {
        lines.push(format!("Section path: {}", context.section_path.join(" > ")));
    }
    if !context.heading_trail.is_empty() {
        lines.push(format!("Heading trail: {}", context.heading_trail.join(" > ")));
    }
    if !context.support_block_ids.is_empty() {
        lines.push(format!("Support block count: {}", context.support_block_ids.len()));
    }
    if let Some(literal_digest) = &context.literal_digest {
        lines.push(format!("Literal digest: {literal_digest}"));
    }
    lines.join("\n")
}

fn render_graph_extraction_technical_facts(
    facts: &[GraphExtractionTechnicalFact],
    max_bytes: usize,
) -> Option<String> {
    if facts.is_empty() {
        return None;
    }

    let mut rendered = String::new();
    for fact in facts {
        let qualifiers = if fact.qualifiers.is_empty() {
            String::new()
        } else {
            format!(
                " | qualifiers: {}",
                fact.qualifiers
                    .iter()
                    .map(|qualifier| format!("{}={}", qualifier.key, qualifier.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let line = format!(
            "- {}: {} | display: {}{}",
            fact.fact_kind, fact.canonical_value, fact.display_value, qualifiers
        );
        let next_len = rendered.len().saturating_add(line.len()).saturating_add(1);
        if !rendered.is_empty() && next_len > max_bytes.max(256) {
            break;
        }
        if !rendered.is_empty() {
            rendered.push('\n');
        }
        rendered.push_str(&line);
    }

    (!rendered.is_empty()).then_some(rendered)
}

#[cfg(test)]
pub(crate) fn graph_extraction_response_format(provider_kind: &str) -> serde_json::Value {
    if provider_kind == "deepseek" {
        return serde_json::json!({
            "type": "json_object"
        });
    }

    serde_json::json!({
        "type": "json_schema",
        "json_schema": {
            "name": "graph_extraction",
            "strict": true,
            "schema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "entities": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "label": { "type": "string" },
                                "node_type": {
                                    "type": "string",
                                    "enum": ["person", "organization", "location", "event", "artifact", "natural_kind", "process", "concept", "topic", "metric", "regulation", "code_symbol", "entity"]
                                },
                                "aliases": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                },
                                "summary": { "type": "string" }
                            },
                            "required": ["label", "node_type", "aliases", "summary"]
                        }
                    },
                    "relations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "source_label": { "type": "string" },
                                "target_label": { "type": "string" },
                                "relation_type": {
                                    "type": "string",
                                    "enum": crate::services::graph::identity::canonical_relation_type_catalog()
                                },
                                "summary": { "type": "string" }
                            },
                            "required": ["source_label", "target_label", "relation_type", "summary"]
                        }
                    }
                },
                "required": ["entities", "relations"]
            }
        }
    })
}
