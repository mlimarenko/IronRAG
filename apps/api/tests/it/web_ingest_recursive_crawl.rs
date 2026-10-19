use crate::{web_ingest_fixture, web_ingest_support};

use anyhow::{Context, Result};
use std::time::Duration;
use tokio::{sync::broadcast, time};

use ironrag_backend::services::ingest::worker;
use ironrag_backend::shared::web::ingest::{WebIngestPattern, WebIngestUrlFilter};

use web_ingest_fixture::{
    FilteredRecursiveCrawlRequest, WebIngestFixture, assert_outside_boundary_page_excluded,
};

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list recursive same-host pages")?;
        let documents = fixture
            .state
            .application_services
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
        assert_outside_boundary_page_excluded(&pages);
        assert!(!pages.iter().any(|page| {
            matches!(
                page.normalized_url.as_str(),
                value if value == server.url("/recursive/cycle-b")
                    || value == server.url("/recursive/depth-two")
            )
        }));

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
async fn recursive_crawl_allow_external_materializes_reachable_external_pages() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_allow_external").await?;
    let server = web_ingest_support::WebTestServer::start().await?;
    let external_server = web_ingest_support::WebTestServer::start_external().await?;

    let result = async {
        let mut seed_url = reqwest::Url::parse(&server.url("/recursive/seed"))
            .context("failed to parse recursive seed url")?;
        seed_url
            .query_pairs_mut()
            .append_pair("external", &external_server.url("/recursive/external"));

        let submitted_run = fixture
            .submit_recursive_run(seed_url.to_string(), "allow_external", Some(1), Some(20))
            .await?;
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);
        let run =
            fixture.wait_for_run_terminal(submitted_run.run_id, Duration::from_secs(20)).await?;
        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list allow-external pages")?;
        let documents = fixture
            .state
            .application_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list allow-external documents")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed");
        assert!(documents.len() >= 5);
        assert!(pages.iter().any(|page| {
            page.normalized_url == external_server.url("/recursive/external")
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
#[ignore = "requires local postgres with required extensions"]
async fn recursive_crawl_allowlist_traverses_to_matching_urls() -> Result<()> {
    let fixture = WebIngestFixture::create("web_ingest_recursive_allowlist_traversal").await?;
    let server = web_ingest_support::WebTestServer::start().await?;

    let result = async {
        let submitted_run = fixture
            .submit_recursive_run_with_filters(FilteredRecursiveCrawlRequest {
                seed_url: server.url("/recursive/seed"),
                boundary_policy: "same_host",
                max_depth: Some(2),
                max_pages: Some(20),
                crawl_filter: WebIngestUrlFilter::default(),
                materialization_filter: WebIngestUrlFilter {
                    allow_patterns: vec![WebIngestPattern {
                        kind: "path_prefix".to_string(),
                        value: "/recursive/depth-two".to_string(),
                        source: None,
                    }],
                    block_patterns: Vec::new(),
                },
            })
            .await?;
        let (shutdown_tx, shutdown_rx) = broadcast::channel(1);
        let worker_handle = worker::spawn_ingestion_worker(fixture.state.clone(), shutdown_rx);
        let run =
            fixture.wait_for_run_terminal(submitted_run.run_id, Duration::from_secs(20)).await?;
        let pages = fixture
            .state
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list allowlist traversal pages")?;
        let documents = fixture
            .state
            .application_services
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
#[ignore = "requires local postgres with required extensions"]
async fn recursive_crawl_visits_each_cycle_url_once_and_terminates() -> Result<()> {
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
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list recursive cycle pages")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed_partial");
        for cycle_path in ["/recursive/cycle-a", "/recursive/cycle-b"] {
            let cycle_url = server.url(cycle_path);
            let matches =
                pages.iter().filter(|page| page.normalized_url == cycle_url).collect::<Vec<_>>();
            assert_eq!(matches.len(), 1, "cycle URL must be persisted exactly once");
            assert_eq!(matches[0].candidate_state, "processed");
        }
        assert_eq!(pages.iter().filter(|page| page.candidate_state == "processed").count(), 7);
        assert_outside_boundary_page_excluded(&pages);

        Ok(())
    }
    .await;

    server.shutdown().await?;
    fixture.cleanup().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres with required extensions"]
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
            .application_services
            .web_ingest
            .list_pages(&fixture.state, run.run_id)
            .await
            .context("failed to list recursive alias pages")?;

        let _ = shutdown_tx.send(());
        let _ = time::timeout(Duration::from_secs(5), worker_handle).await;

        assert_eq!(run.run_state, "completed");
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
