use super::*;
use crate::domains::query_ir::{
    DocumentHint, EntityMention, EntityRole, QueryLanguage, QueryScope, SourceSliceSpec,
    UnresolvedRef,
};
use crate::services::query::execution::RuntimeChunkScoreKind;

#[test]
fn module_configuration_setup_answer_prefers_package_owned_config_path() {
    let target_document_id = Uuid::now_v7();
    let distractor_document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(
            distractor_document_id,
            0,
            "Widget Beta setup",
            r#"
Install the module:
sample-install beta-widget

Settings are stored in /opt/beta/display/display.ini.

| settingOne | string | Wrong setting |
"#,
        ),
        runtime_chunk(
            target_document_id,
            1,
            "Widget Alpha setup",
            r#"
Install the module:
sample-install alpha-connector

Configure the module:
sample-configure alpha-connector

Connector settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
"#,
        ),
        runtime_chunk(
            target_document_id,
            2,
            "Widget Alpha setup",
            r#"
| settingOne | string | First connector setting |
| settingTwo | string | Second connector setting |
| settingThree | string | Third connector setting |

Display settings use /opt/alpha/display/display.ini.
"#,
        ),
    ];
    let answer = build_module_configuration_setup_answer(
        "Configure Widget Alpha",
        &configuration_setup_ir(),
        &empty_evidence(),
        &chunks,
    )
    .expect("setup answer");

    assert!(answer.contains("`alpha-connector`"));
    assert!(answer.contains("`/opt/alpha/modules/connector/connector.conf`"));
    assert!(answer.contains("`settingOne`"));
    assert!(answer.contains("`settingTwo`"));
    // The package-owned config path must be selected as the primary, i.e.
    // rendered ahead of the unrelated display-settings path (which may still
    // surface as an additional bullet).
    let primary = answer
        .find("`/opt/alpha/modules/connector/connector.conf`")
        .expect("package-owned config path present");
    if let Some(display) = answer.find("`/opt/alpha/display/display.ini`") {
        assert!(primary < display, "package-owned config path must precede the display path");
    }
    assert!(!answer.contains("`beta-widget`"));
}

#[test]
fn module_configuration_setup_answer_reads_structured_table_rows() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(
            document_id,
            1,
            "Widget Gamma setup",
            r#"
Install the module:
sample-install gamma-connector

Configure the module:
sample-configure gamma-connector

Connector settings are stored in /opt/gamma/modules/connector/connector.conf under [Main].
"#,
        ),
        runtime_chunk(
            document_id,
            2,
            "Widget Gamma setup",
            "Sheet: Connector settings | Row 1 | Name: endpointUrl | Type: string | Description: Service endpoint",
        ),
        runtime_chunk(
            document_id,
            3,
            "Widget Gamma setup",
            "Sheet: Connector settings | Row 2 | Name: partnerId | Type: string | Description: Partner identifier",
        ),
        runtime_chunk(
            document_id,
            4,
            "Widget Gamma setup",
            "Sheet: Connector settings | Row 3 | Name: secretKey | Type: string | Description: Shared secret",
        ),
    ];
    let mut query_ir = configuration_setup_ir();
    query_ir.literal_constraints = vec![literal_constraint("secret"), literal_constraint("id")];

    let answer = build_module_configuration_setup_answer(
        "Configure Widget Gamma",
        &query_ir,
        &empty_evidence(),
        &chunks,
    )
    .expect("setup answer");
    let partner_pos = answer.find("`partnerId`").expect("partnerId row");
    let secret_pos = answer.find("`secretKey`").expect("secretKey row");
    let endpoint_pos = answer.find("`endpointUrl`").expect("endpointUrl row");

    assert!(answer.contains("`/opt/gamma/modules/connector/connector.conf`"));
    assert!(partner_pos < endpoint_pos);
    assert!(secret_pos < endpoint_pos);
}

