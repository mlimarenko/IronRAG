use super::*;

#[test]
fn parses_normalized_json_candidates_only() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "Annual report", "node_type": "artifact", "aliases": [], "sub_type": null, "summary": "report" },
            { "label": "Provider Alpha", "node_type": "concept", "aliases": ["Alpha Provider"], "sub_type": null, "summary": "provider" }
          ],
          "relations": [
            { "source_label": "Annual report", "target_label": "Provider Alpha", "relation_type": "mentions", "summary": "report mentions provider" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.entities.len(), 2);
    assert_eq!(normalized.entities[0].label, "Annual report");
    assert_eq!(normalized.entities[1].node_type, RuntimeNodeType::Concept);
    assert_eq!(normalized.relations[0].relation_type, "mentions");
}

#[test]
fn drops_unknown_node_type() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "Something", "node_type": "invented_type", "aliases": [], "sub_type": null, "summary": "" }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert!(normalized.entities.is_empty());
}

#[test]
fn recovers_json_object_inside_markdown_fence() {
    // Providers routinely wrap the extraction object in a ```json fence.
    // Structural fence-stripping recovers the embedded object — this is the
    // single most common real-world cause of otherwise-valid graph payloads
    // failing to parse.
    let normalized =
        parse_graph_extraction_output("```json\n{\"entities\":[],\"relations\":[]}\n```")
            .expect("fenced object must be recovered");

    assert!(normalized.entities.is_empty());
    assert!(normalized.relations.is_empty());
}

#[test]
fn drops_empty_candidates_and_structurally_invalid_relation_types() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "  ", "node_type": "entity", "aliases": [], "sub_type": null, "summary": "" },
            { "label": "Provider Delta", "node_type": "entity", "aliases": ["", " Delta Provider "], "sub_type": null, "summary": "provider" }
          ],
          "relations": [
            { "source_label": "Provider Delta", "target_label": "Knowledge Graph", "relation_type": "Builds On" },
            { "source_label": " ", "target_label": "Ignored", "relation_type": "mentions", "summary": "ignored" },
            { "source_label": "Provider Delta", "target_label": "Knowledge Graph", "relation_type": "builds_on", "summary": "provider builds on graph" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "Provider Delta");
    assert_eq!(normalized.entities[0].aliases, vec!["Delta Provider".to_string()]);
    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(normalized.relations[0].relation_type, "builds_on");
}

#[test]
fn repairs_utf8_mojibake_graph_candidate_strings() {
    fn latin1_mojibake(value: &str) -> String {
        value.as_bytes().iter().map(|byte| char::from(*byte)).collect()
    }

    let source_label = latin1_mojibake("\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}");
    let target_label = latin1_mojibake("\u{041f}\u{043e}\u{043b}\u{0435}");
    let subtype =
        latin1_mojibake("\u{043f}\u{0430}\u{0440}\u{0430}\u{043c}\u{0435}\u{0442}\u{0440}");
    let summary = latin1_mojibake(
        "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} — \u{043e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}",
    );
    let output = serde_json::json!({
        "entities": [
            {
                "label": source_label,
                "node_type": "attribute",
                "aliases": [target_label],
                "sub_type": subtype,
                "summary": summary
            }
        ],
        "relations": [
            {
                "source_label": latin1_mojibake("\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}"),
                "target_label": latin1_mojibake("\u{041f}\u{043e}\u{043b}\u{0435}"),
                "relation_type": "describes",
                "summary": latin1_mojibake("\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} — \u{043e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}")
            }
        ]
    });

    let normalized =
        parse_graph_extraction_output(&output.to_string()).expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}");
    assert_eq!(normalized.entities[0].aliases, vec!["\u{041f}\u{043e}\u{043b}\u{0435}"]);
    assert_eq!(
        normalized.entities[0].sub_type.as_deref(),
        Some("\u{043f}\u{0430}\u{0440}\u{0430}\u{043c}\u{0435}\u{0442}\u{0440}")
    );
    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some(
            "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} — \u{043e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}"
        )
    );
    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(
        normalized.relations[0].source_label,
        "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}"
    );
    assert_eq!(normalized.relations[0].target_label, "\u{041f}\u{043e}\u{043b}\u{0435}");
    assert_eq!(
        normalized.relations[0].summary.as_deref(),
        Some(
            "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} — \u{043e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}"
        )
    );
}

