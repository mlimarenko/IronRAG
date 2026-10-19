use super::rendering::render_record_unit;
use super::*;

#[test]
fn extracts_generic_record_jsonl_units() {
    // Arbitrary heterogeneous records — no assumed key vocabulary. The
    // profile is shape-based; every leaf value (including ones nested under
    // arbitrary keys) is searchable, and a value-shaped timestamp under any
    // key stamps the record.
    let payload = br#"{"id":"msg-1","recordedAt":"2026-04-28T09:00:00Z","speaker":{"role":"user","label":"User One"},"text":"How do I configure search?"}
{"ref":"msg-2","speaker":{"role":"assistant"},"body":"Use the retrieval settings."}"#;

    let output = extract_record_jsonl(payload).expect("jsonl extraction");

    assert_eq!(output.extraction_kind, RECORD_JSONL_SOURCE_FORMAT);
    assert_eq!(output.source_metadata.source_format, RECORD_JSONL_SOURCE_FORMAT);
    assert_eq!(output.structure_hints.lines.len(), 3);
    assert_record_jsonl_content_shape(&output);
    assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
    assert_eq!(output.source_map["sourceProfile"]["unitCount"], serde_json::json!(2));
    assert_eq!(output.source_map["sourceProfile"]["keyCoverage"]["id"], serde_json::json!(1));
    assert_record_jsonl_source_map_shape(&output);
}

fn assert_record_jsonl_content_shape(output: &crate::shared::extraction::ExtractionOutput) {
    assert!(output.content_text.starts_with("[source_profile source_format=record_jsonl"));
    assert!(output.content_text.contains("unit_count=2"));
    // Heterogeneous schema is observable via shape-based key coverage.
    assert!(output.content_text.contains("distinct_key_count="));
    assert!(output.content_text.contains("top_keys="));
    // Every leaf value stays searchable text, regardless of key name.
    assert!(output.content_text.contains("id=msg-1"));
    assert!(output.content_text.contains("ref=msg-2"));
    assert!(output.content_text.contains("speaker.role=user"));
    assert!(output.content_text.contains("speaker.role=assistant"));
    // Value-shaped timestamp under an arbitrary key name stamps the record.
    assert!(output.content_text.contains("occurred_at=2026-04-28T09:00:00+00:00"));
}

fn assert_record_jsonl_source_map_shape(output: &crate::shared::extraction::ExtractionOutput) {
    assert!(output.structure_hints.lines[0].source_ordinals.is_empty());
    assert!(output.structure_hints.lines[0].signals.contains(&ExtractionLineSignal::SourceProfile));
    assert!(
        output.structure_hints.lines[0].signals.contains(&ExtractionLineSignal::MetadataCandidate)
    );
    assert!(
        output
            .structure_hints
            .lines
            .iter()
            .skip(1)
            .all(|line| line.signals.contains(&ExtractionLineSignal::SourceUnit))
    );
    assert!(
        output
            .structure_hints
            .lines
            .iter()
            .skip(1)
            .all(|line| { !line.signals.contains(&ExtractionLineSignal::MetadataCandidate) })
    );
}

#[test]
fn renders_deep_leaf_fields_for_detail_retrieval() {
    // Every leaf — scalars, deeply-nested object values, and scalar arrays —
    // is flattened to dotted-path searchable text. No key is privileged.
    let output = extract_record_jsonl(
        br#"{"id":"row-1","recordedAt":"2026-03-14T12:00:00Z","status":"approved","amount":42,"details":{"provider":"Provider Beta","retry":false},"tags":["alpha","receipt"]}"#,
    )
    .expect("jsonl extraction");

    assert!(output.content_text.contains("occurred_at=2026-03-14T12:00:00+00:00"));
    assert!(output.content_text.contains("fields:"));
    assert!(output.content_text.contains("status=approved"));
    assert!(output.content_text.contains("amount=42"));
    assert!(output.content_text.contains("details.provider=Provider Beta"));
    assert!(output.content_text.contains("details.retry=false"));
    assert!(output.content_text.contains("tags=alpha, receipt"));
}