#[test]
fn module_configuration_setup_answer_prefers_parameter_rich_setup_document() {
    let release_document_id = Uuid::now_v7();
    let setup_document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(
            release_document_id,
            0,
            "Widget Alpha release note",
            r#"
Release note:
sample-install alpha-connector
Configuration file: /opt/alpha/modules/connector/connector.conf
"#,
        ),
        runtime_chunk(
            setup_document_id,
            1,
            "Widget Alpha administrator guide",
            r#"
Install the module:
sample-install alpha-connector

Connector settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
"#,
        ),
        runtime_chunk(
            setup_document_id,
            2,
            "Widget Alpha administrator guide",
            "Sheet: Connector settings | Row 1 | Name: partnerId | Type: string | Description: Partner identifier",
        ),
        runtime_chunk(
            setup_document_id,
            3,
            "Widget Alpha administrator guide",
            "Sheet: Connector settings | Row 2 | Name: secretKey | Type: string | Description: Shared secret",
        ),
    ];

    let answer = build_module_configuration_setup_answer(
        "Configure Widget Alpha",
        &configuration_setup_ir(),
        &empty_evidence(),
        &chunks,
    )
    .expect("setup answer");

    assert!(answer.contains("`Widget Alpha administrator guide`"));
    assert!(answer.contains("`partnerId`"));
    assert!(answer.contains("`secretKey`"));
    assert!(!answer.contains("`Widget Alpha release note`"));
}

#[test]
fn module_configuration_setup_answer_requires_typed_configuration_target() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(document_id, 0, "Provider Alpha setup", "Overview of Provider Alpha."),
        runtime_chunk(
            document_id,
            1,
            "Provider Alpha setup",
            r#"
To use the module, install it with sample-install alpha-connector and run sample-configure alpha-connector.

The module configuration file is /opt/alpha/modules/connector/connector.conf.
| endpointUrl | string | Service endpoint |
| partnerId | string | Partner identifier |
"#,
        ),
    ];
    let mut query_ir = configuration_setup_ir();
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Procedure];
    query_ir.document_focus = Some(DocumentHint { hint: "Provider Alpha".to_string() });

    let answer = build_module_configuration_setup_answer(
        "How do I configure Provider Alpha?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn module_configuration_setup_answer_abstains_for_untyped_low_confidence_ir() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(
            document_id,
            1,
            "Provider Alpha setup",
            r#"
Install the module:
sample-install alpha-connector

The module configuration file is /opt/alpha/modules/connector/connector.conf.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
"#,
        ),
        runtime_chunk(
            document_id,
            2,
            "Provider Alpha setup",
            r#"
| endpointUrl | string | Service endpoint |
| partnerId | string | Partner identifier |
| visible | boolean | true false | Display the code |
"#,
        ),
    ];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "Provider Alpha setup `/opt/alpha/modules/connector/connector.conf` `endpointUrl`",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn low_confidence_untyped_ir_requires_query_anchor_before_setup_answer() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        1,
        "Provider Alpha setup",
        r#"
Install the module:
sample-install alpha-connector

The module configuration file is /opt/alpha/modules/connector/connector.conf.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "Provider Alpha terminal loses payment confirmation",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn low_confidence_untyped_ir_does_not_turn_unmatched_config_evidence_into_setup_answer() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        1,
        "Provider Beta setup",
        r#"
Install the module:
sample-install beta-connector

The module configuration file is /opt/beta/modules/connector/connector.conf.
[Main]
endpointUrl = https://beta.example/api
partnerId = beta-partner
visible = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "Provider Alpha operational troubleshooting",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn low_confidence_untyped_ir_requires_shared_code_for_weak_label_overlap() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        1,
        "Cash link setup",
        r#"
Install the module:
sample-install cash-link

The module configuration file is /opt/cash/link/link.conf.
[Main]
endpointUrl = https://cash.example/api
partnerId = cash-partner
visible = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "PAY cash link troubleshooting",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn low_confidence_untyped_ir_abstains_despite_shared_config_literals() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        1,
        "PAY cash link setup",
        r#"
Install the module:
sample-install cash-link

The module configuration file is /opt/cash/link/link.conf.
[Main]
endpointUrl = https://cash.example/api
partnerId = cash-partner
visible = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types.clear();
    query_ir.target_entities.clear();
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "PAY cash link setup `/opt/cash/link/link.conf` `visible`",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn low_confidence_structural_ir_abstains_even_for_matching_config_evidence() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        1,
        "Provider Alpha setup",
        r#"
Install the module:
sample-install alpha-connector

The module configuration file is /opt/alpha/modules/connector/connector.conf.
[Main]
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
visible = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.target_types.clear();
    query_ir.target_entities =
        vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "Provider Alpha settings",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn low_confidence_structural_ir_rejects_unmatched_config_evidence() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        1,
        "Provider Beta setup",
        r#"