#[test]
fn repairs_provider_output_that_misdecoded_full_json_as_latin1() {
    fn latin1_mojibake(value: &str) -> String {
        value.as_bytes().iter().map(|byte| char::from(*byte)).collect()
    }

    let clean_output = serde_json::json!({
        "entities": [
            {
                "label": "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}",
                "node_type": "attribute",
                "aliases": [],
                "sub_type": "field_name",
                "summary": "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} — \u{043e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}"
            }
        ],
        "relations": [
            {
                "source_label": "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}",
                "target_label": "\u{041f}\u{043e}\u{043b}\u{0435}",
                "relation_type": "describes",
                "summary": "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} \u{043e}\u{043f}\u{0438}\u{0441}\u{044b}\u{0432}\u{0430}\u{0435}\u{0442} \u{043f}\u{043e}\u{043b}\u{0435}"
            }
        ]
    })
    .to_string();

    let normalized = parse_graph_extraction_output(&latin1_mojibake(&clean_output))
        .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}");
    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some(
            "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430} — \u{043e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}"
        )
    );
    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(
        normalized.relations[0].source_label,
        "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}"
    );
    assert_eq!(normalized.relations[0].target_label, "\u{041f}\u{043e}\u{043b}\u{0435}");
}

#[test]
fn repairs_escaped_live_mojibake_graph_candidate_strings() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            {
              "label": "\u00d0\u009f\u00d0\u00be\u00d0\u00bb\u00d0\u00b5: \u00d0\u0092\u00d1\u008b\u00d1\u0081\u00d0\u00ba\u00d0\u00b0\u00d0\u00ba\u00d0\u00b8\u00d0\u00b2\u00d0\u00b0\u00d1\u0082\u00d1\u008c",
              "node_type": "attribute",
              "aliases": [],
              "sub_type": "field_name",
              "summary": "\u00d0\u009d\u00d0\u00b0\u00d0\u00b7\u00d0\u00b2\u00d0\u00b0\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 \u00d0\u00bf\u00d0\u00be\u00d0\u00bb\u00d1\u008f"
            }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(
        normalized.entities[0].label,
        "\u{041f}\u{043e}\u{043b}\u{0435}: \u{0412}\u{044b}\u{0441}\u{043a}\u{0430}\u{043a}\u{0438}\u{0432}\u{0430}\u{0442}\u{044c}"
    );
    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some(
            "\u{041d}\u{0430}\u{0437}\u{0432}\u{0430}\u{043d}\u{0438}\u{0435} \u{043f}\u{043e}\u{043b}\u{044f}"
        )
    );
}

#[test]
fn repairs_live_mojibake_candidate_with_code_spans() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            {
              "label": "name",
              "node_type": "attribute",
              "aliases": [],
              "sub_type": "field_name",
              "summary": "\u00d0\u009f\u00d0\u00be\u00d0\u00bb\u00d0\u00b5 `name` \u00e2\u0080\u0094 \u00d1\u0081\u00d1\u0082\u00d1\u0080\u00d0\u00be\u00d0\u00ba\u00d0\u00be\u00d0\u00b2\u00d0\u00be\u00d0\u00b5 \u00d0\u00bf\u00d0\u00be\u00d0\u00bb\u00d0\u00b5."
            },
            {
              "label": "\u00d0\u00a1\u00d1\u0082\u00d1\u0080\u00d0\u00be\u00d0\u00ba\u00d0\u00b0",
              "node_type": "attribute",
              "aliases": [],
              "sub_type": "data_type",
              "summary": "\u00d0\u00a2\u00d0\u00b8\u00d0\u00bf \u00d0\u00b4\u00d0\u00b0\u00d0\u00bd\u00d0\u00bd\u00d1\u008b\u00d1\u0085 `\u00d0\u00a1\u00d1\u0082\u00d1\u0080\u00d0\u00be\u00d0\u00ba\u00d0\u00b0`."
            }
          ],
          "relations": [
            {
              "source_label": "name",
              "target_label": "\u00d0\u00a1\u00d1\u0082\u00d1\u0080\u00d0\u00be\u00d0\u00ba\u00d0\u00b0",
              "relation_type": "has_mode",
              "summary": "\u00d0\u009f\u00d0\u00be\u00d0\u00bb\u00d0\u00b5 `name` \u00d0\u00b8\u00d0\u00bc\u00d0\u00b5\u00d0\u00b5\u00d1\u0082 \u00d1\u0082\u00d0\u00b8\u00d0\u00bf \u00d0\u00b4\u00d0\u00b0\u00d0\u00bd\u00d0\u00bd\u00d1\u008b\u00d1\u0085."
            }
          ]
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 2);
    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some(
            "\u{041f}\u{043e}\u{043b}\u{0435} `name` — \u{0441}\u{0442}\u{0440}\u{043e}\u{043a}\u{043e}\u{0432}\u{043e}\u{0435} \u{043f}\u{043e}\u{043b}\u{0435}."
        )
    );
    assert_eq!(normalized.entities[1].label, "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}");
    assert_eq!(
        normalized.entities[1].summary.as_deref(),
        Some(
            "\u{0422}\u{0438}\u{043f} \u{0434}\u{0430}\u{043d}\u{043d}\u{044b}\u{0445} `\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}`."
        )
    );
    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(
        normalized.relations[0].target_label,
        "\u{0421}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}"
    );
    assert_eq!(
        normalized.relations[0].summary.as_deref(),
        Some(
            "\u{041f}\u{043e}\u{043b}\u{0435} `name` \u{0438}\u{043c}\u{0435}\u{0435}\u{0442} \u{0442}\u{0438}\u{043f} \u{0434}\u{0430}\u{043d}\u{043d}\u{044b}\u{0445}."
        )
    );
}

