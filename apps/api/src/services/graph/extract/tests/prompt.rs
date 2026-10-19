use super::*;

#[test]
fn prompt_mentions_json_contract_and_chunk_text() {
    let prompt = build_graph_extraction_prompt(&sample_request());

    assert!(prompt.contains("strict JSON"));
    assert!(prompt.contains("entities"));
    assert!(prompt.contains("annual report graph"));
    assert!(prompt.contains("Chunk kind"));
    assert!(prompt.contains("technical_facts"));
    assert!(prompt.contains("lowercase ASCII snake_case"));
    assert!(prompt.contains("Choose a concise `relation_type`"));
    assert!(!prompt.contains("catalog"));
    assert!(!prompt.contains("relation=uses"));
    assert!(!prompt.contains("`topic`, or `document`"));
}

#[test]
fn downgraded_prompt_plan_reduces_segment_count_and_marks_shape() {
    let mut request = oversized_request();
    request.resume_hint = Some(GraphExtractionResumeHint { replay_count: 4, downgrade_level: 1 });

    let plan = build_graph_extraction_prompt_plan(
        &request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        256 * 1024,
    );

    assert!(plan.request_shape_key.contains("downgrade_1"));
    assert!(plan.request_size_bytes < 256 * 1024);
    assert!(plan.prompt.contains("Adaptive downgrade level: 1"));
}

#[test]
fn response_format_constrains_relation_type_by_shape_and_length() {
    let response_format = graph_extraction_response_format();
    let relation_type_schema = response_format
        .get("json_schema")
        .and_then(|value| value.get("schema"))
        .and_then(|value| value.get("properties"))
        .and_then(|value| value.get("relations"))
        .and_then(|value| value.get("items"))
        .and_then(|value| value.get("properties"))
        .and_then(|value| value.get("relation_type"))
        .expect("relation_type schema");

    assert!(relation_type_schema.get("enum").is_none());
    assert_eq!(
        relation_type_schema.get("pattern").and_then(serde_json::Value::as_str),
        Some(crate::services::graph::identity::RELATION_TYPE_JSON_SCHEMA_PATTERN)
    );
    assert_eq!(
        relation_type_schema.get("maxLength").and_then(serde_json::Value::as_u64),
        Some(crate::services::graph::identity::RELATION_TYPE_MAX_LENGTH as u64)
    );
}

fn hints_for(entries: &[(&str, &str)]) -> GraphExtractionSubTypeHints {
    let mut by_node_type: Vec<GraphExtractionSubTypeHintGroup> = Vec::new();
    for (node_type, sub_type) in entries {
        match by_node_type.last_mut() {
            Some(group) if group.node_type == *node_type => {
                group
                    .entries
                    .push(GraphExtractionSubTypeHintEntry { sub_type: (*sub_type).to_string() });
            }
            _ => by_node_type.push(GraphExtractionSubTypeHintGroup {
                node_type: (*node_type).to_string(),
                entries: vec![GraphExtractionSubTypeHintEntry {
                    sub_type: (*sub_type).to_string(),
                }],
            }),
        }
    }
    GraphExtractionSubTypeHints { by_node_type }
}

fn prompt_for(request: &GraphExtractionRequest) -> String {
    build_graph_extraction_prompt_plan(
        request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        256 * 1024,
    )
    .prompt
}

#[test]
fn extraction_prompt_carries_no_chunk_ordinal_and_no_hint_counts() {
    let mut request = sample_request();
    request.chunk.ordinal = 987_654_321;
    request.sub_type_hints = hints_for(&[("artifact", "service"), ("artifact", "database")]);

    let prompt = prompt_for(&request);

    assert!(!prompt.contains("Chunk ordinal"), "ordinal must not enter the extraction prompt");
    assert!(
        !prompt.contains("987654321"),
        "the removed ordinal value must not leak through another field"
    );
    assert!(prompt.contains("- artifact: database, service"), "hints render as a sorted set");
    assert!(
        !prompt.contains("service (") && !prompt.contains("database ("),
        "hint occurrence counts must not be rendered"
    );
}

#[test]
fn extraction_contract_hash_ignores_ordinal_and_hint_counts() {
    let binding = sample_runtime_binding();
    let request = sample_request();

    let mut same_text_other_revision = request.clone();
    same_text_other_revision.chunk.ordinal = 431;
    same_text_other_revision.revision_id = Some(uuid::Uuid::now_v7());
    same_text_other_revision.activated_by_attempt_id = Some(uuid::Uuid::now_v7());
    same_text_other_revision.document.id = uuid::Uuid::now_v7();
    same_text_other_revision.sub_type_hints =
        hints_for(&[("artifact", "service"), ("process", "workflow"), ("concept", "paradigm")]);

    let first = build_graph_extraction_cache_fingerprint(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::Off,
        256 * 1024,
    );
    let second = build_graph_extraction_cache_fingerprint(
        &same_text_other_revision,
        &binding,
        GraphEntityTypeClassifierIdentity::Off,
        256 * 1024,
    );

    assert_eq!(
        first.extraction_contract_hash, second.extraction_contract_hash,
        "a different revision ordinal or hint set must not move the cache identity"
    );
    assert_eq!(first.extraction_version, second.extraction_version);
}

