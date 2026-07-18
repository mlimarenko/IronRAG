use std::collections::BTreeSet;

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::knowledge_rows::{
        KnowledgeEntityRow, KnowledgeRelationRow, NewKnowledgeEntity, NewKnowledgeRelation,
    },
};

use super::{
    GraphRevisionContext, GraphService, canonical_entity_id, placeholder_entity_parts_from_key,
};

const CANONICAL_UPSERT_MAX_RETRIES: usize = 2;
const CANONICAL_UPSERT_BASE_BACKOFF_MS: u64 = 50;
const PG_SQLSTATE_SERIALIZATION_FAILURE: &str = "40001";
const PG_SQLSTATE_DEADLOCK_DETECTED: &str = "40P01";
const PG_SQLSTATE_LOCK_NOT_AVAILABLE: &str = "55P03";

fn is_retryable_upsert_contention(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<sqlx::Error>()
        .and_then(sqlx::Error::as_database_error)
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| {
            matches!(
                code.as_ref(),
                PG_SQLSTATE_SERIALIZATION_FAILURE
                    | PG_SQLSTATE_DEADLOCK_DETECTED
                    | PG_SQLSTATE_LOCK_NOT_AVAILABLE
            )
        })
}

const fn canonical_upsert_backoff(retry_count: usize) -> std::time::Duration {
    std::time::Duration::from_millis(CANONICAL_UPSERT_BASE_BACKOFF_MS * (1_u64 << retry_count))
}

impl GraphService {
    pub(super) async fn upsert_canonical_entity(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        normalization_key: &str,
        canonical_label: &str,
        entity_type: &str,
        aliases: BTreeSet<String>,
        confidence: Option<f64>,
        support_count: i64,
        freshness_generation: i64,
    ) -> Result<KnowledgeEntityRow> {
        let entity_id = canonical_entity_id(library_id, normalization_key);
        let existing = state
            .graph_store
            .get_entity_by_id(entity_id)
            .await
            .context("failed to load canonical entity before upsert")?;
        let mut merged_aliases =
            existing.as_ref().map(|row| row.aliases.clone()).unwrap_or_default();
        for alias in aliases {
            if !merged_aliases.iter().any(|existing| existing == &alias) {
                merged_aliases.push(alias);
            }
        }
        if !merged_aliases.iter().any(|alias| alias == canonical_label) {
            merged_aliases.push(canonical_label.to_string());
        }
        let summary = existing.as_ref().and_then(|row| row.summary.clone());
        let confidence = match (existing.as_ref().and_then(|row| row.confidence), confidence) {
            (Some(existing_confidence), Some(candidate_confidence)) => {
                Some(existing_confidence.max(candidate_confidence))
            }
            (Some(existing_confidence), None) => Some(existing_confidence),
            (None, Some(candidate_confidence)) => Some(candidate_confidence),
            (None, None) => None,
        };
        let entity = NewKnowledgeEntity {
            entity_id,
            workspace_id,
            library_id,
            canonical_label: canonical_label.to_string(),
            aliases: merged_aliases,
            entity_type: entity_type.to_string(),
            entity_sub_type: None,
            summary,
            confidence,
            support_count,
            freshness_generation,
            entity_state: "active".to_string(),
            created_at: existing.as_ref().map(|row| row.created_at),
            updated_at: Some(Utc::now()),
        };
        let mut retry_count = 0;
        loop {
            match state.graph_store.upsert_entity(&entity).await {
                Ok(row) => return Ok(row),
                Err(error) => {
                    if is_retryable_upsert_contention(&error)
                        && retry_count < CANONICAL_UPSERT_MAX_RETRIES
                    {
                        let backoff = canonical_upsert_backoff(retry_count);
                        retry_count += 1;
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(error).context("failed to upsert canonical knowledge entity");
                }
            }
        }
    }

    pub(super) async fn upsert_placeholder_entity_for_key(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        canonical_key: &str,
    ) -> Result<KnowledgeEntityRow> {
        let normalization_key = canonical_key.trim();
        let (node_type, canonical_label) = placeholder_entity_parts_from_key(normalization_key)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid canonical entity key `{normalization_key}` while materializing relation endpoints"
                )
            })?;
        let aliases = {
            let mut set = BTreeSet::new();
            set.insert(canonical_label.clone());
            set
        };
        self.upsert_canonical_entity(
            state,
            library_id,
            workspace_id,
            normalization_key,
            &canonical_label,
            crate::services::graph::identity::runtime_node_type_slug(&node_type),
            aliases,
            None,
            1,
            0,
        )
        .await
    }

    pub(super) async fn upsert_canonical_relation(
        &self,
        state: &AppState,
        library_id: Uuid,
        workspace_id: Uuid,
        normalized_assertion: &str,
        predicate: &str,
        confidence: Option<f64>,
        support_count: i64,
        freshness_generation: i64,
    ) -> Result<KnowledgeRelationRow> {
        let relation_id = super::canonical_relation_id(library_id, normalized_assertion);
        let existing = state
            .graph_store
            .get_relation_by_id(relation_id)
            .await
            .context("failed to load canonical relation before upsert")?;
        let confidence = match (existing.as_ref().and_then(|row| row.confidence), confidence) {
            (Some(existing_confidence), Some(candidate_confidence)) => {
                Some(existing_confidence.max(candidate_confidence))
            }
            (Some(existing_confidence), None) => Some(existing_confidence),
            (None, Some(candidate_confidence)) => Some(candidate_confidence),
            (None, None) => None,
        };
        let relation = NewKnowledgeRelation {
            relation_id,
            workspace_id,
            library_id,
            predicate: predicate.to_string(),
            normalized_assertion: normalized_assertion.to_string(),
            confidence,
            support_count,
            contradiction_state: existing
                .as_ref()
                .map_or_else(|| "unknown".to_string(), |row| row.contradiction_state.clone()),
            freshness_generation,
            relation_state: "active".to_string(),
            created_at: existing.as_ref().map(|row| row.created_at),
            updated_at: Some(Utc::now()),
        };
        let mut retry_count = 0;
        loop {
            match state.graph_store.upsert_relation(&relation).await {
                Ok(row) => return Ok(row),
                Err(error) => {
                    if is_retryable_upsert_contention(&error)
                        && retry_count < CANONICAL_UPSERT_MAX_RETRIES
                    {
                        let backoff = canonical_upsert_backoff(retry_count);
                        retry_count += 1;
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(error).context("failed to upsert canonical knowledge relation");
                }
            }
        }
    }

    pub(super) async fn upsert_relation_edges(
        &self,
        state: &AppState,
        relation: &KnowledgeRelationRow,
        subject: &KnowledgeEntityRow,
        object: &KnowledgeEntityRow,
    ) -> Result<()> {
        state
            .graph_store
            .upsert_relation_subject_edge(
                relation.relation_id,
                subject.entity_id,
                relation.library_id,
            )
            .await
            .context("failed to upsert relation-subject edge")?;
        state
            .graph_store
            .upsert_relation_object_edge(
                relation.relation_id,
                object.entity_id,
                relation.library_id,
            )
            .await
            .context("failed to upsert relation-object edge")
    }

    pub(super) async fn upsert_revision_edges(
        &self,
        state: &AppState,
        revision: &GraphRevisionContext,
    ) -> Result<()> {
        state
            .graph_store
            .upsert_document_revision_edge(
                revision.document_id,
                revision.revision_id,
                revision.library_id,
            )
            .await
            .context("failed to upsert document-revision edge")
    }

    pub(super) async fn upsert_chunk_edge(
        &self,
        state: &AppState,
        revision: &GraphRevisionContext,
        chunk_id: Uuid,
    ) -> Result<()> {
        state
            .graph_store
            .upsert_revision_chunk_edge(revision.revision_id, chunk_id, revision.library_id)
            .await
            .context("failed to upsert revision-chunk edge")
    }

    pub(super) async fn upsert_chunk_mentions_entity_edge(
        &self,
        state: &AppState,
        chunk_id: Uuid,
        entity_id: Uuid,
        score: Option<f64>,
        library_id: Uuid,
    ) -> Result<()> {
        state
            .graph_store
            .upsert_chunk_mentions_entity_edge(
                chunk_id,
                entity_id,
                Some(1),
                score,
                Some("graph_extract_entity_candidate".to_string()),
                library_id,
            )
            .await
            .context("failed to upsert chunk-mentions-entity edge")
    }
}

