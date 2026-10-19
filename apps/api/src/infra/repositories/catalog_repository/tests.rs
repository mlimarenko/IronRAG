use std::time::Duration;

use serde_json::json;
use sqlx::postgres::PgPoolOptions;

use super::library_queries::QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL;
use super::*;

#[test]
fn query_authorized_library_candidates_are_active_minimal_and_bounded() {
    let normalized = QUERY_AUTHORIZED_LIBRARY_CANDIDATES_SQL.to_ascii_lowercase();

    assert!(normalized.contains("select id"));
    assert!(normalized.contains("lifecycle_state = 'active'"));
    assert!(normalized.contains("workspace_id = any($2::uuid[])"));
    assert!(normalized.contains("id = any($3::uuid[])"));
    assert!(normalized.contains("limit 2"));
    assert!(!normalized.contains("extraction_prompt"));
    assert!(!normalized.contains("retrieval_config"));
    assert!(!normalized.contains("select *"));
}

#[test]
fn catalog_delete_blockers_are_empty_only_without_pending_work() {
    assert!(
        CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        }
        .is_empty()
    );

    for blockers in [
        CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 1,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 0,
        },
        CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 1,
            unresolved_webhook_delivery_attempts: 0,
        },
        CatalogDeleteBlockers {
            undispatched_webhook_lifecycle_events: 0,
            active_webhook_delivery_jobs: 0,
            unresolved_webhook_delivery_attempts: 1,
        },
    ] {
        assert!(!blockers.is_empty());
    }
}

fn unavailable_pool() -> PgPool {
    PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(1))
        .connect_lazy("postgresql://postgres@127.0.0.1:1/ironrag")
        .expect("static PostgreSQL URL must parse")
}

#[tokio::test]
async fn catalog_queries_propagate_database_unavailability() {
    let postgres = unavailable_pool();
    let id = Uuid::nil();

    assert!(list_workspaces(&postgres).await.is_err());
    assert!(list_workspace_ids(&postgres).await.is_err());
    assert!(get_workspace_by_id(&postgres, id).await.is_err());
    assert!(get_workspace_by_slug(&postgres, "workspace-alpha").await.is_err());
    assert!(create_workspace(&postgres, "workspace-alpha", "Workspace Alpha", None).await.is_err());
    assert!(
        update_workspace(&postgres, id, "workspace-alpha", "Workspace Alpha", "active")
            .await
            .is_err()
    );
    assert!(archive_workspace(&postgres, id).await.is_err());
    assert!(list_libraries(&postgres, Some(id)).await.is_err());
    assert!(list_libraries(&postgres, None).await.is_err());
    assert!(list_library_identities(&postgres).await.is_err());
    assert!(list_query_authorized_library_candidates(&postgres, true, &[], &[]).await.is_err());
    assert!(get_library_by_id(&postgres, id).await.is_err());
    assert!(get_library_by_workspace_and_slug(&postgres, id, "library-alpha").await.is_err());
    assert!(
        create_library(&postgres, id, "library-alpha", "Library Alpha", None, None).await.is_err()
    );
    assert!(
        create_library_with_recognition_policy(
            &postgres,
            NewCatalogLibrary {
                workspace_id: id,
                slug: "library-alpha",
                display_name: "Library Alpha",
                description: None,
                recognition_policy: json!({}),
                created_by_principal_id: None,
            },
        )
        .await
        .is_err()
    );
    assert!(touch_library_source_truth_version(&postgres, id).await.is_err());
    assert!(get_library_source_truth_version(&postgres, id).await.is_err());
    assert!(
        update_library(
            &postgres,
            CatalogLibraryUpdate {
                library_id: id,
                slug: "library-alpha",
                display_name: "Library Alpha",
                description: None,
                extraction_prompt: None,
                lifecycle_state: "active",
                include_document_hint_in_mcp_answers: false,
            },
        )
        .await
        .is_err()
    );
    assert!(update_library_web_ingest_policy(&postgres, id, json!({})).await.is_err());
    assert!(update_library_recognition_policy(&postgres, id, json!({})).await.is_err());
    assert!(update_library_retrieval_config(&postgres, id, json!({})).await.is_err());
    assert!(archive_library(&postgres, id).await.is_err());
}
