use super::libraries::{missing_required_purposes, runtime_readiness};
use super::*;

fn present(purposes: &[AiBindingPurpose]) -> HashSet<String> {
    purposes.iter().map(|purpose| purpose.as_str().to_string()).collect()
}

#[test]
fn runtime_readiness_uses_the_active_embedding_profile() {
    let present = present(&[AiBindingPurpose::ExtractGraph, AiBindingPurpose::EmbedChunk]);
    let ingestion_readiness = CatalogLibraryIngestionReadiness {
        ready: true,
        missing_binding_purposes: missing_required_purposes(
            Some(&present),
            INGEST_REQUIRED_BINDINGS,
        ),
    };

    let readiness =
        runtime_readiness(Some(&present), &ingestion_readiness.missing_binding_purposes);

    assert_eq!(
        readiness.missing_binding_purposes,
        vec![AiBindingPurpose::QueryCompile, AiBindingPurpose::QueryAnswer]
    );
}

#[test]
fn ingestion_readiness_does_not_block_on_query_purposes() {
    let present = present(&[AiBindingPurpose::ExtractGraph, AiBindingPurpose::EmbedChunk]);
    let missing = missing_required_purposes(Some(&present), INGEST_REQUIRED_BINDINGS);

    assert!(missing.is_empty());
}

#[test]
fn runtime_readiness_preserves_document_understanding_when_ingest_requires_it() {
    let ingestion_readiness = CatalogLibraryIngestionReadiness {
        ready: false,
        missing_binding_purposes: vec![AiBindingPurpose::ExtractText],
    };

    let readiness = runtime_readiness(None, &ingestion_readiness.missing_binding_purposes);

    assert_eq!(readiness.missing_binding_purposes[0], AiBindingPurpose::ExtractText);
    assert!(readiness.missing_binding_purposes.contains(&AiBindingPurpose::EmbedChunk));
    assert!(readiness.missing_binding_purposes.contains(&AiBindingPurpose::QueryCompile));
    assert!(readiness.missing_binding_purposes.contains(&AiBindingPurpose::QueryAnswer));
}

#[test]
fn webhook_delete_blockers_map_to_catalog_conflict() {
    let error = map_catalog_delete_blockers(
        "workspace",
        catalog_repository::CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 2,
            active_webhook_delivery_jobs: 3,
            unresolved_webhook_delivery_attempts: 4,
        },
    );

    assert!(matches!(&error, ApiError::Conflict(_)));
    assert_eq!(error.kind(), "conflict");
    assert_eq!(
        error.to_string(),
        "conflict: workspace deletion is blocked until durable webhook work is drained \
         (undispatched lifecycle events: 2, active delivery jobs: 3, \
         unresolved delivery attempts: 4)"
    );
}