#[cfg(test)]
mod tests {
    use std::{borrow::Cow, error::Error as StdError, fmt};

    use sqlx::error::{DatabaseError, ErrorKind};

    use super::is_retryable_upsert_contention;

    #[derive(Debug)]
    struct FakeDatabaseError {
        code: &'static str,
    }

    impl fmt::Display for FakeDatabaseError {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("opaque database failure")
        }
    }

    impl StdError for FakeDatabaseError {}

    impl DatabaseError for FakeDatabaseError {
        fn message(&self) -> &str {
            "opaque database failure"
        }

        fn code(&self) -> Option<Cow<'_, str>> {
            Some(Cow::Borrowed(self.code))
        }

        fn as_error(&self) -> &(dyn StdError + Send + Sync + 'static) {
            self
        }

        fn as_error_mut(&mut self) -> &mut (dyn StdError + Send + Sync + 'static) {
            self
        }

        fn into_error(self: Box<Self>) -> Box<dyn StdError + Send + Sync + 'static> {
            self
        }

        fn kind(&self) -> ErrorKind {
            ErrorKind::Other
        }
    }

    fn database_error(code: &'static str) -> anyhow::Error {
        anyhow::Error::new(sqlx::Error::Database(Box::new(FakeDatabaseError { code })))
            .context("canonical graph upsert failed")
    }

    #[test]
    fn retries_only_typed_database_contention_codes() {
        assert!(is_retryable_upsert_contention(&database_error("40001")));
        assert!(is_retryable_upsert_contention(&database_error("40P01")));
        assert!(is_retryable_upsert_contention(&database_error("55P03")));
        assert!(!is_retryable_upsert_contention(&database_error("23505")));
    }

    #[test]
    fn legacy_conflict_text_does_not_trigger_retry() {
        let error = anyhow::anyhow!("409 write-write conflict");

        assert!(!is_retryable_upsert_contention(&error));
    }
}