#[test]
fn extraction_contract_hash_tracks_provider_model_and_template() {
    let binding = sample_runtime_binding();
    let request = sample_request();
    let sections = crate::services::graph::extract::prompt::graph_extraction_static_sections();
    let response_format =
        crate::services::graph::extract::prompt::graph_extraction_response_format();
    let base = graph_extraction_contract_hash(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::Off,
        256 * 1024,
    );

    assert_eq!(
        base,
        graph_extraction_contract_hash_with(
            crate::services::graph::extract::prompt::GRAPH_EXTRACTION_CONTRACT_VERSION,
            &sections,
            &response_format,
            &graph_extraction_stable_context(&request, 256 * 1024),
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
        )
    );

    let mut different_model = binding.clone();
    different_model.model_name = "alpha-chat-large".to_string();
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &request,
            &different_model,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        )
    );

    let mut different_provider = binding.clone();
    different_provider.provider_kind = "provider-beta".to_string();
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &request,
            &different_provider,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        )
    );

    let mut different_params = binding.clone();
    different_params.temperature = Some(0.9);
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &request,
            &different_params,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        )
    );

    let context = graph_extraction_stable_context(&request, 256 * 1024);
    assert_ne!(
        base,
        graph_extraction_contract_hash_with(
            "graph_extract_contract_other",
            &sections,
            &response_format,
            &context,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
        ),
        "a contract version bump must invalidate the cache"
    );

    let mut changed_sections = sections.clone();
    changed_sections[0].1.push_str("\nExtra contract clause.");
    assert_ne!(
        base,
        graph_extraction_contract_hash_with(
            crate::services::graph::extract::prompt::GRAPH_EXTRACTION_CONTRACT_VERSION,
            &changed_sections,
            &response_format,
            &context,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
        ),
        "a template change must invalidate the cache"
    );

    let mut changed_format = response_format.clone();
    changed_format["json_schema"]["name"] = serde_json::json!("graph_extraction_v2");
    assert_ne!(
        base,
        graph_extraction_contract_hash_with(
            crate::services::graph::extract::prompt::GRAPH_EXTRACTION_CONTRACT_VERSION,
            &sections,
            &changed_format,
            &context,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
        ),
        "an output schema change must invalidate the cache"
    );
}

#[test]
fn extraction_contract_hash_tracks_library_prompt_and_stable_request_shape() {
    let binding = sample_runtime_binding();
    let request = sample_request();
    let base = graph_extraction_contract_hash(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::Off,
        256 * 1024,
    );

    // Identical inputs hash identically.
    assert_eq!(
        base,
        graph_extraction_contract_hash(
            &request,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        )
    );

    let mut library_prompt = request.clone();
    library_prompt.library_extraction_prompt = Some("Prefer document-local terms.".to_string());
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &library_prompt,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        ),
        "the library extraction prompt digest must move the cache identity"
    );

    let mut blank_library_prompt = request.clone();
    blank_library_prompt.library_extraction_prompt = Some("   ".to_string());
    assert_eq!(
        base,
        graph_extraction_contract_hash(
            &blank_library_prompt,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        ),
        "a blank library prompt is not rendered, so it must not move the identity"
    );

    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &request,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            64 * 1024
        ),
        "the segmentation size limit must move the cache identity"
    );

    let mut renamed = request.clone();
    renamed.document.title = Some("Renamed Spec".to_string());
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &renamed,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        ),
        "the document label rendered in the prompt must move the cache identity"
    );

    let mut moved_section = request.clone();
    moved_section.structured_chunk.section_path =
        vec!["REST API".to_string(), "Errors".to_string()];
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &moved_section,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        ),
        "a different rendered section path must move the cache identity"
    );

    let mut different_facts = request.clone();
    different_facts.technical_facts.push(GraphExtractionTechnicalFact {
        fact_kind: "endpoint_path".to_string(),
        normalized_value: "/other".to_string(),
        display_value: "/other".to_string(),
        qualifiers: Vec::new(),
    });
    assert_ne!(
        base,
        graph_extraction_contract_hash(
            &different_facts,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        ),
        "a different set of technical facts must move the cache identity"
    );

    // The same facts in a different input order are the same contract.
    let mut reordered_facts = request.clone();
    reordered_facts.technical_facts.reverse();
    assert_eq!(
        base,
        graph_extraction_contract_hash(
            &reordered_facts,
            &binding,
            GraphEntityTypeClassifierIdentity::Off,
            256 * 1024
        ),
        "technical fact order must not move the identity"
    );
}