#[test]
fn renders_late_structural_fields_for_inventory_retrieval() {
    let mut record = serde_json::Map::new();
    for index in 0..80 {
        record.insert(format!("early_{index:02}"), serde_json::json!(format!("value-{index}")));
    }
    record.insert(
        "service".to_string(),
        serde_json::json!({
            "ports": ["5432:5432", "5672:5672"],
            "environment": {
                "RABBITMQ_URL": "amqp://rabbitmq:5672",
                "RATE_LIMIT_REQUESTS": "100"
            },
            "webhooks": ["stock.low", "stock.adjusted"],
            "reason_code": ["receiving", "damage", "loss", "correction"]
        }),
    );

    let output = normalize_structured_records(
        serde_json::Value::Object(record).to_string().as_bytes(),
        StructuredRecordHint::Json,
    )
    .expect("wide record normalizes");

    assert!(output.content_text.contains("service.ports=5432:5432, 5672:5672"));
    assert!(output.content_text.contains("service.environment.RABBITMQ_URL=amqp://rabbitmq:5672"));
    assert!(output.content_text.contains("service.environment.RATE_LIMIT_REQUESTS=100"));
    assert!(output.content_text.contains("service.webhooks=stock.low, stock.adjusted"));
    assert!(
        output.content_text.contains("service.reason_code=receiving, damage, loss, correction")
    );
}

#[test]
fn splits_wide_record_units_without_losing_late_fields() {
    let mut record = serde_json::Map::new();
    for index in 0..32 {
        record.insert(format!("early_{index:02}"), serde_json::json!(format!("value-{index}")));
    }
    record.insert(
        "service".to_string(),
        serde_json::json!({
            "ports": ["5432:5432", "5672:5672"],
            "environment": {
                "RABBITMQ_URL": "amqp://rabbitmq:5672",
                "RATE_LIMIT_REQUESTS": "100"
            },
            "webhooks": ["stock.low", "stock.adjusted"],
            "reason_code": ["receiving", "damage", "loss", "correction"]
        }),
    );

    let rendered = render_record_unit(&record, 0).expect("rendered record");
    let block = StructuredBlockData {
        block_id: uuid::Uuid::now_v7(),
        ordinal: 0,
        block_kind: StructuredBlockKind::SourceUnit,
        text: rendered.clone(),
        normalized_text: rendered,
        heading_trail: Vec::new(),
        section_path: Vec::new(),
        page_number: None,
        source_span: None,
        parent_block_id: None,
        table_coordinates: None,
        code_language: None,
        is_boilerplate: false,
    };

    let split = split_large_record_units(&[block], 360);

    assert!(split.len() > 1);
    assert!(split.iter().all(|block| block.block_kind == StructuredBlockKind::SourceUnit));
    assert!(split.iter().any(|block| {
        block.normalized_text.contains("service.environment.RABBITMQ_URL=amqp://rabbitmq:5672")
    }));
    assert!(split.iter().any(|block| {
        block.normalized_text.contains("service.reason_code=receiving, damage, loss, correction")
    }));
    assert!(split.iter().all(|block| block.normalized_text.contains("unit_ordinal=0")));
}

#[test]
fn focused_record_unit_excerpt_centers_late_matching_fields() {
    let mut record = serde_json::Map::new();
    for index in 0..40 {
        record.insert(format!("early_{index:02}"), serde_json::json!(format!("value-{index}")));
    }
    record.insert(
        "service".to_string(),
        serde_json::json!({
            "deep": {
                "port": "8443",
                "protocol": "https"
            }
        }),
    );
    let rendered = render_record_unit(&record, 0).expect("rendered record");

    let excerpt = focused_record_unit_excerpt(
        &rendered,
        &["service.deep.port".to_string(), "8443".to_string()],
        260,
    )
    .expect("focused excerpt");

    assert!(excerpt.contains("service.deep.port=8443"));
    assert!(!excerpt.contains("early_00=value-0"));
}