Install the module:
sample-install beta-connector

The module configuration file is /opt/beta/modules/connector/connector.conf.
[Main]
endpointUrl = https://beta.example/api
partnerId = beta-partner
visible = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.target_types.clear();
    query_ir.target_entities =
        vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];
    query_ir.literal_constraints.clear();
    query_ir.temporal_constraints.clear();
    query_ir.document_focus = None;
    query_ir.source_slice = None;
    query_ir.conversation_refs.clear();
    query_ir.confidence = 0.25;

    let answer = build_module_configuration_setup_answer(
        "Provider Alpha settings",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(answer.is_none(), "{answer:?}");
}

#[test]
fn module_configuration_setup_answer_adds_structured_rows_for_focused_document() {
    let setup_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![
        runtime_chunk(
            setup_document_id,
            0,
            "Provider Delta setup",
            r#"
Install the module:
sample-install delta-connector

The module configuration file is /opt/delta/modules/connector/connector.conf.
| endpointUrl | string | Service endpoint |
"#,
        ),
        runtime_chunk(
            setup_document_id,
            1,
            "Provider Delta setup",
            "Table Summary | Sheet: Connector settings | Row Count: 12",
        ),
    ];
    let mut block = crate::services::query::execution::types::sample_structured_block_row(
        Uuid::now_v7(),
        setup_document_id,
        revision_id,
    );
    block.ordinal = 12;
    block.text =
        "Sheet: Connector settings | Row 12 | Name: fillDetails | Type: boolean | Description: Send detailed payload"
            .to_string();
    block.normalized_text = block.text.clone();
    let mut unrelated_block = crate::services::query::execution::types::sample_structured_block_row(
        Uuid::now_v7(),
        other_document_id,
        Uuid::now_v7(),
    );
    unrelated_block.text =
        "Sheet: Other settings | Row 1 | Name: unrelatedSecret | Type: string".to_string();
    unrelated_block.normalized_text = unrelated_block.text.clone();
    let evidence = evidence_with_blocks(vec![block, unrelated_block]);

    let answer = build_module_configuration_setup_answer(
        "Configure Provider Delta parameters",
        &configuration_setup_ir(),
        &evidence,
        &chunks,
    )
    .expect("setup answer");

    assert!(answer.contains("`endpointUrl`"));
    assert!(answer.contains("`fillDetails`"));
    assert!(!answer.contains("- ``"));
    assert!(!answer.contains("`unrelatedSecret`"));
}

#[test]
fn module_configuration_setup_answer_reads_structured_paths_and_packages() {
    let setup_document_id = Uuid::now_v7();
    let other_document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        setup_document_id,
        0,
        "Provider Epsilon setup",
        "Overview for Provider Epsilon connector settings.",
    )];
    let mut install_block = crate::services::query::execution::types::sample_structured_block_row(
        Uuid::now_v7(),
        setup_document_id,
        revision_id,
    );
    install_block.ordinal = 1;
    install_block.text =
        "Install the module:\nsample-install epsilon-connector\n\nConfigure it:\nsample-configure epsilon-connector"
            .to_string();
    install_block.normalized_text = install_block.text.clone();
    let mut path_block = crate::services::query::execution::types::sample_structured_block_row(
        Uuid::now_v7(),
        setup_document_id,
        revision_id,
    );
    path_block.ordinal = 2;
    path_block.text =
        "The module configuration file is /opt/epsilon/modules/connector/connector.conf. Receipt display uses /opt/epsilon/receipt/receipt.ini."
            .to_string();
    path_block.normalized_text = path_block.text.clone();
    let mut parameter_block = crate::services::query::execution::types::sample_structured_block_row(
        Uuid::now_v7(),
        setup_document_id,
        revision_id,
    );
    parameter_block.ordinal = 3;
    parameter_block.text =
        "Sheet: Connector settings | Row 1 | Name: endpointUrl | Type: string | Description: Service endpoint"
            .to_string();
    parameter_block.normalized_text = parameter_block.text.clone();
    let mut unrelated_block = crate::services::query::execution::types::sample_structured_block_row(
        Uuid::now_v7(),
        other_document_id,
        Uuid::now_v7(),
    );
    unrelated_block.text =
        "Install unrelated module with `sample-install omega-connector`; file /opt/omega/omega.conf"
            .to_string();
    unrelated_block.normalized_text = unrelated_block.text.clone();
    let evidence =
        evidence_with_blocks(vec![install_block, path_block, parameter_block, unrelated_block]);

    let answer = build_module_configuration_setup_answer(
        "Configure Provider Epsilon connector",
        &configuration_setup_ir(),
        &evidence,
        &chunks,
    )
    .expect("setup answer");

    assert!(answer.contains("`epsilon-connector`"));
    assert!(answer.contains("`/opt/epsilon/modules/connector/connector.conf`"));
    assert!(answer.contains("`/opt/epsilon/receipt/receipt.ini`"));
    assert!(answer.contains("`endpointUrl`"));
    assert!(!answer.contains("- ``"));
    assert!(!answer.contains("omega-connector"));
    assert!(!answer.contains("/opt/omega/omega.conf"));
}

