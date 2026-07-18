//! `migrate vector-profile-keys` — re-key legacy per-model vector rows onto
//! the canonical embedding-execution-profile fingerprint.
//!
//! Before the execution-profile hardening, chunk/entity vector rows and their
//! `knowledge_vector_relation_manifest` lane were keyed by the bare
//! `model_catalog_id` UUID. The query path now looks vectors up exclusively by
//! `ResolvedRuntimeBinding::embedding_execution_profile_key()`, so every
//! pre-hardening library fails closed with a state conflict even though its
//! stored vectors are byte-for-byte valid for the active profile.
//!
//! This migration proves identity instead of re-embedding: a library is
//! re-keyed only when its active `EmbedChunk` binding still resolves to the
//! same `model_catalog_id` the legacy rows were written under (and, when the
//! binding declares dimensions, the manifest lane dimension matches). Anything
//! that cannot be proven — mixed old/new lanes, retargeted bindings, foreign
//! legacy models — is left untouched with a warning prescribing the paid
//! `rebuild vector-plane` path. Idempotent: a second run finds no legacy lanes
//! and reports zero work.

use anyhow::{Context, Result};
use serde::Serialize;
use sqlx::{Postgres, Transaction};
use tracing::{info, warn};
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::ai::AiBindingPurpose, infra::repositories::catalog_repository,
    services::ai_catalog_service::ResolvedRuntimeBinding,
};

const CHUNK_VECTOR_RELATION_PREFIX: &str = "knowledge_chunk_vector_d";
const ENTITY_VECTOR_RELATION_PREFIX: &str = "knowledge_entity_vector_d";

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct VectorProfileKeyMigrationReport {
    pub libraries_rekeyed: usize,
    pub libraries_without_legacy_lanes: usize,
    pub libraries_skipped_unprovable: usize,
    pub vector_rows_rekeyed: u64,
    pub manifest_lanes_rekeyed: u64,
}