#[test]
fn focused_record_unit_excerpt_does_not_prioritize_handwritten_field_names() {
    let text = "[unit_ordinal=0] fields: f0=aaaaaaaaaaaaaaaa; first.payload=needle; \
                f2=bbbbbbbbbbbbbbbb; f3=cccccccccccccccc; f4=dddddddddddddddd; \
                second.description=needle; f6=eeeeeeeeeeeeeeee";

    let excerpt =
        focused_record_unit_excerpt(text, &["needle".to_string()], 70).expect("focused excerpt");

    assert!(excerpt.contains("first.payload=needle"));
    assert!(!excerpt.contains("second.description=needle"));
}

#[test]
fn focused_record_unit_excerpt_preserves_matching_field_when_neighbors_exceed_budget() {
    let text = "[unit_ordinal=0] fields: left=aaaaaaaaaaaaaaaa; target=needle; \
                right=bbbbbbbbbbbbbbbb";

    let excerpt = focused_record_unit_excerpt(text, &["needle".to_string()], 45);

    assert_eq!(excerpt.as_deref(), Some("[unit_ordinal=0] fields: target=needle"));
}

#[test]
fn rejects_record_without_searchable_content() {
    let error = extract_record_jsonl(br#"{}"#).unwrap_err();
    assert!(error.to_string().contains("non-null scalar"));
}

#[test]
fn extract_chunk_temporal_bounds_returns_none_for_chunks_without_header() {
    assert!(extract_chunk_temporal_bounds("plain text without any temporal header").is_none());
    assert!(extract_chunk_temporal_bounds("").is_none());
}

#[test]
fn extract_chunk_temporal_bounds_handles_single_record_chunk() {
    let text = "[unit_ordinal=0 occurred_at=2024-09-06T12:19:38+00:00] fields: body=hello world";
    let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
    assert_eq!(min, max);
    assert_eq!(min.to_rfc3339(), "2024-09-06T12:19:38+00:00");
}

#[test]
fn extract_chunk_temporal_bounds_aggregates_min_and_max_across_records() {
    let text = "\
        [unit_ordinal=0 occurred_at=2024-09-01T09:37:18+00:00] fields: a=one\n\
        [unit_ordinal=1 occurred_at=2026-04-28T10:52:10+00:00] fields: a=two\n\
        [unit_ordinal=2 occurred_at=2025-03-15T18:00:00+00:00] fields: a=three";
    let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
    assert_eq!(min.to_rfc3339(), "2024-09-01T09:37:18+00:00");
    assert_eq!(max.to_rfc3339(), "2026-04-28T10:52:10+00:00");
}

#[test]
fn extract_chunk_temporal_bounds_normalizes_zulu_form() {
    let text = "[occurred_at=2026-03-15T12:00:00Z] body";
    let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
    assert_eq!(min, max);
    assert_eq!(min.timezone(), Utc);
}

#[test]
fn extract_chunk_temporal_bounds_skips_unparseable_timestamps() {
    let text = "[occurred_at=not-a-date occurred_at=2024-09-01T00:00:00+00:00]";
    let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");
    assert_eq!(min, max);
    assert_eq!(min.to_rfc3339(), "2024-09-01T00:00:00+00:00");
}

#[test]
fn extract_chunk_temporal_bounds_ignores_config_comment_tokens() {
    let text = "[unit_ordinal=0 occurred_at=2026-04-28T09:00:00+00:00] fields: id=alpha-1\n\
# occurred_at=2001-01-01T00:00:00Z";
    let (min, max) = extract_chunk_temporal_bounds(text).expect("bounds present");

    assert_eq!(min, max);
    assert_eq!(min.to_rfc3339(), "2026-04-28T09:00:00+00:00");
}

#[test]
fn project_record_unit_values_strips_scaffolding_keeps_values() {
    // A realistic rendered source_unit line: the header (unit_ordinal +
    // occurred_at), the dotted-path KEYS, and the timestamp VALUE are all
    // scaffolding; only the real values must reach the graph LLM.
    let text = "[unit_ordinal=0 occurred_at=2026-04-22T19:05:00+00:00] fields: \
carrier=ZephyrFreight-77; origin=depot-gamma; destination=warehouse-beta; \
eventTime=2026-04-22T19:05:00+00:00; invoice=/srv/billing/invoices/inv-9931.pdf";
    let projected = project_record_unit_values_for_graph(text).expect("projection present");
    // Real values survive verbatim (the prompt requires byte-for-byte labels).
    assert!(projected.contains("ZephyrFreight-77"));
    assert!(projected.contains("depot-gamma"));
    assert!(projected.contains("warehouse-beta"));
    assert!(projected.contains("/srv/billing/invoices/inv-9931.pdf"));
    // Scaffolding is gone: no header, no dotted-path keys, no raw timestamp.
    assert!(!projected.contains("unit_ordinal"));
    assert!(!projected.contains("occurred_at"));
    assert!(!projected.contains("carrier="));
    assert!(!projected.contains("origin="));
    assert!(!projected.contains("eventTime"));
    assert!(!projected.contains("2026-04-22T19:05:00"));
}

#[test]
fn project_record_unit_values_keeps_value_internal_equals_and_deep_keys() {
    // A value carrying its own `=` (a URL/DSN) must survive intact, and a
    // deep dotted-path key (shelves.0.code) must be dropped entirely.
    let text = "[unit_ordinal=3] fields: \
shelves.0.code=A1; shelves.0.contents=postgres://host/app?sslmode=require";
    let projected = project_record_unit_values_for_graph(text).expect("projection present");
    assert!(projected.contains("A1"));
    assert!(projected.contains("postgres://host/app?sslmode=require"));
    assert!(!projected.contains("shelves.0.code"));
    assert!(!projected.contains("shelves.0.contents"));
}

#[test]
fn project_record_unit_values_returns_none_when_only_timestamps_remain() {
    // A record whose sole leaf value is a timestamp projects to nothing —
    // there is no real entity to feed the graph.
    let text = "[unit_ordinal=0 occurred_at=2026-04-22T19:05:00+00:00] fields: \
at=2026-04-22T19:05:00+00:00";
    assert!(project_record_unit_values_for_graph(text).is_none());
}

#[test]
fn project_record_unit_values_returns_none_without_fields_marker() {
    // The source_profile line and any non-record text carry no `fields:`
    // boundary, so they project to nothing.
    assert!(
        project_record_unit_values_for_graph(
            "[source_profile source_format=record_jsonl unit_count=2 top_keys=carrier:1]"
        )
        .is_none()
    );
    assert!(project_record_unit_values_for_graph("plain prose without a marker").is_none());
}

#[test]
fn project_record_unit_values_round_trips_render_record_unit_output() {
    // Locks the projection to exactly what render_record_unit emits: the
    // value reaches the graph, the key and the timestamp do not.
    let payload = br#"{"carrier":"ZephyrFreight-77","shippedAt":"2026-03-15T12:00:00Z"}"#;
    let output = extract_record_jsonl(payload).expect("jsonl extraction");
    let unit_line = output
        .content_text
        .lines()
        .find(|line| line.contains("fields:"))
        .expect("source unit line present");
    let projected = project_record_unit_values_for_graph(unit_line).expect("projection present");
    assert_eq!(projected, "ZephyrFreight-77");
}

#[test]
fn extract_chunk_temporal_bounds_round_trips_render_record_unit_output() {
    // Verifies the helper consumes exactly the format render_record_unit
    // produces — keeping the read/write halves locked together. The
    // timestamp lives under an arbitrary key and is detected by shape.
    let payload = br#"{"id":"msg-x","emittedAt":"2026-03-15T12:00:00Z","text":"hello"}"#;
    let output = extract_record_jsonl(payload).expect("jsonl extraction");
    let (min, max) = extract_chunk_temporal_bounds(&output.content_text).expect("bounds present");
    assert_eq!(min, max);
    assert_eq!(min.to_rfc3339(), "2026-03-15T12:00:00+00:00");
}

#[test]
fn structured_record_hint_resolves_from_extension_and_mime() {
    use StructuredRecordHint::{Json, Jsonl, Toml, Yaml};
    assert_eq!(structured_record_hint(Some("a.jsonl"), None), Some(Jsonl));
    assert_eq!(structured_record_hint(Some("a.ndjson"), None), Some(Jsonl));
    assert_eq!(structured_record_hint(Some("a.json"), None), Some(Json));
    assert_eq!(structured_record_hint(Some("a.yaml"), None), Some(Yaml));
    assert_eq!(structured_record_hint(Some("a.yml"), None), Some(Yaml));
    assert_eq!(structured_record_hint(Some("a.toml"), None), Some(Toml));
    assert_eq!(structured_record_hint(None, Some("application/json")), Some(Json));
    assert_eq!(structured_record_hint(None, Some("application/yaml")), Some(Yaml));
    assert_eq!(structured_record_hint(None, Some("text/yaml; charset=utf-8")), Some(Yaml));
    assert_eq!(structured_record_hint(None, Some("application/toml")), Some(Toml));
    assert_eq!(structured_record_hint(None, Some("application/x-ndjson")), Some(Jsonl));
    // Arbitrary text-like input must never be offered to the record path.
    assert_eq!(structured_record_hint(Some("notes.md"), Some("text/markdown")), None);
    assert_eq!(structured_record_hint(Some("readme.txt"), None), None);
    assert_eq!(structured_record_hint(None, Some("text/plain")), None);
}

#[test]
fn normalizes_single_json_object_into_one_record() {
    let payload = br#"{"id":"alpha-1","status":"approved","detail":{"provider":"Provider Beta"}}"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("json object normalizes");

    assert_eq!(output.source_metadata.source_format, RECORD_JSONL_SOURCE_FORMAT);
    assert_eq!(output.source_map["recordCount"], serde_json::json!(1));
    assert!(output.content_text.contains("unit_count=1"));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("status=approved"));
    assert!(output.content_text.contains("detail.provider=Provider Beta"));
}

