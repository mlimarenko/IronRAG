#[path = "support/web_ingest_fixture.rs"]
mod web_ingest_fixture;
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::{sync::broadcast, time};

use ironrag_backend::services::ingest::worker;

use web_ingest_fixture::WebIngestFixture;

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn recursive_run_surfaces_mixed_page_outcomes_with_truthful_partial_counts() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_visibility_mixed").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let mut seed_url =
            reqwest::Url::parse(&server.url("/recursive/seed")).context("parse recursive seed")?;
        seed_url.query_pairs_mut().append_pair("broken", "1").append_pair("unsupported", "1");

        let submitted_run = fixture
            .submit_recursive_run(seed_url.to_string(), "same_host", Some(1), Some(20))
            .await?;
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);

        let run =
            fixture.wait_for_run_terminal(submitted_run.run_id, Duration::from_secs(20)).await?;
        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list visibility pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list visibility documents")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed_partial");
        assert_eq!(run.counts.discovered, 9);
        assert_eq!(run.counts.eligible, 5);
        assert_eq!(run.counts.processed, 4);
        assert_eq!(run.counts.failed, 1);
        assert_eq!(run.counts.blocked, 1);
        assert_eq!(run.counts.excluded, 3);
        assert_eq!(run.counts.canceled, 0);
        assert_eq!(documents.len(), 4);

        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/unsupported.bin")
                && page.candidate_state == "failed"
                && page.classification_reason.as_deref() == Some("unsupported_content")
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/missing")
                && page.candidate_state == "blocked"
                && page.classification_reason.as_deref() == Some("inaccessible")
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/depth-two")
                && page.candidate_state == "excluded"
                && page.classification_reason.as_deref() == Some("exceeded_max_depth")
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == "https://external.example/docs"
                && page.candidate_state == "excluded"
                && page.classification_reason.as_deref() == Some("outside_boundary_policy")
        }));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn cancel_requested_during_discovery_stops_new_admission_and_marks_seed_canceled()
-> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_visibility_cancel").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let mut seed_url =
            reqwest::Url::parse(&server.url("/recursive/seed")).context("parse recursive seed")?;
        seed_url.query_pairs_mut().append_pair("sleepMs", "800");

        let submitted_run = fixture
            .submit_recursive_run(seed_url.to_string(), "same_host", Some(1), Some(20))
            .await?;
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);

        time::sleep(Duration::from_millis(100)).await;
        let _ = fixture
            .state
            .canonical_services
            .web_ingest
            .cancel_run(&fixture.state, submitted_run.run_id)
            .await
            .context("failed to request recursive cancel")?;

        let run =
            fixture.wait_for_run_terminal(submitted_run.run_id, Duration::from_secs(20)).await?;
        let pages = fixture
            .state
            .canonical_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list canceled visibility pages")?;
        let jobs = fixture
            .state
            .canonical_services
            .ingest
            .list_jobs(&fixture.state, Some(fixture.workspace_id), Some(fixture.library_id))
            .await
            .context("failed to list canonical jobs for canceled run")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "canceled");
        assert_eq!(run.counts.discovered, 1);
        assert_eq!(run.counts.eligible, 1);
        assert_eq!(run.counts.processed, 0);
        assert_eq!(run.counts.queued, 0);
        assert_eq!(run.counts.processing, 0);
        assert_eq!(run.counts.canceled, 1);
        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].candidate_state, "canceled");
        assert_eq!(pages[0].classification_reason.as_deref(), Some("cancel_requested"));
        assert_eq!(jobs.iter().filter(|job| job.job_kind == "web_discovery").count(), 1);
        assert_eq!(jobs.iter().filter(|job| job.job_kind == "web_materialize_page").count(), 0);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
