use crate::domains::query_ir::LiteralSpan;

use super::query_ir_allows_structured_source_unit_inventory;
use super::test_support::*;
use super::*;

#[test]
fn structured_source_unit_field_answer_preserves_late_matching_fields() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.scope = QueryScope::SingleDocument;
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "service.deep.port".to_string() }];
    let wide_unit = format!(
        "[unit_ordinal=0] fields: {}; service.deep.port=8443; service.deep.protocol=https",
        (0..140)
            .map(|index| format!("early_{index:02}=value-{index}"))
            .collect::<Vec<_>>()
            .join("; ")
    );
    let answer = build_structured_source_unit_field_answer(
        "Which service deep port is configured?",
        &query_ir,
        &[source_unit(0, &wide_unit)],
    )
    .expect("structured source-unit answer");

    assert!(answer.contains("service.deep.port=8443"));
    assert!(!answer.contains("early_00=value-0"));
}

#[test]
fn structured_source_unit_field_answer_skips_broad_enumerations() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.scope = QueryScope::SingleDocument;
    query_ir.act = QueryAct::Enumerate;
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "services.api.ports".to_string() }];

    let answer = build_structured_source_unit_field_answer(
        "What services are defined and what ports does each service use?",
        &query_ir,
        &[source_unit(
            0,
            "[unit_ordinal=0] fields: services.alpha.ports.0=8001:8001; services.gamma.image=sample-store:16",
        )],
    );

    assert!(answer.is_none());
}

#[test]
fn structured_source_unit_inventory_answer_lists_service_ports() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Service, QueryTargetKind::Port];
    let mut chunk = source_unit(
        0,
        "[unit_ordinal=0] fields: services.alpha-edge.environment.ALPHA_ADMIN_LISTEN=0.0.0.0:8001; services.beta-worker.expose=3001; services.gamma-store.ports=${GAMMA_STORE_PORT:-5432}:5432",
    );
    chunk.document_label = "sample_manifest.yaml".to_string();

    let answer = build_structured_source_unit_inventory_answer(
        "What services are defined and what ports does each service use?",
        &query_ir,
        &[chunk],
    )
    .expect("service port inventory answer");

    assert!(answer.contains("alpha-edge"));
    assert!(answer.contains("8001"));
    assert!(answer.contains("beta-worker"));
    assert!(answer.contains("3001"));
    assert!(answer.contains("5432"));
}

#[test]
fn structured_source_unit_inventory_answer_selects_relevant_group_fields() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Group, QueryTargetKind::Flag];
    let answer = build_structured_source_unit_inventory_answer(
            "Which groups are defined and which flags are active?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: groups.alpha.mode=primary; groups.beta.mode=secondary; groups.gamma.flag=true; components.worker.groups=alpha, beta",
            )],
        )
        .expect("generic group inventory answer");

    assert!(answer.contains("alpha"));
    assert!(answer.contains("beta"));
    assert!(answer.contains("gamma"));
    assert!(answer.contains("flag=true"));
}

#[test]
fn structured_source_unit_inventory_answer_abstains_for_typed_table_column_inventory() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary];

    assert!(
            build_structured_source_unit_inventory_answer(
                "Which columns are in the accounts table?",
                &query_ir,
                &[source_unit(
                    0,
                    "[unit_ordinal=0] fields: routes.accounts.columns=Application route handlers; routes.accounts.description=Handler list",
                )],
            )
            .is_none()
        );
}

#[test]
fn structured_source_unit_inventory_answer_abstains_for_latest_release_slice() {
    let query_ir = latest_source_slice_ir(2);

    assert!(
            build_structured_source_unit_inventory_answer(
                "latest release records",
                &query_ir,
                &[source_unit(
                    0,
                    "[unit_ordinal=0] fields: releases.alpha.version=1.0.2; releases.alpha.change=neutral-evidence",
                )],
            )
            .is_none()
        );
}

