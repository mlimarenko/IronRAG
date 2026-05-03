#[path = "support/web_ingest_fixture.rs"]
mod web_ingest_fixture;
#[path = "support/web_ingest_support.rs"]
mod web_ingest_support;

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::{sync::broadcast, time};

use ironrag_backend::services::ingest::worker;
use ironrag_backend::shared::web::ingest::{
    WebIngestPattern, WebIngestUrlFilter, WebIngestUrlFilterMode,
};

use web_ingest_fixture::WebIngestFixture;

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn recursive_crawl_same_host_respects_depth_and_excludes_external_links() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_same_host").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let submitted_run = fixture
            .submit_recursive_run(server.url("/recursive/seed"), "same_host", Some(1), Some(20))
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
            .context("failed to list recursive same-host pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list recursive same-host documents")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.mode, "recursive_crawl");
        assert_eq!(run.run_state, "completed_partial");
        assert_eq!(documents.len(), 4);
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/first")
                && page.candidate_state == "processed"
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/second")
                && page.candidate_state == "processed"
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/cycle-a")
                && page.candidate_state == "processed"
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == "https://external.example/docs"
                && page.candidate_state == "excluded"
                && page.classification_reason.as_deref() == Some("outside_boundary_policy")
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/cycle-b")
                && page.candidate_state == "excluded"
                && page.classification_reason.as_deref() == Some("exceeded_max_depth")
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
async fn recursive_crawl_allow_external_materializes_reachable_external_pages() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_allow_external").await?;
    let server = web_ingest_support::WebTestServer::start().await?;
    let external_server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let mut seed_url = reqwest::Url::parse(&server.url("/recursive/seed"))
            .context("failed to parse recursive seed url")?;
        seed_url
            .query_pairs_mut()
            .append_pair("external", &external_server.url("/recursive/second"));

        let submitted_run = fixture
            .submit_recursive_run(seed_url.to_string(), "allow_external", Some(1), Some(20))
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
            .context("failed to list allow-external pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list allow-external documents")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed_partial");
        assert!(documents.len() >= 5);
        assert!(pages.iter().any(|page| {
            page.normalized_url == external_server.url("/recursive/second")
                && page.candidate_state == "processed"
                && page.host_classification == "external"
        }));

        Ok(())
    }
    .await;

    external_server.shutdown().await?;
    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn recursive_crawl_allowlist_traverses_to_matching_urls() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_allowlist_traversal").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let submitted_run = fixture
            .submit_recursive_run_with_url_filter(
                server.url("/recursive/seed"),
                "same_host",
                Some(2),
                Some(20),
                WebIngestUrlFilter {
                    mode: WebIngestUrlFilterMode::Allowlist,
                    patterns: vec![WebIngestPattern {
                        kind: "path_prefix".to_string(),
                        value: "/recursive/depth-two".to_string(),
                        source: None,
                    }],
                },
            )
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
            .context("failed to list allowlist traversal pages")?;
        let documents = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list allowlist traversal documents")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed_partial");
        assert_eq!(documents.len(), 1);
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/seed")
                && page.candidate_state == "excluded"
                && page.classification_reason.as_deref() == Some("url_filter")
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/first")
                && page.candidate_state == "excluded"
                && page.classification_reason.as_deref() == Some("url_filter")
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/depth-two")
                && page.candidate_state == "processed"
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
async fn recursive_crawl_marks_cycle_revisits_as_duplicates() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_cycle").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let submitted_run = fixture
            .submit_recursive_run(server.url("/recursive/seed"), "same_host", Some(3), Some(20))
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
            .context("failed to list recursive cycle pages")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/cycle-a")
                && page.candidate_state == "duplicate"
                && page.classification_reason.as_deref() == Some("duplicate_canonical_url")
        }));
        assert!(pages.iter().filter(|page| page.candidate_state == "processed").count() >= 6);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn recursive_crawl_keeps_redirect_alias_duplicates_without_failing_run() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_alias_duplicates").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let submitted_run = fixture
            .submit_recursive_run(
                server.url("/recursive/alias-seed"),
                "same_host",
                Some(2),
                Some(20),
            )
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
            .context("failed to list recursive alias pages")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed_partial");
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/alias-direct")
                && page.candidate_state == "processed"
        }));
        assert!(pages.iter().any(|page| {
            page.normalized_url == server.url("/recursive/alias-short")
                && page.final_url.as_deref() == Some(server.url("/recursive/alias-direct").as_str())
                && page.candidate_state == "duplicate"
                && page.classification_reason.as_deref() == Some("duplicate_canonical_url")
        }));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}