fn classifier_binding(
    model_name: &str,
    provider_kind: &str,
    base_url: Option<&str>,
) -> ResolvedRuntimeBinding {
    let mut binding = sample_runtime_binding();
    binding.provider_kind = provider_kind.to_string();
    binding.provider_base_url = base_url.map(str::to_string);
    binding.provider_api_style = "typesafe_system_one".to_string();
    binding.model_name = model_name.to_string();
    binding
}

#[test]
fn extraction_contract_hash_tracks_the_entity_type_classifier() {
    let binding = sample_runtime_binding();
    let request = sample_request();
    let classifier = classifier_binding("jev-latest", "typesafe", Some("http://127.0.0.1:9"));

    let off = graph_extraction_contract_hash(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::Off,
        256 * 1024,
    );
    let on = graph_extraction_contract_hash(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::On(&classifier),
        256 * 1024,
    );

    assert_ne!(
        off, on,
        "the explicit off marker and a resolved classifier must not share a cache identity"
    );
    assert_eq!(
        on,
        graph_extraction_contract_hash(
            &request,
            &binding,
            GraphEntityTypeClassifierIdentity::On(&classifier),
            256 * 1024,
        ),
        "the classifier identity is deterministic"
    );

    let mut other_model = classifier.clone();
    other_model.model_name = "jev-mini".to_string();
    assert_ne!(
        on,
        graph_extraction_contract_hash(
            &request,
            &binding,
            GraphEntityTypeClassifierIdentity::On(&other_model),
            256 * 1024,
        ),
        "a classifier model change must invalidate the cache"
    );

    let mut other_provider = classifier.clone();
    other_provider.provider_kind = "typesafe-alt".to_string();
    assert_ne!(
        on,
        graph_extraction_contract_hash(
            &request,
            &binding,
            GraphEntityTypeClassifierIdentity::On(&other_provider),
            256 * 1024,
        ),
        "a classifier provider change must invalidate the cache"
    );

    let mut other_endpoint = classifier.clone();
    other_endpoint.provider_base_url = Some("http://127.0.0.1:8".to_string());
    assert_ne!(
        on,
        graph_extraction_contract_hash(
            &request,
            &binding,
            GraphEntityTypeClassifierIdentity::On(&other_endpoint),
            256 * 1024,
        ),
        "a classifier endpoint change must invalidate the cache"
    );

    let mut different_extract_binding = binding.clone();
    different_extract_binding.model_name = "alpha-chat-large".to_string();
    assert_ne!(
        on,
        graph_extraction_contract_hash(
            &request,
            &different_extract_binding,
            GraphEntityTypeClassifierIdentity::On(&classifier),
            256 * 1024,
        ),
        "a change to the extract binding alone must still move the identity"
    );

    let off_fingerprint = build_graph_extraction_cache_fingerprint(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::Off,
        256 * 1024,
    );
    let on_fingerprint = build_graph_extraction_cache_fingerprint(
        &request,
        &binding,
        GraphEntityTypeClassifierIdentity::On(&classifier),
        256 * 1024,
    );
    assert_ne!(off_fingerprint.extraction_contract_hash, on_fingerprint.extraction_contract_hash);
    assert_eq!(off_fingerprint.extraction_version, on_fingerprint.extraction_version);
}

#[test]
fn prompt_hash_still_tracks_the_full_rendered_prompt_for_audit() {
    let binding = sample_runtime_binding();
    let request = sample_request();
    let base_prompt = prompt_for(&request);
    let base_hash = graph_extraction_prompt_hash(&base_prompt, &binding);

    let mut same_contract = request.clone();
    same_contract.revision_id = Some(uuid::Uuid::now_v7());
    same_contract.activated_by_attempt_id = Some(uuid::Uuid::now_v7());
    same_contract.document.id = uuid::Uuid::now_v7();
    same_contract.document.active_revision_id = Some(uuid::Uuid::now_v7());
    let mut same_binding = binding.clone();
    same_binding.binding_id = uuid::Uuid::now_v7();
    same_binding.provider_catalog_id = uuid::Uuid::now_v7();
    same_binding.model_catalog_id = uuid::Uuid::now_v7();

    assert_eq!(base_hash, graph_extraction_prompt_hash(&prompt_for(&same_contract), &same_binding));

    let mut different_prompt = request.clone();
    different_prompt.library_extraction_prompt = Some("Prefer document-local terms.".to_string());
    assert_ne!(base_hash, graph_extraction_prompt_hash(&prompt_for(&different_prompt), &binding));

    let mut different_hints = request.clone();
    different_hints.sub_type_hints = hints_for(&[("artifact", "service")]);
    assert_ne!(
        base_hash,
        graph_extraction_prompt_hash(&prompt_for(&different_hints), &binding),
        "the audit hash records the prompt that actually ran, hints included"
    );

    let mut different_model = binding.clone();
    different_model.model_name = "alpha-chat-large".to_string();
    assert_ne!(base_hash, graph_extraction_prompt_hash(&base_prompt, &different_model));
}