#[test]
fn normalizes_heterogeneous_json_array_into_per_element_records() {
    // Each element carries a DIFFERENT field set; a value present in only
    // one element must still be searchable and attributed to that record.
    let payload = br#"[
        {"id":"alpha-1","status":"approved","region":"north"},
        {"id":"beta-2","amount":4242,"carrier":"Carrier Gamma"},
        {"id":"gamma-3","note":"deferred receipt","priority":7}
    ]"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("json array normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(3));
    assert!(output.content_text.contains("unit_count=3"));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("id=beta-2"));
    assert!(output.content_text.contains("id=gamma-3"));
    // Field present in only one record is searchable text.
    assert!(output.content_text.contains("carrier=Carrier Gamma"));
    assert!(output.content_text.contains("region=north"));
    assert!(output.content_text.contains("note=deferred receipt"));
    // The field deep in one record is attributed to that record's line.
    let carrier_line = output
        .structure_hints
        .lines
        .iter()
        .find(|line| line.text.contains("carrier=Carrier Gamma"))
        .expect("carrier line present");
    assert!(carrier_line.text.contains("id=beta-2"));
    assert_eq!(carrier_line.source_ordinals, vec![1]);
}

#[test]
fn normalizes_single_yaml_document_into_one_record() {
    let payload = b"id: alpha-1\nstatus: approved\ndetail:\n  provider: Provider Beta\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("yaml document normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(1));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("status=approved"));
    assert!(output.content_text.contains("detail.provider=Provider Beta"));
}

