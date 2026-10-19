// ---------------------------------------------------------------------------
// gc
// ---------------------------------------------------------------------------

use super::*;
use maintenance_gc as gc;

pub(super) async fn run_gc(state: &AppState, command: GcCommand) -> Result<()> {
    match command {
        GcCommand::StaleEvidence { library, json } => {
            gc_commands::stale_evidence(state, library, json).await
        }
        GcCommand::SupersededVectors { library, dry_run, include_null_head, json } => {
            gc_commands::superseded_vectors(state, library, dry_run, include_null_head, json).await
        }
        GcCommand::HistoricalProjectionWindow { library, dry_run, json } => {
            gc_commands::historical_projection_window(state, library, dry_run, json).await
        }
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
mod gc_commands {
    use super::*;

    pub(super) async fn stale_evidence(
        state: &AppState,
        library: Option<Uuid>,
        json: bool,
    ) -> Result<()> {
        let libraries = libraries_for_gc(state, library).await?;
        let total_report = collect_stale_evidence(state, &libraries).await;
        if json {
            println!("{}", serde_json::to_string_pretty(&total_report)?);
        } else {
            println!(
                "gc.stale-evidence  stale_revision_rows={}  phantom_chunk_rows={}",
                total_report.stale_revision_rows, total_report.phantom_chunk_rows,
            );
        }
        Ok(())
    }

    pub(super) async fn superseded_vectors(
        state: &AppState,
        library: Option<Uuid>,
        dry_run: bool,
        include_null_head: bool,
        json: bool,
    ) -> Result<()> {
        let options = GcSupersededVectorsOptions { dry_run, include_null_head, max_batches: None };
        let report = match library {
            Some(library_id) => {
                let row = library_for_gc(state, library_id).await?;
                gc::run_superseded_vectors_for_library(state, row.workspace_id, row.id, options)
                    .await?
            }
            None => gc::run_superseded_vectors_for_all_libraries(state, options).await?,
        };
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            print_gc_report(library, options, &report);
        }
        Ok(())
    }

    pub(super) async fn historical_projection_window(
        state: &AppState,
        library: Uuid,
        dry_run: bool,
        json: bool,
    ) -> Result<()> {
        let row = library_for_gc(state, library).await?;
        let report = gc::run_historical_projection_window_for_library(
            state,
            row.workspace_id,
            row.id,
            dry_run,
        )
        .await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
        println!(
            "gc.historical-projection library={} dry_run={} planned={}",
            row.id, dry_run, report.planned,
        );
        println!("  documents_visited            {}", report.documents_visited);
        println!("  documents_purged             {}", report.documents_purged);
        println!("  knowledge_revisions_removed  {}", report.knowledge_revisions_removed);
        println!("  chunk_vector_rows_removed    {}", report.chunk_vector_rows_removed);
        println!("  graph_evidence_rows_removed  {}", report.graph_evidence_rows_removed);
        println!("  graph_rebuilds_completed     {}", report.graph_rebuilds_completed);
        println!("  graph_rebuilds_skipped       {}", report.graph_rebuilds_skipped);
        Ok(())
    }

    async fn libraries_for_gc(
        state: &AppState,
        library: Option<Uuid>,
    ) -> Result<Vec<catalog_repository::CatalogLibraryRow>> {
        match library {
            Some(library_id) => Ok(vec![library_for_gc(state, library_id).await?]),
            None => catalog_repository::list_libraries(&state.persistence.postgres, None)
                .await
                .map_err(Into::into),
        }
    }

    async fn library_for_gc(
        state: &AppState,
        library_id: Uuid,
    ) -> Result<catalog_repository::CatalogLibraryRow> {
        catalog_repository::list_libraries(&state.persistence.postgres, None)
            .await?
            .into_iter()
            .find(|library| library.id == library_id)
            .ok_or_else(|| anyhow::anyhow!("library {library_id} not found"))
    }

    async fn collect_stale_evidence(
        state: &AppState,
        libraries: &[catalog_repository::CatalogLibraryRow],
    ) -> gc::StaleEvidenceReport {
        let mut totals = (0_i64, 0_i64);
        for library_row in libraries {
            match gc::run_stale_evidence(state, library_row.id).await {
                Ok(report) => {
                    totals.0 += report.stale_revision_rows;
                    totals.1 += report.phantom_chunk_rows;
                }
                Err(error) => {
                    eprintln!("gc.stale-evidence failed for library {}: {error}", library_row.id)
                }
            }
        }
        gc::StaleEvidenceReport { stale_revision_rows: totals.0, phantom_chunk_rows: totals.1 }
    }
}

#[allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "operator maintenance commands intentionally stream reports to the terminal"
)]
fn print_gc_report(
    library: Option<Uuid>,
    options: GcSupersededVectorsOptions,
    report: &SupersededVectorGcReport,
) {
    let scope = library.map_or_else(|| "all".to_string(), |id| id.to_string());
    println!(
        "gc.superseded-vectors scope={scope} dry_run={} include_null_head={}",
        options.dry_run, options.include_null_head,
    );
    println!("  documents_visited           {}", report.documents_visited);
    println!("  documents_with_superseded_vectors {}", report.documents_with_superseded_vectors);
    println!("  superseded_vectors_removed  {}", report.superseded_vectors_removed);
    println!("  null_head_docs_total        {}", report.null_head_docs_total);
    println!("  null_head_docs_processed    {}", report.null_head_docs_processed);
    println!("  null_head_vectors_removed   {}", report.null_head_vectors_removed);
    println!("  work_remaining              {}", report.work_remaining);
}