#[test]
fn prompt_preview_is_deterministic_for_large_chunks() {
    let request = oversized_request();
    let (first_prompt, first_shape, first_size) =
        build_graph_extraction_prompt_preview(&request, 8 * 1024);
    let (second_prompt, second_shape, second_size) =
        build_graph_extraction_prompt_preview(&request, 8 * 1024);

    assert_eq!(first_prompt, second_prompt);
    assert_eq!(first_shape, second_shape);
    assert_eq!(first_size, second_size);
    assert!(first_prompt.contains("[chunk_segment_1]"));
    assert!(first_shape.contains("segments_3"));
    assert!(first_size <= 8 * 1024 + GRAPH_EXTRACTION_REQUEST_OVERHEAD_BYTES);
}

#[test]
fn entity_type_section_renders_the_canonical_reference_byte_for_byte() {
    // The reference moved from an inline literal into `type_reference` (IRG-184);
    // the rendered section must stay byte-identical so the extraction prompt,
    // and with it every cached extraction identity, does not move.
    let expected = [
        "Entity type reference (choose the single best type for each entity):",
        "- person: A named individual human being (Linus Torvalds, Marie Curie, Warren Buffett, Hippocrates)",
        "- organization: A company, institution, government body, team, or standards body (Google, WHO, SEC, IETF, Supreme Court, Red Cross)",
        "- location: A named geographic place, region, facility, or site (Silicon Valley, Wall Street, Chernobyl, Amazon rainforest)",
        "- event: A named occurrence, incident, milestone, or time-bounded happening (COVID-19 pandemic, Log4Shell, 2008 financial crisis, Roe v. Wade, Apollo 11)",
        "- artifact: Anything created, built, or designed by humans — software, tools, products, drugs, devices, standards, protocols, laws, licenses, code functions, APIs, documents (PostgreSQL, Aspirin, TCP/IP, GDPR, MIT License, build_router(), GET /api/users, Basel III, React, insulin pump)",
        "- natural: Anything existing in nature without human creation — species, organisms, diseases, genes, proteins, elements, minerals, natural phenomena (SARS-CoV-2, BRCA1 gene, malaria, silicon, photosynthesis, earthquake, DNA)",
        "- process: A named procedure, method, algorithm, workflow, or repeatable sequence of steps (Agile methodology, PCR testing, IPO process, judicial review, gradient descent, fermentation)",
        "- concept: An abstract idea, theory, principle, pattern, paradigm, theme, or field of study (dependency injection, herd immunity, supply and demand, due process, machine learning, oncology, relativity)",
        "- attribute: A named measurable property, metric, indicator, parameter, status, threshold, or configuration value (p99 latency, blood pressure, GDP, APP_PORT, credit score, HTTP 200, melting point, pH level)",
        "- entity: Catch-all for named things that do not fit any other type. Always prefer a more specific type above.",
    ]
    .join("\n");
    let sections = graph_extraction_static_sections();
    let (_, section) = sections
        .iter()
        .find(|(name, _)| name == "entity_types")
        .expect("the extraction prompt carries the entity type reference");
    assert_eq!(section, &expected);
}

#[test]
fn classifier_identity_hash_tracks_the_classifier_request_contract() {
    use sha2::{Digest as _, Sha256};
    let classifier = classifier_binding("jev-latest", "typesafe", Some("http://127.0.0.1:9"));
    let digest = |identity: GraphEntityTypeClassifierIdentity<'_>, contract: &[String]| {
        let mut hasher = Sha256::new();
        update_hash_classifier_identity(&mut hasher, identity, contract);
        hasher.finalize()
    };
    let contract = vec!["prompt".to_string(), "person".to_string(), "a human".to_string()];
    let changed = vec!["prompt".to_string(), "person".to_string(), "an individual".to_string()];

    assert_ne!(
        digest(GraphEntityTypeClassifierIdentity::On(&classifier), &contract),
        digest(GraphEntityTypeClassifierIdentity::On(&classifier), &changed),
        "a changed classifier request must invalidate node types cached under the old one"
    );
    assert_eq!(
        digest(GraphEntityTypeClassifierIdentity::Off, &contract),
        digest(GraphEntityTypeClassifierIdentity::Off, &changed),
        "with typing off no classifier request is sent, so its contract is not identity"
    );
}