#[test]
fn repairs_live_mojibake_candidate_with_mixed_script_label() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            {
              "label": "\u00d0\u00a1\u00d0\u00be\u00d0\u00be\u00d0\u00b1\u00d1\u0089\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 TransferCallReturned",
              "node_type": "artifact",
              "aliases": [],
              "sub_type": "message",
              "summary": "\u00d0\u00a1\u00d0\u00be\u00d0\u00be\u00d0\u00b1\u00d1\u0089\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 TransferCallReturned, \u00d1\u0083\u00d0\u00ba\u00d0\u00b0\u00d0\u00b7\u00d0\u00b0\u00d0\u00bd\u00d0\u00bd\u00d0\u00be\u00d0\u00b5 \u00d0\u00b2 \u00d0\u00bf\u00d0\u00b5\u00d1\u0080\u00d0\u00b5\u00d1\u0087\u00d0\u00bd\u00d0\u00b5."
            },
            {
              "label": "\u00d0\u00a1\u00d0\u00be\u00d0\u00be\u00d0\u00b1\u00d1\u0089\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 TransferSucceed",
              "node_type": "artifact",
              "aliases": [],
              "sub_type": "message",
              "summary": "\u00d0\u00a1\u00d0\u00be\u00d0\u00be\u00d0\u00b1\u00d1\u0089\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 TransferSucceed."
            }
          ],
          "relations": [
            {
              "source_label": "\u00d0\u00a1\u00d0\u00be\u00d0\u00be\u00d0\u00b1\u00d1\u0089\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 TransferCallReturned",
              "target_label": "\u00d0\u00a1\u00d0\u00be\u00d0\u00be\u00d0\u00b1\u00d1\u0089\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 TransferSucceed",
              "relation_type": "followed_by",
              "summary": "\u00d0\u0092 \u00d1\u0082\u00d0\u00b5\u00d0\u00ba\u00d1\u0081\u00d1\u0082\u00d0\u00b5 \u00d0\u00bf\u00d0\u00b5\u00d1\u0080\u00d0\u00b5\u00d1\u0087\u00d0\u00b8\u00d1\u0081\u00d0\u00bb\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5."
            }
          ]
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 2);
    assert_eq!(
        normalized.entities[0].label,
        "\u{0421}\u{043e}\u{043e}\u{0431}\u{0449}\u{0435}\u{043d}\u{0438}\u{0435} TransferCallReturned"
    );
    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some(
            "\u{0421}\u{043e}\u{043e}\u{0431}\u{0449}\u{0435}\u{043d}\u{0438}\u{0435} TransferCallReturned, \u{0443}\u{043a}\u{0430}\u{0437}\u{0430}\u{043d}\u{043d}\u{043e}\u{0435} \u{0432} \u{043f}\u{0435}\u{0440}\u{0435}\u{0447}\u{043d}\u{0435}."
        )
    );
    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(
        normalized.relations[0].source_label,
        "\u{0421}\u{043e}\u{043e}\u{0431}\u{0449}\u{0435}\u{043d}\u{0438}\u{0435} TransferCallReturned"
    );
    assert_eq!(
        normalized.relations[0].target_label,
        "\u{0421}\u{043e}\u{043e}\u{0431}\u{0449}\u{0435}\u{043d}\u{0438}\u{0435} TransferSucceed"
    );
    assert_eq!(
        normalized.relations[0].summary.as_deref(),
        Some(
            "\u{0412} \u{0442}\u{0435}\u{043a}\u{0441}\u{0442}\u{0435} \u{043f}\u{0435}\u{0440}\u{0435}\u{0447}\u{0438}\u{0441}\u{043b}\u{0435}\u{043d}\u{0438}\u{0435}."
        )
    );
}