#[test]
fn structured_source_unit_inventory_answer_selects_relevant_status_fields() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Parameter, QueryTargetKind::ErrorCode];
    let answer = build_structured_source_unit_inventory_answer(
            "Which reason codes are valid for quantity adjustments and what status is returned for insufficient quantity?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: records.item.adjust.description=Creates a quantity adjustment record. Positive quantities increase balance (receiving), negative quantities decrease balance (damage, loss, correction).; records.item.adjust.request.schema.properties.reason_code.enum=receiving, return, damage, loss, correction, transfer_in, transfer_out; records.item.adjust.responses.422.description=Insufficient quantity for negative adjustment",
            )],
        )
        .expect("generic status inventory answer");

    assert!(answer.contains("receiving"));
    assert!(answer.contains("damage"));
    assert!(answer.contains("correction"));
    assert!(answer.contains("422"));
}

#[test]
fn structured_source_unit_inventory_answer_selects_relevant_event_and_secret_fields() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Event, QueryTargetKind::Credential];
    let answer = build_structured_source_unit_inventory_answer(
            "Which events are supported and how are event payloads authenticated?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: callbacks.create.description=Registers a URL to receive event notifications. Supported events: alpha.low, alpha.adjusted, beta.created. Event payloads are signed with HMAC-SHA256 using the returned secret.; callbacks.create.response.schema.properties.secret.description=Secret used for HMAC-SHA256 signatures",
            )],
        )
        .expect("generic event inventory answer");

    assert!(answer.contains("alpha.low"));
    assert!(answer.contains("alpha.adjusted"));
    assert!(answer.contains("beta.created"));
    assert!(answer.contains("HMAC-SHA256"));
}

#[test]
fn structured_source_unit_inventory_answer_expands_relevant_sibling_fields() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Event, QueryTargetKind::Credential];
    let answer = build_structured_source_unit_inventory_answer(
            "Which events are supported and how are event payloads authenticated?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: resources.alpha.create.responses.201.schema.properties.events.type=array; resources.alpha.create.responses.201.schema.properties.events.items.enum.0=alpha.low; resources.alpha.create.responses.201.schema.properties.events.items.enum.1=alpha.adjusted; resources.alpha.create.responses.201.schema.properties.id.format=uuid; resources.alpha.create.responses.201.schema.properties.secret.description=HMAC-SHA256 signing secret shown once; resources.alpha.create.responses.201.schema.properties.secret.type=string; resources.alpha.create.summary=Register event receiver",
            )],
        )
        .expect("sibling-expanded structured inventory answer");

    assert!(answer.contains("alpha.low"));
    assert!(answer.contains("alpha.adjusted"));
    assert!(answer.contains("HMAC-SHA256"));
}