impl VectorProfileKeyMigrationReport {
    const fn merge(self, other: Self) -> Self {
        Self {
            libraries_rekeyed: self.libraries_rekeyed + other.libraries_rekeyed,
            libraries_without_legacy_lanes: self.libraries_without_legacy_lanes
                + other.libraries_without_legacy_lanes,
            libraries_skipped_unprovable: self.libraries_skipped_unprovable
                + other.libraries_skipped_unprovable,
            vector_rows_rekeyed: self.vector_rows_rekeyed + other.vector_rows_rekeyed,
            manifest_lanes_rekeyed: self.manifest_lanes_rekeyed + other.manifest_lanes_rekeyed,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct LegacyManifestLane {
    dim: i32,
    vector_kind: String,
    relation_name: String,
}

/// Re-key legacy vector lanes for one library or every library.
pub async fn legacy_vector_profile_keys(
    state: &AppState,
    library_filter: Option<Uuid>,
    dry_run: bool,
) -> Result<VectorProfileKeyMigrationReport> {
    let mut libraries =
        catalog_repository::list_libraries(&state.persistence.postgres, None).await?;
    if let Some(target) = library_filter {
        libraries.retain(|library| library.id == target);
    }
    if libraries.is_empty() {
        anyhow::bail!("no libraries matched vector-profile-key migration target");
    }

    let mut totals = VectorProfileKeyMigrationReport::default();
    let mut failed_libraries = 0_usize;
    for library in libraries {
        match migrate_library(state, library.workspace_id, library.id, dry_run).await {
            Ok(counts) => totals = totals.merge(counts),
            Err(error) => {
                failed_libraries += 1;
                warn!(
                    library_id = %library.id,
                    library_name = %library.display_name,
                    ?error,
                    "vector-profile-key migration failed; continuing with next library",
                );
            }
        }
    }

    anyhow::ensure!(
        failed_libraries == 0,
        "vector-profile-key migration failed for {failed_libraries} librar{}",
        if failed_libraries == 1 { "y" } else { "ies" }
    );
    Ok(totals)
}

async fn migrate_library(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    dry_run: bool,
) -> Result<VectorProfileKeyMigrationReport> {
    let mut report = VectorProfileKeyMigrationReport::default();

    let Some(binding) = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await
        .map_err(anyhow::Error::new)?
    else {
        // Without a binding there is no target fingerprint; a library that has
        // legacy lanes but no binding cannot be proven and must stay untouched.
        let legacy_lane_count: i64 = sqlx::query_scalar(
            "select count(*) from knowledge_vector_relation_manifest
             where library_id = $1 and embedding_model_key not like 'embedding-profile:v1:%'",
        )
        .bind(library_id)
        .fetch_one(&state.persistence.postgres)
        .await
        .context("failed to count legacy manifest lanes without a binding")?;
        if legacy_lane_count > 0 {
            report.libraries_skipped_unprovable += 1;
            warn!(
                library_id = %library_id,
                legacy_lane_count,
                "library has legacy vector lanes but no active embed_chunk binding; \
                 configure the binding, then re-run the migration or rebuild the vector plane",
            );
        } else {
            report.libraries_without_legacy_lanes += 1;
        }
        return Ok(report);
    };

    let legacy_key = binding.model_catalog_id.to_string();
    let canonical_key = binding.embedding_execution_profile_key();

    let legacy_lanes: Vec<LegacyManifestLane> = sqlx::query_as(
        "select dim, vector_kind, relation_name
         from knowledge_vector_relation_manifest
         where library_id = $1 and embedding_model_key = $2
         order by vector_kind, dim",
    )
    .bind(library_id)
    .bind(&legacy_key)
    .fetch_all(&state.persistence.postgres)
    .await
    .context("failed to list legacy manifest lanes")?;

    if legacy_lanes.is_empty() {
        let foreign_legacy: i64 = sqlx::query_scalar(
            "select count(*) from knowledge_vector_relation_manifest
             where library_id = $1
               and embedding_model_key not like 'embedding-profile:v1:%'
               and embedding_model_key <> $2",
        )
        .bind(library_id)
        .bind(&legacy_key)
        .fetch_one(&state.persistence.postgres)
        .await
        .context("failed to count foreign legacy manifest lanes")?;
        if foreign_legacy > 0 {
            report.libraries_skipped_unprovable += 1;
            warn!(
                library_id = %library_id,
                foreign_legacy_lanes = foreign_legacy,
                active_model_catalog_id = %binding.model_catalog_id,
                "library has legacy vector lanes for a model the active embed_chunk \
                 binding no longer targets; run `rebuild vector-plane` instead",
            );
        } else {
            report.libraries_without_legacy_lanes += 1;
        }
        return Ok(report);
    }

    let canonical_lane_count: i64 = sqlx::query_scalar(
        "select count(*) from knowledge_vector_relation_manifest
         where library_id = $1 and embedding_model_key = $2",
    )
    .bind(library_id)
    .bind(&canonical_key)
    .fetch_one(&state.persistence.postgres)
    .await
    .context("failed to count canonical manifest lanes")?;
    if canonical_lane_count > 0 {
        report.libraries_skipped_unprovable += 1;
        warn!(
            library_id = %library_id,
            legacy_lanes = legacy_lanes.len(),
            canonical_lanes = canonical_lane_count,
            "library mixes legacy and canonical vector lanes; \
             run `rebuild vector-plane` instead of re-keying",
        );
        return Ok(report);
    }

    for lane in &legacy_lanes {
        validate_legacy_lane(library_id, lane, &binding)?;
    }

    if dry_run {
        let mut vector_rows = 0_u64;
        for lane in &legacy_lanes {
            let relation = validated_relation_identifier(&lane.relation_name)?;
            let count: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
                "select count(*) from {relation}
                 where library_id = $1 and embedding_model_key = $2 and vector_kind = $3",
            )))
            .bind(library_id)
            .bind(&legacy_key)
            .bind(&lane.vector_kind)
            .fetch_one(&state.persistence.postgres)
            .await
            .with_context(|| format!("failed to count legacy rows in {relation}"))?;
            vector_rows += u64::try_from(count).unwrap_or_default();
        }
        report.libraries_rekeyed += 1;
        report.vector_rows_rekeyed += vector_rows;
        report.manifest_lanes_rekeyed += legacy_lanes.len() as u64;
        info!(
            library_id = %library_id,
            vector_rows,
            manifest_lanes = legacy_lanes.len(),
            "dry-run: library would be re-keyed onto the canonical embedding profile",
        );
        return Ok(report);
    }

    let mut transaction = state.persistence.postgres.begin().await.with_context(|| {
        format!("failed to start vector-profile-key transaction for library {library_id}")
    })?;
    let parent_locked = catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await
    .with_context(|| format!("failed to lock library {library_id} for vector re-key"))?;
    anyhow::ensure!(parent_locked, "library {library_id} disappeared during vector re-key");