#[test]
fn normalized_json_drops_unrepairable_encoding_damage() {
    let normalized = GraphExtractionCandidateSet {
        entities: vec![GraphEntityCandidate {
            label: "\u{0081}".to_string(),
            node_type: RuntimeNodeType::Attribute,
            sub_type: None,
            aliases: Vec::new(),
            summary: Some("valid".to_string()),
        }],
        relations: Vec::new(),
    };

    let repaired = normalized_graph_extraction_json(normalized);

    assert_eq!(repaired, serde_json::json!({ "entities": [], "relations": [] }));
}

#[test]
fn repairs_ascii_prefixed_mojibake_graph_candidate_strings() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            {
              "label": "3.2.5. \u00d0\u009e\u00d0\u00b1\u00d0\u00bd\u00d0\u00be\u00d0\u00b2\u00d0\u00bb\u00d0\u00b5\u00d0\u00bd\u00d0\u00b8\u00d0\u00b5 \u00d1\u0081\u00d0\u00bf\u00d0\u00b8\u00d1\u0081\u00d0\u00ba\u00d0\u00b0",
              "node_type": "process",
              "aliases": [],
              "sub_type": null,
              "summary": "\u00d0\u00bf\u00d1\u0080\u00d0\u00be\u00d1\u0086\u00d0\u00b5\u00d1\u0081\u00d1\u0081"
            }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(
        normalized.entities[0].label,
        "3.2.5. \u{041e}\u{0431}\u{043d}\u{043e}\u{0432}\u{043b}\u{0435}\u{043d}\u{0438}\u{0435} \u{0441}\u{043f}\u{0438}\u{0441}\u{043a}\u{0430}"
    );
    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some("\u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}")
    );
}

#[test]
fn repairs_mojibake_graph_summaries_with_ascii_prefixes() {
    fn latin1_mojibake(value: &str) -> String {
        value.as_bytes().iter().map(|byte| char::from(*byte)).collect()
    }

    let output = serde_json::json!({
        "entities": [
            {
                "label": "ExampleTool",
                "node_type": "artifact",
                "aliases": [],
                "sub_type": null,
                "summary": latin1_mojibake("ExampleTool — \u{0421}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{0430}\u{044f} \u{0441}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}.")
            },
            {
                "label": latin1_mojibake("\u{0421}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{0438}\u{0439} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}"),
                "node_type": "process",
                "aliases": [],
                "sub_type": null,
                "summary": latin1_mojibake("\u{041e}\u{043f}\u{0438}\u{0441}\u{0430}\u{043d}\u{0438}\u{0435} \u{0441}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{043e}\u{0433}\u{043e} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}\u{0430}.")
            }
        ],
        "relations": [
            {
                "source_label": "ExampleTool",
                "target_label": latin1_mojibake("\u{0421}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{0438}\u{0439} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}"),
                "relation_type": "describes",
                "summary": latin1_mojibake("ExampleTool — \u{043e}\u{043f}\u{0438}\u{0441}\u{044b}\u{0432}\u{0430}\u{0435}\u{0442} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}.")
            }
        ]
    });

    let normalized =
        parse_graph_extraction_output(&output.to_string()).expect("parse graph extraction");

    assert_eq!(
        normalized.entities[0].summary.as_deref(),
        Some(
            "ExampleTool — \u{0421}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{0430}\u{044f} \u{0441}\u{0442}\u{0440}\u{043e}\u{043a}\u{0430}."
        )
    );
    assert_eq!(
        normalized.entities[1].label,
        "\u{0421}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{0438}\u{0439} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}"
    );
    assert_eq!(
        normalized.relations[0].target_label,
        "\u{0421}\u{0438}\u{043d}\u{0442}\u{0435}\u{0442}\u{0438}\u{0447}\u{0435}\u{0441}\u{043a}\u{0438}\u{0439} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}"
    );
    assert_eq!(
        normalized.relations[0].summary.as_deref(),
        Some(
            "ExampleTool — \u{043e}\u{043f}\u{0438}\u{0441}\u{044b}\u{0432}\u{0430}\u{0435}\u{0442} \u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}."
        )
    );
}