#[test]
fn structured_source_unit_inventory_answer_keeps_specific_sibling_fields_with_broad_distractors() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Event, QueryTargetKind::Credential];
    let answer = build_structured_source_unit_inventory_answer(
            "What callback events does the inventory API support, and how are callback payloads authenticated?",
            &query_ir,
            &[
                source_unit(
                    0,
                    "[unit_ordinal=0] fields: info.description=REST API for managing inventory, records, orders, and supplier relationships with real-time synchronization; paths._records.get.description=Returns records with filtering by category, status, location, and range; paths._records.post.description=Creates a new record and updates inventory counters; components.schemas.Record.properties.status.enum=active, discontinued, draft",
                ),
                source_unit(
                    1,
                    "[unit_ordinal=0] fields: paths._callbacks.post.requestBody.required=true; paths._callbacks.post.responses.201.content.application_json.schema.properties.created_at.format=date-time; paths._callbacks.post.responses.201.content.application_json.schema.properties.created_at.type=string; paths._callbacks.post.responses.201.content.application_json.schema.properties.events.type=array; paths._callbacks.post.responses.201.content.application_json.schema.properties.id.format=uuid; paths._callbacks.post.responses.201.content.application_json.schema.properties.id.type=string; paths._callbacks.post.responses.201.content.application_json.schema.properties.secret.description=HMAC-SHA256 signing secret shown once; paths._callbacks.post.responses.201.content.application_json.schema.properties.secret.type=string; paths._callbacks.post.responses.201.description=Callback registered; paths._callbacks.post.summary=Register a callback endpoint; paths._callbacks.post.tags=Callbacks",
                ),
            ],
        )
        .expect("specific sibling fields should survive broad distractors");

    assert!(answer.contains("events.type=array"), "{answer}");
    assert!(answer.contains("HMAC-SHA256"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_expands_compact_root_groups_ahead_of_subject_noise() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Group, QueryTargetKind::State];
    let answer = build_structured_source_unit_inventory_answer(
            "Which groups are defined, which are active, and what arrangement is described?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: groups.alpha.mode=primary; groups.beta.active=true; groups.gamma.mode=standby; items.runner.description=Group-aware runner component; items.worker.description=Arranges jobs across active targets",
            )],
        )
        .expect("compact root inventory answer");

    let alpha_index = answer.find("groups.alpha.mode=primary").unwrap_or(usize::MAX);
    let gamma_index = answer.find("groups.gamma.mode=standby").unwrap_or(usize::MAX);
    let item_index = answer.find("items.runner.description").unwrap_or(usize::MAX);
    assert!(alpha_index < item_index, "{answer}");
    assert!(gamma_index < item_index, "{answer}");
    assert!(answer.contains("groups.beta.active=true"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_does_not_expand_large_root_as_direct_match() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Item, QueryTargetKind::Value];
    let distractors = (0..10)
        .map(|index| format!("items.extra{index}.description=Supplemental note {index}"))
        .collect::<Vec<_>>()
        .join("; ");
    let answer = build_structured_source_unit_inventory_answer(
            "Which item values are listed?",
            &query_ir,
            &[source_unit(
                0,
                &format!(
                    "[unit_ordinal=0] fields: items.alpha.values=10, 20; items.beta.values=30, 40; {distractors}"
                ),
            )],
        )
        .expect("direct value inventory answer");

    assert!(answer.contains("items.alpha.values=10, 20"), "{answer}");
    assert!(answer.contains("items.beta.values=30, 40"), "{answer}");
    assert!(!answer.contains("items.extra0.description"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_ignores_off_document_source_units_when_ordinary_chunk_wins()
 {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Group, QueryTargetKind::State];
    let target_document_id = Uuid::now_v7();
    let mut ordinary = evidence_chunk(
        0,
        Some("paragraph"),
        "Target document describes the requested groups in prose.",
    );
    ordinary.document_id = target_document_id;
    ordinary.document_label = "target.txt".to_string();
    ordinary.score = Some(25.0);
    let mut off_document = source_unit(
        1,
        "[unit_ordinal=0] fields: groups.alpha.mode=primary; groups.beta.active=true",
    );
    off_document.document_label = "other.json".to_string();
    off_document.score = Some(10.0);

    assert!(
        build_structured_source_unit_inventory_answer(
            "Which groups are defined and active?",
            &query_ir,
            &[ordinary, off_document],
        )
        .is_none()
    );
}

#[test]
fn structured_source_unit_inventory_answer_requires_distinctive_surface_anchor_when_present() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Service, QueryTargetKind::Connection];

    assert!(
            build_structured_source_unit_inventory_answer(
                "Which services use BETA_QUEUE_URL and what is the connection value?",
                &query_ir,
                &[source_unit(
                    0,
                    "[unit_ordinal=0] fields: services.alpha.environment.DATABASE_URL=alpha-db; services.alpha.environment.CACHE_URL=alpha-cache",
                )],
            )
            .is_none()
        );

    let answer = build_structured_source_unit_inventory_answer(
            "Which services use BETA_QUEUE_URL and what is the connection value?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: services.beta.environment.BETA_QUEUE_URL=beta-queue; services.beta.environment.CACHE_URL=beta-cache",
            )],
        )
        .expect("exact surface anchor should allow source-unit answer");

    assert!(answer.contains("BETA_QUEUE_URL"), "{answer}");
    assert!(answer.contains("beta-queue"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_uses_document_with_exact_field_anchor() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Entry, QueryTargetKind::Value];
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Identifier, text: "AlphaToken".to_string() }];
    let focused_document_id = Uuid::now_v7();
    let distractor_document_id = Uuid::now_v7();
    let mut focused = source_unit(
        0,
        "[unit_ordinal=0] fields: entries.sender.marker=AlphaToken; entries.sender.value=alphatoken://{account}/{item}; entries.sender.mode=active",
    );
    focused.document_id = focused_document_id;
    focused.document_label = "focused-records.jsonl".to_string();
    let mut distractor = source_unit(
        1,
        "[unit_ordinal=0] fields: entries.worker.value=beta://worker; entries.monitor.value=gamma://monitor; entries.worker.mode=active",
    );
    distractor.document_id = distractor_document_id;
    distractor.document_label = "nearby-records.jsonl".to_string();
    distractor.score = Some(1_000_000.0);

    let answer = build_structured_source_unit_inventory_answer(
        "Which entries reference AlphaToken and what value format is used?",
        &query_ir,
        &[distractor, focused],
    )
    .expect("focused source-unit inventory answer");

    assert!(answer.contains("focused-records.jsonl"), "{answer}");
    assert!(answer.contains("AlphaToken"), "{answer}");
    assert!(answer.contains("alphatoken://{account}/{item}"), "{answer}");
    assert!(!answer.contains("nearby-records.jsonl"), "{answer}");
    assert!(!answer.contains("beta://worker"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_keeps_noisy_same_item_siblings_below_direct_fields() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Enumerate;
    query_ir.target_types = vec![QueryTargetKind::Value];
    let answer = build_structured_source_unit_inventory_answer(
            "Which values are exposed for each item?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: items.alpha.values=10, 20; items.alpha.description=Detailed Alpha component with long identifier alpha-component-2026-prod; items.beta.values=30, 40; items.beta.description=Detailed Beta component with long identifier beta-component-2026-prod",
            )],
        )
        .expect("direct value inventory answer");

    let values_index = answer.find("items.alpha.values=10, 20").unwrap_or(usize::MAX);
    let description_index = answer.find("items.alpha.description").unwrap_or(usize::MAX);
    assert!(values_index < description_index, "{answer}");
    assert!(answer.contains("items.beta.values=30, 40"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_upgrades_weak_direct_sibling_matches() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Event];
    let distractors = (0..24)
        .map(|index| format!("channels.alpha.metadata.field{index}=metadata-{index}"))
        .collect::<Vec<_>>()
        .join("; ");
    let answer = build_structured_source_unit_inventory_answer(
            "Which channel events are supported and how are channel payloads authenticated?",
            &query_ir,
            &[source_unit(
                0,
                &format!(
                    "[unit_ordinal=0] fields: {distractors}; channels.alpha.create.schema.properties.events.type=array; channels.alpha.create.schema.properties.events.items.enum=alpha.low, alpha.adjusted; channels.alpha.create.schema.properties.secret.description=HMAC-SHA256 signing secret"
                ),
            )],
        )
        .expect("sibling-expanded structured inventory answer");

    assert!(answer.contains("alpha.low"), "{answer}");
    assert!(answer.contains("alpha.adjusted"), "{answer}");
    assert!(answer.contains("HMAC-SHA256"), "{answer}");
}

