use anyhow::Context;
use chrono::Utc;
use serde_json::json;
use sqlx::query;
use uuid::Uuid;

use rustrag_backend::{
    app::{config::Settings, state::AppState},
    domains::pricing_catalog::{PricingBillingUnit, PricingCapability},
    infra::repositories,
    services::{document_accounting, pricing_catalog},
};

async fn build_state() -> anyhow::Result<AppState> {
    let settings = Settings::from_env().context("failed to load settings for accounting test")?;
    let state = AppState::new(settings).await.context("failed to build app state")?;
    pricing_catalog::bootstrap_from_env_if_enabled(&state)
        .await
        .context("failed to bootstrap pricing catalog for accounting test")?;
    Ok(state)
}

#[tokio::test]
#[ignore = "requires local postgres + neo4j services reachable from host settings"]
async fn repeated_provider_call_key_reuses_existing_usage_and_cost_artifacts() -> anyhow::Result<()>
{
    let state = build_state().await?;
    let pool = &state.persistence.postgres;
    let suffix = Uuid::now_v7().simple().to_string();
    let workspace = repositories::create_workspace(
        pool,
        &format!("acct-resume-{suffix}"),
        "Accounting Resume Idempotency",
    )
    .await
    .context("failed to create workspace")?;
    let project = repositories::create_project(
        pool,
        workspace.id,
        &format!("acct-resume-library-{suffix}"),
        "Accounting Resume Library",
        Some("resume idempotency fixture"),
    )
    .await
    .context("failed to create project")?;

    let result = async {
        let run = repositories::create_runtime_ingestion_run(
            pool,
            project.id,
            None,
            None,
            None,
            "track-accounting-resume",
            "resume-idempotency.md",
            "md",
            Some("text/markdown"),
            Some(128),
            "processing",
            "extracting_graph",
            "initial_upload",
            json!({}),
        )
        .await
        .context("failed to create runtime ingestion run")?;
        let stage_event = repositories::append_runtime_stage_event(
            pool,
            run.id,
            run.current_attempt_no,
            "extracting_graph",
            "started",
            None,
            json!({
                "provider_kind": "openai",
                "model_name": "gpt-5-mini",
                "started_at": Utc::now(),
            }),
        )
        .await
        .context("failed to append extracting_graph stage event")?;

        let request = document_accounting::StageUsageAccountingRequest {
            ingestion_run_id: run.id,
            stage_event_id: stage_event.id,
            stage: "extracting_graph".to_string(),
            accounting_scope: document_accounting::StageAccountingScope::ProviderCall {
                call_sequence_no: 1,
            },
            workspace_id: Some(workspace.id),
            project_id: Some(project.id),
            model_profile_id: None,
            provider_kind: "openai".to_string(),
            model_name: "gpt-5-mini".to_string(),
            capability: PricingCapability::GraphExtract,
            billing_unit: PricingBillingUnit::Per1MTokens,
            usage_kind: "runtime_document_graph_extract_call".to_string(),
            prompt_tokens: Some(1200),
            completion_tokens: Some(340),
            total_tokens: Some(1540),
            raw_usage_json: json!({
                "provider_call_no": 1,
                "provider_attempt_no": 1,
                "request_shape_key": "graph_extract_v3:initial:segments_1:full",
                "request_size_bytes": 8192,
                "prompt_tokens": 1200,
                "completion_tokens": 340,
                "total_tokens": 1540,
            }),
        };

        let first =
            document_accounting::record_stage_usage_and_cost(&state, request.clone()).await?;
        let second = document_accounting::record_stage_usage_and_cost(&state, request).await?;

        let stage_rows = repositories::list_attempt_stage_accounting_by_run(pool, run.id)
            .await
            .context("failed to list stage accounting rows")?;
        let usage_rows = repositories::list_usage_events(pool, Some(project.id))
            .await
            .context("failed to list usage events")?;
        let cost_rows = repositories::list_cost_ledger(pool, Some(project.id))
            .await
            .context("failed to list cost ledger rows")?;
        let settlement =
            repositories::load_runtime_collection_settlement_snapshot(pool, project.id)
                .await
                .context("failed to load settlement snapshot")?
                .context("missing settlement snapshot after accounting write")?;

        assert_eq!(first.usage_event.id, second.usage_event.id);
        assert_eq!(
            first.cost_ledger.as_ref().map(|row| row.id),
            second.cost_ledger.as_ref().map(|row| row.id)
        );
        assert_eq!(first.stage_accounting.id, second.stage_accounting.id);
        assert_eq!(stage_rows.len(), 1);
        assert_eq!(usage_rows.len(), 1);
        assert_eq!(cost_rows.len(), 1);
        assert_eq!(settlement.total_tokens, 1540);
        assert_eq!(settlement.priced_stage_count, 1);
        assert_eq!(settlement.in_flight_stage_count, 1);

        Ok(())
    }
    .await;

    query("delete from workspace where id = $1")
        .bind(workspace.id)
        .execute(pool)
        .await
        .context("failed to clean accounting resume workspace")?;

    result
}