#[test]
fn repairs_mojibake_normalized_json_payloads() {
    let repaired = repair_graph_extraction_normalized_json(serde_json::json!({
        "entities": [
            {
                "label": "3.2.5. \u{00d0}\u{009e}\u{00d0}\u{00b1}\u{00d0}\u{00bd}\u{00d0}\u{00be}\u{00d0}\u{00b2}\u{00d0}\u{00bb}\u{00d0}\u{00b5}\u{00d0}\u{00bd}\u{00d0}\u{00b8}\u{00d0}\u{00b5}",
                "node_type": "process",
                "aliases": [],
                "sub_type": null,
                "summary": "\u{00d0}\u{00bf}\u{00d1}\u{0080}\u{00d0}\u{00be}\u{00d1}\u{0086}\u{00d0}\u{00b5}\u{00d1}\u{0081}\u{00d1}\u{0081}"
            }
        ],
        "relations": []
    }));

    assert_eq!(
        repaired["entities"][0]["label"],
        serde_json::Value::String("3.2.5. \u{041e}\u{0431}\u{043d}\u{043e}\u{0432}\u{043b}\u{0435}\u{043d}\u{0438}\u{0435}".to_string())
    );
    assert_eq!(
        repaired["entities"][0]["summary"],
        serde_json::Value::String(
            "\u{043f}\u{0440}\u{043e}\u{0446}\u{0435}\u{0441}\u{0441}".to_string()
        )
    );
}

#[test]
fn keeps_valid_non_ascii_graph_candidate_strings() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "Café metric", "node_type": "attribute", "aliases": ["naïve score"], "sub_type": null, "summary": "Café metric" }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "Café metric");
    assert_eq!(normalized.entities[0].aliases, vec!["naïve score"]);
}

#[test]
fn keeps_valid_latin1_supplement_pairs_without_repair() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [
            { "label": "Â£ rate", "node_type": "attribute", "aliases": ["Â£"], "sub_type": null, "summary": "Â£ rate" }
          ],
          "relations": []
        }"#,
    )
    .expect("parse graph extraction");

    assert_eq!(normalized.entities.len(), 1);
    assert_eq!(normalized.entities[0].label, "Â£ rate");
    assert_eq!(normalized.entities[0].aliases, vec!["Â£"]);
}

#[test]
fn keeps_arbitrary_structurally_valid_relation_types_at_parse_time() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [],
          "relations": [
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "opaque_predicate_7" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(normalized.relations[0].relation_type, "opaque_predicate_7");
}

#[test]
fn drops_non_ascii_relation_types_at_parse_time() {
    let normalized = parse_graph_extraction_output(
        r#"{
          "entities": [],
          "relations": [
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "περιέχει" },
            { "source_label": "Alpha", "target_label": "Beta", "relation_type": "supports" }
          ]
        }"#,
    )
    .expect("normalize graph extraction");

    assert_eq!(normalized.relations.len(), 1);
    assert_eq!(normalized.relations[0].relation_type, "supports");
}