#[test]
fn structured_source_unit_inventory_answer_abstains_for_broad_concept_explanation() {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![QueryTargetKind::Protocol, QueryTargetKind::Concept];
    let document_id = Uuid::now_v7();
    let mut ordinary = evidence_chunk(
        0,
        Some("paragraph"),
        "Protocol X version 2 improves multiplexing, header compression, and binary framing over version 1.",
    );
    ordinary.document_id = document_id;
    ordinary.document_label = "protocol-x-notes.md".to_string();
    let mut structured = source_unit(
        1,
        "[unit_ordinal=0] fields: services.alpha.endpoint=https://example.invalid:9443; services.alpha.timeout=30; services.beta.enabled=true",
    );
    structured.document_id = Uuid::now_v7();
    structured.document_label = "neighboring-config.yaml".to_string();

    let answer = build_structured_source_unit_inventory_answer(
        "What are the main improvements of Protocol X version 2 over version 1?",
        &query_ir,
        &[ordinary, structured],
    );

    assert!(
        answer.is_none(),
        "broad concept/protocol questions should use ordinary evidence synthesis instead of structured field inventory: {answer:?}"
    );
}

fn named_document_overview_ir(act: QueryAct, filename: &str) -> QueryIR {
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = act;
    query_ir.scope = QueryScope::SingleDocument;
    query_ir.confidence = 0.99;
    query_ir.target_types = vec![QueryTargetKind::Document];
    query_ir.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: filename.to_string() });
    query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
        label: filename.to_string(),
        role: crate::domains::query_ir::EntityRole::Subject,
    }];
    query_ir.literal_constraints =
        vec![LiteralSpan { kind: LiteralKind::Path, text: filename.to_string() }];
    query_ir.retrieval_query =
        Some(format!("{filename} (Документ) что известно в этой библиотеке"));
    query_ir
}

