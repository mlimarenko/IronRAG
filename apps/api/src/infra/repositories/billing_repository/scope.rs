//! The one resolver from a billing owning execution to its workspace, library
//! and document scope.
//!
//! Both the durable settlement read and the per-send admission transaction need
//! the same answer, so they call this single implementation instead of each
//! carrying its own match. A missing or inconsistent owning execution resolves
//! to `None`, which every caller treats as a refusal rather than a default.

use sqlx::PgPool;
use uuid::Uuid;

use crate::infra::repositories::{
    catalog_repository, ingest_repository, query_repository, runtime_graph_repository,
};

use super::{
    BillingDocumentReadRow, BillingExecutionCostRollupStateRow, BillingKnowledgeSearchRow,
};

/// Workspace, library and optional document of one billing owning execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BillingExecutionScopeRow {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub knowledge_document_id: Option<Uuid>,
}

/// Resolves the scope of one billing owning execution.
///
/// The resource name a caller reports on a miss is derived from the owner kind,
/// so a missing row keeps the same public shape it had when each domain
/// resolved its own scope.
pub async fn resolve_billing_execution_scope(
    postgres: &PgPool,
    owning_execution_kind: &str,
    owning_execution_id: Uuid,
) -> Result<Option<BillingExecutionScopeRow>, sqlx::Error> {
    match owning_execution_kind {
        "knowledge_search" => {
            let Some(BillingKnowledgeSearchRow { workspace_id, library_id, .. }) =
                super::get_knowledge_search(postgres, owning_execution_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(BillingExecutionScopeRow {
                workspace_id,
                library_id,
                knowledge_document_id: None,
            }))
        }
        "mcp_document_read" => {
            let Some(BillingDocumentReadRow { workspace_id, library_id, document_id, .. }) =
                super::get_document_read(postgres, owning_execution_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(BillingExecutionScopeRow {
                workspace_id,
                library_id,
                knowledge_document_id: Some(document_id),
            }))
        }
        "query_execution" => {
            if let Some(execution) =
                query_repository::get_execution_by_id(postgres, owning_execution_id).await?
            {
                return Ok(Some(BillingExecutionScopeRow {
                    workspace_id: execution.workspace_id,
                    library_id: execution.library_id,
                    knowledge_document_id: None,
                }));
            }
            // Conversation retention removes completed queries but preserves
            // their billing history in the durable rollup cursor.
            let Some(BillingExecutionCostRollupStateRow { workspace_id, library_id, .. }) =
                super::get_execution_cost_rollup_state(
                    postgres,
                    owning_execution_kind,
                    owning_execution_id,
                )
                .await?
            else {
                return Ok(None);
            };
            Ok(Some(BillingExecutionScopeRow {
                workspace_id,
                library_id,
                knowledge_document_id: None,
            }))
        }
        "graph_extraction_attempt" => {
            let Some(extraction) =
                runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
                    postgres,
                    owning_execution_id,
                )
                .await?
            else {
                return Ok(None);
            };
            let Some(library) =
                catalog_repository::get_library_by_id(postgres, extraction.library_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(BillingExecutionScopeRow {
                workspace_id: library.workspace_id,
                library_id: extraction.library_id,
                knowledge_document_id: Some(extraction.document_id),
            }))
        }
        "ingest_attempt" => {
            let Some(attempt) =
                ingest_repository::get_ingest_attempt_by_id(postgres, owning_execution_id).await?
            else {
                return Ok(None);
            };
            let Some(job) =
                ingest_repository::get_ingest_job_by_id(postgres, attempt.job_id).await?
            else {
                return Ok(None);
            };
            Ok(Some(BillingExecutionScopeRow {
                workspace_id: job.workspace_id,
                library_id: job.library_id,
                knowledge_document_id: job.knowledge_document_id,
            }))
        }
        _ => Ok(None),
    }
}

/// The resource name a miss reports, matching the per-domain vocabulary the
/// settlement path already used.
#[must_use]
pub fn billing_scope_not_found_resource(owning_execution_kind: &str) -> &'static str {
    match owning_execution_kind {
        "knowledge_search" => "knowledge_search",
        "mcp_document_read" => "mcp_document_read",
        "query_execution" => "query_execution",
        "graph_extraction_attempt" => "runtime_graph_extraction",
        "ingest_attempt" => "ingest_attempt",
        _ => "owning_execution",
    }
}