#[test]
fn rejects_non_json_payloads() {
    let error = parse_graph_extraction_output("not valid json").expect_err("invalid json");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn recovers_json_object_surrounded_by_prose() {
    // Recovery is purely structural — it scans for the first balanced JSON
    // object and never inspects the surrounding text, so it stays
    // language-agnostic. Here the embedded object is recovered; the bare
    // string entity has no `label` field and is dropped, yielding an empty set.
    let normalized = parse_graph_extraction_output(
        "Here is the result:\n{\"entities\":[\"Provider Alpha\"],\"relations\":[]}\nThanks.",
    )
    .expect("embedded object must be recovered");

    assert!(normalized.entities.is_empty());
    assert!(normalized.relations.is_empty());
}

#[test]
fn rejects_json5_style_payloads() {
    let error = parse_graph_extraction_output(
        "{entities:[{label:'Provider Alpha', node_type:'entity', aliases:['Alpha Provider'], summary:'provider',},], relations:[]}",
    )
    .expect_err("json5 payload must fail");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn rejects_truncated_json_payloads() {
    let error = parse_graph_extraction_output(
        r#"{"entities":[{"label":"Provider Alpha","node_type":"entity","aliases":[],"summary":"provider"}],"relations":[{"source_label":"Provider Alpha","target_label":"Graph","relation_type":"mentions","summary":"link"}"#,
    )
    .expect_err("truncated payload must fail");

    assert!(error.to_string().contains("invalid graph extraction json"));
}

#[test]
fn rejects_named_sections_without_outer_object() {
    let error = normalize_graph_extraction_output(
        r#"
        entities:
        [{"label":"Provider Alpha","node_type":"entity","aliases":[],"summary":"provider"}]
        relations:
        [{"source_label":"Provider Alpha","target_label":"Annual report","relation_type":"mentions","summary":"citation"}]
        "#,
    )
    .expect_err("named sections must fail");

    assert!(error.parse_error.contains("malformed_output"));
}

#[test]
fn sanitizes_low_confidence_graph_candidates_from_unstable_source_text() {
    let candidates = GraphExtractionCandidateSet {
        entities: vec![GraphEntityCandidate {
            label: "aBcD3eFgH".to_string(),
            node_type: RuntimeNodeType::Entity,
            sub_type: None,
            aliases: Vec::new(),
            summary: Some("qWeR7tYuI zXcV9bNmP lMnO4pQrS tUvW6xYzA".to_string()),
        }],
        relations: vec![GraphRelationCandidate {
            source_label: "aBcD3eFgH".to_string(),
            target_label: "qWeR7tYuI".to_string(),
            relation_type: "mentions".to_string(),
            summary: Some("zXcV9bNmP lMnO4pQrS tUvW6xYzA aBcD3eFgH".to_string()),
        }],
    };

    let sanitized = sanitize_graph_extraction_candidate_set(
        candidates,
        "aBcD3eFgH qWeR7tYuI zXcV9bNmP lMnO4pQrS tUvW6xYzA",
    );

    assert!(sanitized.entities.is_empty());
    assert!(sanitized.relations.is_empty());
}

#[test]
fn sanitizes_unstable_graph_labels_without_dropping_camel_case_labels() {
    let candidates = GraphExtractionCandidateSet {
        entities: vec![
            GraphEntityCandidate {
                label: "qWeR7tYuI".to_string(),
                node_type: RuntimeNodeType::Entity,
                sub_type: None,
                aliases: Vec::new(),
                summary: None,
            },
            GraphEntityCandidate {
                label: "renderHTMLNode".to_string(),
                node_type: RuntimeNodeType::Entity,
                sub_type: None,
                aliases: vec!["parseHTTPResponse".to_string()],
                summary: None,
            },
        ],
        relations: vec![
            GraphRelationCandidate {
                source_label: "qWeR7tYuI".to_string(),
                target_label: "renderHTMLNode".to_string(),
                relation_type: "mentions".to_string(),
                summary: None,
            },
            GraphRelationCandidate {
                source_label: "renderHTMLNode".to_string(),
                target_label: "parseHTTPResponse".to_string(),
                relation_type: "mentions".to_string(),
                summary: None,
            },
        ],
    };

    let sanitized = sanitize_graph_extraction_candidate_set(
        candidates,
        "The renderHTMLNode utility calls parseHTTPResponse while building a response view.",
    );

    assert_eq!(sanitized.entities.len(), 1);
    assert_eq!(sanitized.entities[0].label, "renderHTMLNode");
    assert_eq!(sanitized.entities[0].aliases, vec!["parseHTTPResponse"]);
    assert_eq!(sanitized.relations.len(), 1);
    assert_eq!(sanitized.relations[0].source_label, "renderHTMLNode");
    assert_eq!(sanitized.relations[0].target_label, "parseHTTPResponse");
}