fn named_document_overview_unit(filename: &str) -> RuntimeMatchedChunk {
    let mut unit = source_unit(
        0,
        "[unit_ordinal=0] fields: text=Кажется, у тебя нет активных задач на генерацию видео. Попробуй начать заново. Text-монтаж и улучшить качество видео; actor_role=user",
    );
    unit.document_label = filename.to_string();
    unit
}

#[test]
fn structured_source_unit_inventory_answer_abstains_for_named_document_overview() {
    let filename = "chat-8530110298.records.jsonl";
    for act in [QueryAct::Describe, QueryAct::Enumerate, QueryAct::RetrieveValue] {
        let answer = build_structured_source_unit_inventory_answer(
            &format!("Что известно про «{filename}» (Документ) в этой библиотеке?"),
            &named_document_overview_ir(act, filename),
            &[named_document_overview_unit(filename)],
        );
        assert!(
            answer.is_none(),
            "{act:?} named-document overview must synthesize instead of dumping record text= bodies: {answer:?}"
        );
    }
}

#[test]
fn structured_source_unit_inventory_answer_keeps_field_literal_on_named_document() {
    let filename = "chat-8530110298.records.jsonl";
    let mut query_ir = low_confidence_concept_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.scope = QueryScope::SingleDocument;
    query_ir.confidence = 0.99;
    query_ir.target_types = vec![QueryTargetKind::Document, QueryTargetKind::Attribute];
    query_ir.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: filename.to_string() });
    query_ir.literal_constraints = vec![
        LiteralSpan { kind: LiteralKind::Path, text: filename.to_string() },
        LiteralSpan { kind: LiteralKind::Identifier, text: "actor_role".to_string() },
    ];

    let mut unit = source_unit(
        0,
        "[unit_ordinal=0] fields: text=long unused prose body that must not become the answer; actor_role=user",
    );
    unit.document_label = filename.to_string();

    let answer = build_structured_source_unit_inventory_answer(
        &format!("Which actor_role values appear in {filename}?"),
        &query_ir,
        &[unit],
    )
    .expect("field-literal inventory on a named document");

    assert!(answer.contains("actor_role=user"), "{answer}");
    assert!(!answer.contains("long unused prose"), "{answer}");
}

#[test]
fn query_ir_allows_structured_source_unit_inventory_rejects_document_path_overview() {
    let filename = "chat-8530110298.records.jsonl";
    let query_ir = named_document_overview_ir(QueryAct::Describe, filename);
    assert!(
        !query_ir_allows_structured_source_unit_inventory(&query_ir),
        "path-only document overview must not open structured inventory"
    );

    let mut with_field = query_ir.clone();
    with_field
        .literal_constraints
        .push(LiteralSpan { kind: LiteralKind::Identifier, text: "actor_role".to_string() });
    assert!(
        query_ir_allows_structured_source_unit_inventory(&with_field),
        "an identifier field literal on a named document must still open inventory"
    );
}
