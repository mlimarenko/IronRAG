use crate::{web_ingest_fixture, web_ingest_support};

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::{sync::broadcast, time};

use ironrag_backend::services::ingest::worker;

use web_ingest_fixture::WebIngestFixture;

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn single_page_default_does_not_follow_links() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_single_page").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run_to_terminal(server.url("/seed")).await?;
        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list run pages")?;
        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents")?;

        assert_eq!(run.run_state, "completed");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].normalized_url, server.url("/seed"));
        assert_eq!(pages[0].candidate_state, "processed");
        assert_eq!(documents.len(), 1);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn single_page_direct_download_passthrough_materializes_one_document() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_single_page").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run_to_terminal(server.url("/download.txt")).await?;
        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after download ingest")?;

        assert_eq!(run.run_state, "completed");
        assert_eq!(documents.len(), 1);
        assert_eq!(
            documents[0].active_revision.as_ref().map(|revision| revision.mime_type.as_str()),
            Some("text/plain; charset=utf-8")
        );
        assert_eq!(
            documents[0]
                .active_revision
                .as_ref()
                .and_then(|revision| revision.source_uri.as_deref()),
            Some(server.url("/download.txt").as_str())
        );

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn single_page_unsupported_payload_fails_without_materializing_documents() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_single_page").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run =
            fixture.submit_single_page_run_to_terminal(server.url("/unsupported.bin")).await?;
        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list failed run pages")?;
        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after unsupported ingest")?;

        assert_eq!(run.run_state, "failed");
        assert_eq!(run.failure_code.as_deref(), Some("unsupported_content"));
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].candidate_state, "failed");
        assert_eq!(pages[0].classification_reason.as_deref(), Some("unsupported_content"));
        assert_eq!(documents.len(), 0);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn single_page_redirects_normalize_to_final_url_identity() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_single_page").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run_to_terminal(server.url("/redirect")).await?;
        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list redirect run pages")?;
        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after redirect ingest")?;

        assert_eq!(run.run_state, "completed");
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].normalized_url, server.url("/redirect"));
        assert_eq!(pages[0].final_url.as_deref(), Some(server.url("/authoritative").as_str()));
        assert_eq!(pages[0].preferred_url.as_deref(), Some(server.url("/authoritative").as_str()));
        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].document.external_key, server.url("/authoritative"));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn repeated_single_page_submission_reuses_document_without_duplicate_revision() -> Result<()>
{
    let fixture = WebIngestFixture::create("web_ingest_single_page").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);

        let first_submitted = fixture.submit_single_page_run(server.url("/seed")).await?;
        let first_run =
            fixture.wait_for_run_terminal(first_submitted.run_id, Duration::from_secs(20)).await?;
        let first_page = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, first_run.run_id)
            .await
            .context("failed to list first run pages")?;
        let second_submitted = fixture.submit_single_page_run(server.url("/seed")).await?;
        let second_run =
            fixture.wait_for_run_terminal(second_submitted.run_id, Duration::from_secs(20)).await?;
        let second_page = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, second_run.run_id)
            .await
            .context("failed to list second run pages")?;
        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list documents after repeated submit")?;

        assert_eq!(documents.len(), 1);
        assert_eq!(first_page.len(), 1);
        assert_eq!(second_page.len(), 1);
        assert_eq!(first_page[0].document_id, second_page[0].document_id);
        assert_eq!(second_page[0].candidate_state, "duplicate");
        assert_eq!(second_page[0].classification_reason.as_deref(), Some("duplicate_content"));

        let document_id = documents[0].document.id;
        let revisions = fixture
            .state
            .application_services
            .content
            .list_revisions(&fixture.state, document_id)
            .await
            .context("failed to list revisions after repeated submit")?;
        assert_eq!(revisions.len(), 1);

        let _ = shutdown_tx.send(());
        time::timeout(Duration::from_secs(5), worker_handle)
            .await
            .context("repeated single-page worker did not stop")?
            .context("repeated single-page worker task panicked")?;

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn single_page_worker_runs_web_capture_through_authoritative_pipeline() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_single_page").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let run = fixture.submit_single_page_run(server.url("/seed")).await?;
        assert_eq!(run.run_state, "accepted");

        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);

        let terminal_run =
            fixture.wait_for_run_terminal(run.run_id, Duration::from_secs(20)).await?;
        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list terminal single-page run pages")?;
        let document_id = pages
            .first()
            .and_then(|page| page.document_id)
            .context("terminal single-page run is missing its materialized document id")?;
        let terminal_summary = fixture
            .state
            .application_services
            .content
            .get_document(&fixture.state, document_id)
            .await
            .context("failed to load terminal single-page document summary")?;
        let mutation_id = terminal_summary
            .pipeline
            .latest_mutation
            .as_ref()
            .map(|mutation| mutation.id)
            .context("missing latest mutation after authoritative worker execution")?;
        let job_handle = fixture
            .state
            .application_services
            .ingest
            .get_job_handle_by_mutation_id(&fixture.state, mutation_id)
            .await
            .context("failed to load authoritative ingest job handle")?
            .context("missing authoritative ingest job handle after worker execution")?;
        let attempt_id = job_handle
            .latest_attempt
            .as_ref()
            .map(|attempt| attempt.id)
            .context("missing authoritative ingest attempt after worker execution")?;
        let stage_events = fixture
            .state
            .application_services
            .ingest
            .list_stage_events(&fixture.state, attempt_id)
            .await
            .context("failed to list authoritative ingest stage events")?;
        let completed_graph_details = &stage_events
            .iter()
            .find(|event| event.stage_name == "extract_graph" && event.stage_state == "completed")
            .context("missing completed authoritative graph stage")?
            .details_json;
        assert_eq!(
            completed_graph_details
                .get("extractedEntityCandidates")
                .and_then(serde_json::Value::as_u64),
            Some(2),
            "the deterministic graph fixture must extract its grounded entities: {completed_graph_details}"
        );
        assert!(
            completed_graph_details
                .get("graphContributionCount")
                .and_then(serde_json::Value::as_u64)
                .is_some_and(|count| count > 0),
            "the deterministic graph fixture must contribute to the active projection: {completed_graph_details}"
        );
        assert_eq!(
            completed_graph_details.get("graphReady").and_then(serde_json::Value::as_bool),
            Some(true),
            "a positive deterministic graph contribution must publish as ready: {completed_graph_details}"
        );
        assert_eq!(
            terminal_summary
                .readiness
                .as_ref()
                .map(|readiness| readiness.graph_state.as_str()),
            Some("ready"),
            "the terminal document readiness must agree with its completed graph stage: {completed_graph_details}"
        );
        let ready_summary =
            fixture.wait_for_document_ready(document_id, Duration::from_secs(15)).await?;
        let ready_readiness_summary = serde_json::to_value(
            ready_summary.readiness_summary.as_ref().context("missing ready readiness summary")?,
        )
        .context("failed to serialize ready readiness summary")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(
            ready_summary.readiness.as_ref().map(|readiness| readiness.text_state.as_str()),
            Some("text_readable")
        );
        assert_eq!(terminal_run.run_state, "completed");
        assert_eq!(
            ready_summary.readiness.as_ref().map(|readiness| readiness.graph_state.as_str()),
            Some("ready")
        );
        assert_eq!(
            ready_readiness_summary.get("activityStatus").and_then(serde_json::Value::as_str),
            Some("ready")
        );
        let observed_stages = stage_events
            .iter()
            .map(|event| (event.stage_name.as_str(), event.stage_state.as_str()))
            .collect::<Vec<_>>();

        assert!(observed_stages.contains(&("extract_content", "completed")));
        assert!(observed_stages.contains(&("prepare_structure", "started")));
        assert!(observed_stages.contains(&("prepare_structure", "completed")));
        assert!(observed_stages.contains(&("chunk_content", "completed")));
        assert!(observed_stages.contains(&("extract_technical_facts", "completed")));
        assert!(observed_stages.contains(&("embed_chunk", "completed")));
        assert!(observed_stages.contains(&("extract_graph", "started")));
        assert!(observed_stages.contains(&("extract_graph", "completed")));
        assert!(observed_stages.contains(&("finalizing", "completed")));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