#[test]
fn exact_technical_literal_answer_abstains_for_untyped_entity_only_fallback_ir() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "Provider Alpha setup",
        r#"
Install the module:
sample-install alpha-connector

Settings are stored in /opt/alpha/modules/connector/connector.conf under [Main].
endpointUrl = https://alpha.example/api
partnerId = alpha-partner
"#,
    )];
    let evidence = empty_evidence();
    let mut low_confidence_ir = configuration_setup_ir();
    low_confidence_ir.act = QueryAct::Describe;
    low_confidence_ir.target_types.clear();
    low_confidence_ir.target_entities =
        vec![EntityMention { label: "Provider Alpha".to_string(), role: EntityRole::Subject }];
    low_confidence_ir.confidence = 0.25;

    assert!(
        build_exact_technical_literal_answer(
            "What operational limits apply to Provider Alpha?",
            &low_confidence_ir,
            &evidence,
            &chunks,
        )
        .is_none(),
        "entity-only provider-free fallback IR must not turn setup literals into a final operational answer"
    );

    let typed_ir = configuration_setup_ir();
    let answer = build_exact_technical_literal_answer(
        "How do I configure Provider Alpha?",
        &typed_ir,
        &evidence,
        &chunks,
    )
    .expect("typed configuration IR should still allow deterministic literal answer");
    assert!(answer.contains("`alpha-connector`"), "{answer}");
}

#[test]
fn module_configuration_setup_answer_abstains_for_port_inventory_question() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "sample-manifest.yaml",
        r#"
services:
  api:
environment:
  PORT: 8001
  apiPort = 8001
ports:
  - "8001:8001"
  postgres:
postgresPort = 5432
ports:
  - "5432:5432"
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
        crate::domains::query_ir::QueryTargetKind::Service,
        crate::domains::query_ir::QueryTargetKind::Port,
    ];
    query_ir.target_entities =
        vec![EntityMention { label: "Sample Manifest".to_string(), role: EntityRole::Subject }];

    let answer = build_module_configuration_setup_answer(
        "What ports do the Sample Manifest services expose?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "service/port inventory questions should use synthesis over source coverage, not setup field rendering: {answer:?}"
    );
    let exact_answer = build_exact_technical_literal_answer(
        "What ports do the Sample Manifest services expose?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );
    assert!(
        exact_answer.is_none(),
        "service/port inventory questions should not use exact assignment rendering: {exact_answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_requires_assignment_evidence() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "checkout_service_notes.md",
        "The checkout service accepts HTTPS traffic and calls the inventory service on port 9443.",
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Port,
        crate::domains::query_ir::QueryTargetKind::Connection,
    ];

    let answer = build_transport_config_assignment_answer(
        "Which ports and connections does the checkout service require?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "transport assignment rendering requires concrete config assignments: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_abstains_for_compound_port_inventory() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "alpha_records.txt",
        r#"
entity.alpha = alpha
entity.beta = beta
entity.updated_at = now()
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
        crate::domains::query_ir::QueryTargetKind::Port,
        crate::domains::query_ir::QueryTargetKind::Procedure,
    ];
    query_ir.target_entities =
        vec![EntityMention { label: "Alpha Records".to_string(), role: EntityRole::Subject }];

    let answer = build_transport_config_assignment_answer(
        "Which port values does Alpha Records expose?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "compound port inventory should not be answered by assignment-shaped rows: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_abstains_for_broad_protocol_explanation() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "neighboring_config.txt",
        r#"
service.endpoint = https://example.invalid:9443
service.timeout = 30
service.enabled = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::Describe;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Protocol,
        crate::domains::query_ir::QueryTargetKind::Concept,
    ];

    let answer = build_transport_config_assignment_answer(
        "What are the main improvements of Protocol X version 2 over version 1?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "broad protocol/concept questions should be synthesized from evidence, not rendered as config assignments: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_answer_requires_connection_or_configuration_target() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_chunk(
        document_id,
        0,
        "neighboring_config.txt",
        r#"
listener.protocol = alpha
listener.timeout = 30
listener.enabled = true
"#,
    )];
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Protocol];

    let answer = build_transport_config_assignment_answer(
        "Which protocol is described?",
        &query_ir,
        &empty_evidence(),
        &chunks,
    );

    assert!(
        answer.is_none(),
        "a protocol-only value lookup must not infer a config-assignment answer without a connection/config target: {answer:?}"
    );
}