#[test]
fn comment_free_yaml_rendering_is_byte_stable() {
    let payload = b"id: alpha-1\nstatus: approved\ndetail:\n  provider: Provider Beta\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("yaml document normalizes");

    assert_eq!(
        output.content_text,
        "[source_profile source_format=record_jsonl sequence_kind=record_stream unit_count=1 \
text_unit_count=1 distinct_key_count=3 top_keys=detail:1,id:1,status:1]\n\
[unit_ordinal=0] fields: detail.provider=Provider Beta; id=alpha-1; status=approved"
    );
}

#[test]
fn preserves_yaml_comment_runs_next_to_annotated_port_field() {
    let payload = include_bytes!("../../../../tests/fixtures/extraction/config-comments.yaml");
    let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("annotated yaml document normalizes");
    let unit = output
        .structure_hints
        .lines
        .iter()
        .find(|line| line.signals.contains(&ExtractionLineSignal::SourceUnit))
        .expect("source unit present");

    assert!(unit.text.contains("services.rabbitmq.ports=5672:5672, 15672:15672"));
    assert!(unit.text.contains("# AMQP port: 5672"));
    assert!(unit.text.contains("# Management UI port: 15672"));
}

#[test]
fn normalizes_multi_document_yaml_stream_into_per_document_records() {
    let payload = b"\
id: alpha-1\nregion: north\n---\nid: beta-2\ncarrier: Carrier Gamma\n---\nid: gamma-3\npriority: 7\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("yaml stream normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(3));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("id=beta-2"));
    assert!(output.content_text.contains("id=gamma-3"));
    assert!(output.content_text.contains("carrier=Carrier Gamma"));
    let carrier_line = output
        .structure_hints
        .lines
        .iter()
        .find(|line| line.text.contains("carrier=Carrier Gamma"))
        .expect("carrier line present");
    assert!(carrier_line.text.contains("id=beta-2"));
}

