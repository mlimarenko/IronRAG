use chrono::Utc;
use rustrag_backend::{
    infra::repositories::{ChunkRow, DocumentRow},
    services::graph_extract::{
        GraphExtractionRequest, GraphExtractionResumeHint, build_graph_extraction_prompt_preview,
    },
};
use uuid::Uuid;

fn oversized_request() -> GraphExtractionRequest {
    GraphExtractionRequest {
        project_id: Uuid::nil(),
        document: DocumentRow {
            id: Uuid::nil(),
            project_id: Uuid::nil(),
            source_id: None,
            external_key: "large-doc".to_string(),
            title: Some("Large doc".to_string()),
            mime_type: Some("text/plain".to_string()),
            checksum: None,
            current_revision_id: None,
            active_status: "active".to_string(),
            active_mutation_kind: None,
            active_mutation_status: None,
            deleted_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk: ChunkRow {
            id: Uuid::nil(),
            document_id: Uuid::nil(),
            project_id: Uuid::nil(),
            ordinal: 7,
            content: "Alpha Beta Gamma Delta ".repeat(25_000),
            token_count: None,
            metadata_json: serde_json::json!({}),
            created_at: Utc::now(),
        },
        revision_id: None,
        activated_by_attempt_id: None,
        resume_hint: None,
    }
}

#[test]
fn long_document_prompt_preview_is_bounded_and_repeatable() {
    let request = oversized_request();
    let (first_prompt, first_shape, first_size) =
        build_graph_extraction_prompt_preview(&request, 8 * 1024);
    let (second_prompt, second_shape, second_size) =
        build_graph_extraction_prompt_preview(&request, 8 * 1024);

    assert_eq!(first_prompt, second_prompt);
    assert_eq!(first_shape, second_shape);
    assert_eq!(first_size, second_size);
    assert!(first_shape.starts_with("graph_extract_v3:initial:segments_"));
    assert!(first_prompt.contains("[task]"));
    assert!(first_prompt.contains("[chunk_segment_1]"));
    assert!(first_size <= 16 * 1024);
}

#[test]
fn downgraded_resume_hint_reduces_request_shape_for_long_document() {
    let baseline = oversized_request();
    let mut downgraded = oversized_request();
    downgraded.resume_hint =
        Some(GraphExtractionResumeHint { replay_count: 6, downgrade_level: 2 });

    let (_, baseline_shape, baseline_size) =
        build_graph_extraction_prompt_preview(&baseline, 32 * 1024);
    let (_, downgraded_shape, downgraded_size) =
        build_graph_extraction_prompt_preview(&downgraded, 32 * 1024);

    assert!(baseline_shape.contains("downgrade_0"));
    assert!(downgraded_shape.contains("downgrade_2"));
    assert!(downgraded_size < baseline_size);
}
