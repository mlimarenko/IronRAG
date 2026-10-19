use anyhow::{Context, Result};
use uuid::Uuid;

use ironrag_backend::app::{seed_clean_v1_ai_catalog, state::AppState};

use crate::ingest_ai_support;

pub(crate) async fn configure_ingest_ai(
    state: &mut AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    fixture_label: &str,
) -> Result<()> {
    seed_clean_v1_ai_catalog(&state.persistence.postgres)
        .await
        .context("failed to seed web-ingest AI catalog")?;
    let account_label = format!("{fixture_label} AI fixture");
    ingest_ai_support::configure_ingest_ai_fixture(
        state,
        workspace_id,
        library_id,
        ingest_ai_support::IngestAiFixtureConfig {
            account_label: &account_label,
            account_kind: "AI",
            credential_ref: "test-only-provider-placeholder",
            fixture_name: "web-ingest",
            graph_output: r#"{"entities":[{"label":"Seed Page","node_type":"artifact","aliases":[],"sub_type":null,"summary":""},{"label":"single-page ingest","node_type":"process","aliases":[],"sub_type":null,"summary":""}],"relations":[{"source_label":"Seed Page","target_label":"single-page ingest","relation_type":"describes","summary":""}]}"#,
            invalid_dimensions_subject: None,
            missing_catalog_subject: "web-ingest test",
            io_delay: None,
            panic_on: None,
        },
    )
    .await
}