#[test]
fn normalizes_yaml_sequence_of_mappings_into_per_item_records() {
    let payload = b"- id: alpha-1\n  status: approved\n- id: beta-2\n  carrier: Carrier Gamma\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("yaml sequence normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("id=beta-2"));
    assert!(output.content_text.contains("carrier=Carrier Gamma"));
}

#[test]
fn json_hint_falls_back_to_jsonl_when_payload_is_newline_delimited() {
    let payload = br#"{"id":"alpha-1","text":"first"}
{"id":"beta-2","text":"second"}"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("ndjson under a json hint still normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("id=beta-2"));
}

#[test]
fn degrades_non_record_payloads_to_text_path() {
    // Scalar, array of scalars, empty array/object, prose-shaped YAML, and a
    // YAML mapping with a non-string key must all return None so the caller
    // falls through to the existing text extractor.
    assert!(normalize_structured_records(b"42", StructuredRecordHint::Json).is_none());
    assert!(
        normalize_structured_records(br#""just a string""#, StructuredRecordHint::Json).is_none()
    );
    assert!(normalize_structured_records(b"[1, 2, 3]", StructuredRecordHint::Json).is_none());
    assert!(normalize_structured_records(b"[]", StructuredRecordHint::Json).is_none());
    assert!(normalize_structured_records(b"{}", StructuredRecordHint::Json).is_none());
    assert!(
        normalize_structured_records(b"a plain prose sentence\n", StructuredRecordHint::Yaml)
            .is_none()
    );
    assert!(normalize_structured_records(b"- 1\n- 2\n", StructuredRecordHint::Yaml).is_none());
    assert!(normalize_structured_records(b"7\n", StructuredRecordHint::Yaml).is_none());
}

#[test]
fn normalizes_toml_document_into_one_record() {
    let payload = b"id = \"alpha-1\"\nstatus = \"approved\"\n\n[detail]\nprovider = \"Provider Beta\"\nretry = false\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Toml)
        .expect("toml document normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(1));
    assert!(output.content_text.contains("id=alpha-1"));
    assert!(output.content_text.contains("status=approved"));
    assert!(output.content_text.contains("detail.provider=Provider Beta"));
    assert!(output.content_text.contains("detail.retry=false"));
}

#[test]
fn preserves_toml_comment_runs_next_to_annotated_fields() {
    let payload = b"[broker]\n# AMQP port: 5672\namqp_port = 5672\n\
# Management UI port: 15672\nmanagement_port = 15672\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Toml)
        .expect("annotated toml document normalizes");
    let unit = output
        .structure_hints
        .lines
        .iter()
        .find(|line| line.signals.contains(&ExtractionLineSignal::SourceUnit))
        .expect("source unit present");

    let amqp_field = "broker.amqp_port=5672\n# AMQP port: 5672";
    let management_field = "broker.management_port=15672\n# Management UI port: 15672";
    assert!(unit.text.contains(amqp_field));
    assert!(unit.text.contains(management_field));
    assert_eq!(project_record_unit_values_for_graph(&unit.text).as_deref(), Some("5672; 15672"));
}

#[test]
fn toml_with_datetime_value_normalizes_and_stamps_without_panic() {
    // A TOML datetime is preserved as a searchable string value and detected
    // by value shape, stamping the record's temporal bounds. Must not panic.
    let payload = b"id = \"alpha-1\"\nat = 2026-04-28T09:00:00Z\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Toml)
        .expect("toml with datetime normalizes");
    assert!(output.content_text.contains("id=alpha-1"));
    let (min, max) =
        extract_chunk_temporal_bounds(&output.content_text).expect("temporal bounds present");
    assert_eq!(min, max);
    assert_eq!(min.to_rfc3339(), "2026-04-28T09:00:00+00:00");
}

#[test]
fn surfaces_deep_leaves_from_arbitrary_nested_records() {
    // A deeply-nested config-tree-style record from an unknown system: a
    // path/identifier buried several levels under arbitrary keys must still
    // be searchable text, bounded by the depth cap (no per-field lifting).
    let payload = br#"[
        {"kind":"config","tree":{"service":{"db":{"dsn":"postgres://host/app"}}}},
        {"kind":"action","op":{"tool":{"name":"apply_patch","input":{"path":"/srv/app/etc/settings.ini"}}}}
    ]"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("nested records normalize");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(2));
    // Deep leaves under arbitrary key paths are flattened to searchable text.
    assert!(output.content_text.contains("tree.service.db.dsn=postgres://host/app"));
    assert!(output.content_text.contains("op.tool.name=apply_patch"));
    assert!(output.content_text.contains("op.tool.input.path=/srv/app/etc/settings.ini"));
    // The deep path is attributed to its own record line.
    let path_line = output
        .structure_hints
        .lines
        .iter()
        .find(|line| line.text.contains("op.tool.input.path="))
        .expect("path line present");
    assert_eq!(path_line.source_ordinals, vec![1]);
}

#[test]
fn surfaces_deep_schema_property_keys_without_schema_specific_lifting() {
    let payload = br#"{
        "paths": {
            "/items/{itemId}/stock": {
                "post": {
                    "requestBody": {
                        "content": {
                            "application/json": {
                                "schema": {
                                    "properties": {
                                        "reason_code": {
                                            "type": "string",
                                            "enum": ["receiving", "damage"]
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("nested schema normalizes");

    assert!(output.content_text.contains(
        "paths._items__itemId__stock.post.requestBody.content.application_json.schema.properties.reason_code.type=string"
    ));
    assert!(output.content_text.contains(
        "paths._items__itemId__stock.post.requestBody.content.application_json.schema.properties.reason_code.enum=receiving, damage"
    ));
}

#[test]
fn detects_value_shaped_timestamps_under_arbitrary_key_names() {
    // An event/metrics log where the timestamp lives under different,
    // arbitrary key names per record. All must be detected by VALUE SHAPE
    // and ordered into the chunk's temporal span.
    let payload = br#"[
        {"event":"start","loggedAt":"2026-01-02T03:04:05Z"},
        {"event":"tick","eventTime":"2026-03-04T05:06:07Z"},
        {"event":"stop","when":"2026-06-07T08:09:10Z"}
    ]"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("event log normalizes");

    let (min, max) =
        extract_chunk_temporal_bounds(&output.content_text).expect("temporal bounds present");
    assert_eq!(min.to_rfc3339(), "2026-01-02T03:04:05+00:00");
    assert_eq!(max.to_rfc3339(), "2026-06-07T08:09:10+00:00");
    // Each record carries its own occurred_at derived from its differently
    // named timestamp field.
    let start_line = output
        .structure_hints
        .lines
        .iter()
        .find(|line| line.text.contains("event=start"))
        .expect("start line present");
    assert!(start_line.text.contains("occurred_at=2026-01-02T03:04:05+00:00"));
}

#[test]
fn detects_epoch_timestamps_but_ignores_plain_numbers() {
    // 10-digit seconds / 13-digit millis are stamped; ids/counts/amounts are
    // not. Detection is bounded by magnitude, never by key name.
    let payload = br#"{"id":"alpha-1","seenAt":1767322445,"amount":4242,"count":7}"#;
    let output = normalize_structured_records(payload, StructuredRecordHint::Json)
        .expect("record normalizes");
    let (min, _max) = extract_chunk_temporal_bounds(&output.content_text).expect("epoch stamped");
    assert_eq!(min.to_rfc3339(), "2026-01-02T02:54:05+00:00");
    // The plain numbers remain searchable as fields, not mistaken for time.
    assert!(output.content_text.contains("amount=4242"));
    assert!(output.content_text.contains("count=7"));
}

#[test]
fn renders_long_free_text_without_config_width_truncation() {
    // A long prose body (no privileged key) must survive far past the tight
    // single-token field cap.
    let body = "word ".repeat(200);
    let payload =
        serde_json::json!({ "id": "alpha-1", "body": body.trim() }).to_string().into_bytes();
    let output = normalize_structured_records(&payload, StructuredRecordHint::Json)
        .expect("record normalizes");
    // 200 words past RECORD_JSONL_FIELD_VALUE_LIMIT (240 chars) still render.
    assert!(output.content_text.contains(&"word ".repeat(120).trim().to_string()));
}

#[test]
fn normalizes_ragged_yaml_multi_doc_stream() {
    // Multi-document YAML where each document has a ragged, different schema.
    let payload = b"\
order: alpha-1\nlines:\n  - sku: A\n    qty: 2\n---\nshipment: beta-2\ncarrier: Carrier Gamma\neta: 2026-05-01T00:00:00Z\n---\nnote: a free-form remark with several words\n";
    let output = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("ragged yaml normalizes");

    assert_eq!(output.source_map["recordCount"], serde_json::json!(3));
    assert!(output.content_text.contains("order=alpha-1"));
    assert!(output.content_text.contains("lines.0.sku=A"));
    assert!(output.content_text.contains("carrier=Carrier Gamma"));
    assert!(output.content_text.contains("note=a free-form remark with several words"));
    // The shape-based profile observes the ragged union of keys.
    assert!(output.content_text.contains("distinct_key_count="));
    // The only value-shaped timestamp (under `eta`) sets the temporal span.
    let (min, max) =
        extract_chunk_temporal_bounds(&output.content_text).expect("temporal bounds present");
    assert_eq!(min, max);
    assert_eq!(min.to_rfc3339(), "2026-05-01T00:00:00+00:00");
}

#[test]
fn never_panics_on_pathological_inputs() {
    // Robustness floor: huge arrays, deep nesting beyond the cap, mixed
    // types, and empty/garbage inputs must all return cleanly (Some or None),
    // never panic.
    let deep = (0..50).fold(String::from("\"leaf\""), |acc, _| format!("{{\"n\":{acc}}}"));
    let _ = normalize_structured_records(deep.as_bytes(), StructuredRecordHint::Json);
    let wide = format!("[{}]", vec![r#"{"k":1}"#; 5000].join(","));
    let _ = normalize_structured_records(wide.as_bytes(), StructuredRecordHint::Json);
    let _ = normalize_structured_records(b"\xff\xfe not utf8", StructuredRecordHint::Json);
    let _ = normalize_structured_records(b"", StructuredRecordHint::Yaml);
    let _ = normalize_structured_records(b"{ broken json", StructuredRecordHint::Json);
    let _ = normalize_structured_records(b":\n:\n", StructuredRecordHint::Yaml);
}