    let mut vector_rows = 0_u64;
    for lane in &legacy_lanes {
        vector_rows += rekey_lane(&mut transaction, library_id, lane, &legacy_key, &canonical_key)
            .await
            .with_context(|| {
                format!("failed to re-key lane {} for library {library_id}", lane.relation_name)
            })?;
    }
    let manifest_lanes = sqlx::query(
        "update knowledge_vector_relation_manifest
         set embedding_model_key = $3
         where library_id = $1 and embedding_model_key = $2",
    )
    .bind(library_id)
    .bind(&legacy_key)
    .bind(&canonical_key)
    .execute(&mut *transaction)
    .await
    .context("failed to re-key manifest lanes")?
    .rows_affected();
    transaction.commit().await.context("failed to commit vector-profile-key migration")?;

    report.libraries_rekeyed += 1;
    report.vector_rows_rekeyed += vector_rows;
    report.manifest_lanes_rekeyed += manifest_lanes;
    info!(
        library_id = %library_id,
        vector_rows,
        manifest_lanes,
        "library re-keyed onto the canonical embedding profile",
    );
    Ok(report)
}

async fn rekey_lane(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    lane: &LegacyManifestLane,
    legacy_key: &str,
    canonical_key: &str,
) -> Result<u64> {
    let relation = validated_relation_identifier(&lane.relation_name)?;
    let updated = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update {relation}
         set embedding_model_key = $4
         where library_id = $1 and embedding_model_key = $2 and vector_kind = $3",
    )))
    .bind(library_id)
    .bind(legacy_key)
    .bind(&lane.vector_kind)
    .bind(canonical_key)
    .execute(&mut **transaction)
    .await?
    .rows_affected();
    Ok(updated)
}

fn validate_legacy_lane(
    library_id: Uuid,
    lane: &LegacyManifestLane,
    binding: &ResolvedRuntimeBinding,
) -> Result<()> {
    let dim = u64::try_from(lane.dim)
        .with_context(|| format!("legacy manifest lane dimension {} is negative", lane.dim))?;
    let expected_relation = format!("{}{dim}", expected_relation_prefix(&lane.vector_kind)?);
    anyhow::ensure!(
        lane.relation_name == expected_relation,
        "legacy manifest lane for library {library_id} points at relation {} \
         but dimension {dim} of kind {} maps to {expected_relation}",
        lane.relation_name,
        lane.vector_kind,
    );
    if lane.vector_kind == "chunk_embedding"
        && let Some(declared) = binding.effective_embedding_dimensions
    {
        anyhow::ensure!(
            declared.get() == dim,
            "library {library_id} legacy chunk lane has dimension {dim} but the active \
             embed_chunk binding declares {}; run `rebuild vector-plane` instead",
            declared.get(),
        );
    }
    Ok(())
}

fn expected_relation_prefix(vector_kind: &str) -> Result<&'static str> {
    match vector_kind {
        "chunk_embedding" => Ok(CHUNK_VECTOR_RELATION_PREFIX),
        "entity_embedding" => Ok(ENTITY_VECTOR_RELATION_PREFIX),
        other => anyhow::bail!("unknown legacy vector kind {other}"),
    }
}

/// The relation name is interpolated into SQL, so it must round-trip through a
/// strict allowlist: a known per-dim prefix followed by decimal digits only.
fn validated_relation_identifier(relation_name: &str) -> Result<&str> {
    let digits = relation_name
        .strip_prefix(CHUNK_VECTOR_RELATION_PREFIX)
        .or_else(|| relation_name.strip_prefix(ENTITY_VECTOR_RELATION_PREFIX))
        .with_context(|| format!("relation {relation_name} is not a per-dim vector relation"))?;
    anyhow::ensure!(
        !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()),
        "relation {relation_name} does not end in a plain decimal dimension"
    );
    Ok(relation_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_per_dim_vector_relations() {
        assert!(validated_relation_identifier("knowledge_chunk_vector_d3072").is_ok());
        assert!(validated_relation_identifier("knowledge_entity_vector_d1024").is_ok());
        assert!(validated_relation_identifier("knowledge_chunk_vector_d").is_err());
        assert!(
            validated_relation_identifier("knowledge_chunk_vector_d3072; drop table x").is_err()
        );
        assert!(validated_relation_identifier("pg_catalog.pg_tables").is_err());
    }

    #[test]
    fn maps_vector_kinds_to_relation_prefixes() {
        assert_eq!(
            expected_relation_prefix("chunk_embedding").unwrap(),
            "knowledge_chunk_vector_d"
        );
        assert_eq!(
            expected_relation_prefix("entity_embedding").unwrap(),
            "knowledge_entity_vector_d"
        );
        assert!(expected_relation_prefix("graph_summary").is_err());
    }
}
