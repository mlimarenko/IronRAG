use uuid::Uuid;

use super::{
    build_update_procedure_sequence_answer, line_has_command_signal, split_dense_procedure_line,
    update_procedure_command_heads_match, update_procedure_line_blocks,
};
use crate::{
    domains::query_ir::{
        DocumentHint, EntityMention, EntityRole, QueryAct, QueryIR, QueryLanguage, QueryScope,
    },
    services::query::execution::{RuntimeChunkScoreKind, RuntimeMatchedChunk},
};

fn configure_update_focus_ir(focus: &str) -> QueryIR {
    QueryIR {
        act: QueryAct::ConfigureHow,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Artifact,
            crate::domains::query_ir::QueryTargetKind::Procedure,
        ],
        target_entities: vec![EntityMention {
            label: focus.to_string(),
            role: EntityRole::Subject,
        }],
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: Some(DocumentHint { hint: focus.to_string() }),
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    }
}

fn evidence_chunk(index: i32, kind: Option<&str>, text: &str) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: index,
        chunk_kind: kind.map(str::to_string),
        document_label: "records.jsonl".to_string(),
        excerpt: text.to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(3.0),
        source_text: text.to_string(),
    }
}

#[test]
fn dense_procedure_line_splits_structurally_signaled_commands() {
    assert_eq!(
        split_dense_procedure_line(
            "Sample Target lifecycle: runner-shell --scope=elevated fetch-tool https://example.invalid/sample/update-main --output=/work/update-token",
        ),
        vec![
            "Sample Target lifecycle:",
            "runner-shell --scope=elevated",
            "fetch-tool https://example.invalid/sample/update-main --output=/work/update-token",
        ]
    );
    assert_eq!(
        split_dense_procedure_line("Prelude text runner-tool --mode=strict /work/update-token",),
        vec!["Prelude text", "runner-tool --mode=strict /work/update-token",]
    );
    assert_eq!(
        split_dense_procedure_line(
            "alpha-tool --key=value beta-tool --mode=strict /work/result.bin",
        ),
        vec!["alpha-tool --key=value", "beta-tool --mode=strict /work/result.bin",]
    );
}

#[test]
fn dense_procedure_line_does_not_promote_plain_words_without_structural_signals() {
    assert_eq!(split_dense_procedure_line("alpha beta gamma"), vec!["alpha beta gamma"]);
    assert_eq!(
        split_dense_procedure_line("Prelude well-known limitation"),
        vec!["Prelude well-known limitation"]
    );
}

#[test]
fn dense_procedure_line_keeps_uri_arguments_attached_to_the_invocable_head() {
    assert_eq!(
        split_dense_procedure_line(
            "runner-tool --mode=strict https://example.invalid/object --next=value",
        ),
        vec!["runner-tool --mode=strict https://example.invalid/object --next=value"]
    );
}

#[test]
fn dense_procedure_line_keeps_multiple_path_arguments_in_one_command() {
    assert_eq!(
        split_dense_procedure_line("copy-tool /work/source.bin /work/target.bin",),
        vec!["copy-tool /work/source.bin /work/target.bin"]
    );
}

#[test]
fn dense_procedure_line_splits_repeated_concatenated_artifact_paths() {
    assert_eq!(
        split_dense_procedure_line("sample-prepare +x /work/update-token.sh/work/update-token.sh",),
        vec!["sample-prepare +x /work/update-token.sh", "/work/update-token.sh"]
    );
}

#[test]
fn command_signal_requires_formal_structure_instead_of_executable_looking_prose() {
    for text in [
        "well-known limitation",
        "Alpha-2 remains stable",
        "Release version 2.0",
        "QR-code is not performed automatically.",
        "address should be configured as https://example.invalid/object",
        "refer to /work/item for details",
    ] {
        assert!(!line_has_command_signal(text), "{text}");
    }
}

#[test]
fn command_signal_preserves_ordered_path_flag_assignment_uri_and_code_steps() {
    for text in [
        "1. /work/runner",
        "2. runner --mode=strict",
        "3. runner mode=strict",
        "4. runner https://example.invalid/object",
        "`runner apply`",
    ] {
        assert!(line_has_command_signal(text), "{text}");
    }
}

#[test]
fn command_head_identity_uses_the_invocation_head_not_a_shared_path_argument() {
    assert!(update_procedure_command_heads_match(
        "alpha --output /work/first",
        "alpha --input /work/second",
    ));
    assert!(!update_procedure_command_heads_match(
        "alpha --output /work/shared",
        "beta --output /work/shared",
    ));
}

#[test]
fn procedure_line_blocks_treat_atx_headings_as_structure_not_steps() {
    let blocks = update_procedure_line_blocks(
        "1. sample-runner --prepare\n\
         2. sample-runner --apply\n\
         ## Alternate topology\n\
         1. sample-runner --prepare-secondary\n\
         2. sample-runner --apply-secondary",
    );

    assert_eq!(blocks.len(), 2, "{blocks:#?}");
    assert!(
        blocks.iter().flatten().all(|line| !line.text.trim_start().starts_with('#')),
        "{blocks:#?}"
    );
}

#[test]
fn dense_procedure_line_preserves_bracketed_identifier_as_one_span() {
    assert_eq!(
        split_dense_procedure_line(
            "Add section [ Adapter.Main ] with mode= safe before continuing.",
        ),
        vec!["Add section [Adapter.Main] with mode= safe before continuing."]
    );
    assert_eq!(
        split_dense_procedure_line("Select [Mode Name] before continuing."),
        vec!["Select [Mode Name] before continuing."]
    );
}

#[test]
fn update_procedure_sequence_does_not_flatten_alternative_projections_from_one_block() {
    let mut procedure_chunk = evidence_chunk(
        1,
        Some("paragraph"),
        "Sample Target update:\n\
         - Layout A: runner-shell --scope=elevated sample-fetch https://example.invalid/update-primary.sh --output=/work/update.sh sample-mode +x /work/update.sh /work/update.sh\n\
         - Layout B: runner-shell --scope=elevated sample-fetch https://example.invalid/update-secondary.sh --output=/work/update.sh sample-mode +x /work/update.sh /work/update.sh\n\
         ## Pinned package inventory\n\
         1. sample-package --list\n\
         2. sample-package --pin",
    );
    procedure_chunk.document_label = "Sample Target update guide".to_string();

    let answer = build_update_procedure_sequence_answer(
        "how to update Sample Target?",
        &configure_update_focus_ir("Sample Target"),
        &[procedure_chunk],
    );

    if let Some(answer) = answer {
        let contains_primary = answer.contains("update-primary.sh");
        let contains_secondary = answer.contains("update-secondary.sh");
        assert_ne!(contains_primary, contains_secondary, "{answer}");
        assert!(!answer.contains("Pinned package inventory"), "{answer}");
        assert!(!answer.lines().any(|line| line.contains(". #")), "{answer}");
    }
}