#[test]
fn transport_config_assignment_ranks_typed_facts_above_field_name_spelling() {
    let distractor_document_id = Uuid::now_v7();
    let target_document_id = Uuid::now_v7();
    let distractor = runtime_chunk(
        distractor_document_id,
        0,
        "first_source.cfg",
        r#"
port = 9000
url = https://distractor.invalid:9000
timeout = 30
"#,
    );
    let target = runtime_chunk(
        target_document_id,
        0,
        "typed_source.cfg",
        r#"
keyAlpha = https://target.invalid:9443
keyBeta = 9443
"#,
    );
    let mut url_fact = crate::services::query::execution::sample_technical_fact_row(
        Uuid::now_v7(),
        target.document_id,
        target.revision_id,
    );
    url_fact.fact_kind = "url".to_string();
    url_fact.display_value = "https://target.invalid:9443".to_string();
    url_fact.canonical_value_text = url_fact.display_value.clone();
    url_fact.canonical_value_exact = url_fact.display_value.clone();
    url_fact.support_chunk_ids = vec![target.chunk_id];
    let mut port_fact = crate::services::query::execution::sample_technical_fact_row(
        Uuid::now_v7(),
        target.document_id,
        target.revision_id,
    );
    port_fact.fact_kind = "port".to_string();
    port_fact.display_value = "9443".to_string();
    port_fact.canonical_value_text = port_fact.display_value.clone();
    port_fact.canonical_value_exact = port_fact.display_value.clone();
    port_fact.support_chunk_ids = vec![target.chunk_id];
    let evidence = CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: Vec::new(),
        technical_facts: vec![url_fact, port_fact],
    };
    let mut query_ir = configuration_setup_ir();
    query_ir.act = QueryAct::RetrieveValue;
    query_ir.target_types = vec![
        crate::domains::query_ir::QueryTargetKind::Connection,
        crate::domains::query_ir::QueryTargetKind::Url,
        crate::domains::query_ir::QueryTargetKind::Port,
    ];

    let answer = build_exact_technical_literal_answer(
        "Return the configured transport values.",
        &query_ir,
        &evidence,
        &[distractor, target],
    )
    .expect("typed assignment answer");

    assert!(answer.contains("`typed_source.cfg`"), "{answer}");
    assert!(!answer.contains("`first_source.cfg`"), "{answer}");
}

fn configuration_setup_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::En,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Package,
            crate::domains::query_ir::QueryTargetKind::ConfigurationFile,
            crate::domains::query_ir::QueryTargetKind::Parameter,
        ],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::<UnresolvedRef>::new(),
        needs_clarification: None,
        source_slice: Option::<SourceSliceSpec>::None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

fn literal_constraint(text: &str) -> crate::domains::query_ir::LiteralSpan {
    crate::domains::query_ir::LiteralSpan {
        kind: crate::domains::query_ir::LiteralKind::Identifier,
        text: text.to_string(),
    }
}

fn empty_evidence() -> CanonicalAnswerEvidence {
    evidence_with_blocks(Vec::new())
}

fn evidence_with_blocks(blocks: Vec<KnowledgeStructuredBlockRow>) -> CanonicalAnswerEvidence {
    CanonicalAnswerEvidence {
        bundle: None,
        chunk_rows: Vec::new(),
        structured_blocks: blocks,
        technical_facts: Vec::new(),
    }
}

fn runtime_chunk(document_id: Uuid, index: i32, label: &str, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: index,
        chunk_kind: None,
        document_label: label.to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: text.to_string(),
    }
}
