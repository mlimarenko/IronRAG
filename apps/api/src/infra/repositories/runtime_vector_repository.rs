use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct RuntimeVectorTargetRow {
    pub id: Uuid,
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: Option<i32>,
    pub embedding_json: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct RuntimeVectorTargetUpsertInput {
    pub library_id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub provider_kind: String,
    pub model_name: String,
    pub dimensions: Option<i32>,
    pub embedding_json: serde_json::Value,
}

pub async fn upsert_runtime_vector_target(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
    provider_kind: &str,
    model_name: &str,
    dimensions: Option<i32>,
    embedding_json: serde_json::Value,
) -> Result<RuntimeVectorTargetRow, sqlx::Error> {
    sqlx::query_as::<_, RuntimeVectorTargetRow>(
        "insert into runtime_vector_target (
            id, library_id, target_kind, target_id, provider_kind, model_name, dimensions, embedding_json
         ) values ($1, $2, $3, $4, $5, $6, $7, $8)
         on conflict (library_id, target_kind, target_id, provider_kind, model_name) do update
         set dimensions = excluded.dimensions,
             embedding_json = excluded.embedding_json,
             updated_at = now()
         returning id, library_id, target_kind, target_id, provider_kind, model_name,
            dimensions, embedding_json, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .bind(provider_kind)
    .bind(model_name)
    .bind(dimensions)
    .bind(embedding_json)
    .fetch_one(pool)
    .await
}

fn coalesce_runtime_vector_target_upserts(
    rows: &[RuntimeVectorTargetUpsertInput],
) -> Vec<RuntimeVectorTargetUpsertInput> {
    let mut deduped = BTreeMap::new();
    for row in rows {
        deduped.insert(
            (
                row.library_id,
                row.target_kind.clone(),
                row.target_id,
                row.provider_kind.clone(),
                row.model_name.clone(),
            ),
            row.clone(),
        );
    }
    deduped.into_values().collect()
}

pub async fn upsert_runtime_vector_targets(
    pool: &PgPool,
    rows: &[RuntimeVectorTargetUpsertInput],
) -> Result<(), sqlx::Error> {
    let rows = coalesce_runtime_vector_target_upserts(rows);
    if rows.is_empty() {
        return Ok(());
    }

    let mut builder = QueryBuilder::<Postgres>::new(
        "insert into runtime_vector_target (
            id, library_id, target_kind, target_id, provider_kind, model_name, dimensions, embedding_json
         ) ",
    );
    builder.push_values(rows.iter(), |mut row_builder, row| {
        row_builder
            .push_bind(Uuid::now_v7())
            .push_bind(row.library_id)
            .push_bind(&row.target_kind)
            .push_bind(row.target_id)
            .push_bind(&row.provider_kind)
            .push_bind(&row.model_name)
            .push_bind(row.dimensions)
            .push_bind(&row.embedding_json);
    });
    builder.push(
        " on conflict (library_id, target_kind, target_id, provider_kind, model_name) do update
          set dimensions = excluded.dimensions,
              embedding_json = excluded.embedding_json,
              updated_at = now()
          where runtime_vector_target.dimensions is distinct from excluded.dimensions
             or runtime_vector_target.embedding_json is distinct from excluded.embedding_json",
    );
    builder.build().execute(pool).await?;
    Ok(())
}

pub async fn list_runtime_vector_targets_by_library_and_kind(
    pool: &PgPool,
    library_id: Uuid,
    target_kind: &str,
    provider_kind: &str,
    model_name: &str,
) -> Result<Vec<RuntimeVectorTargetRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeVectorTargetRow>(
        "select id, library_id, target_kind, target_id, provider_kind, model_name,
            dimensions, embedding_json, created_at, updated_at
         from runtime_vector_target
         where library_id = $1
           and target_kind = $2
           and provider_kind = $3
           and model_name = $4
         order by updated_at desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(provider_kind)
    .bind(model_name)
    .fetch_all(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn runtime_vector_target_batch_coalesces_duplicate_keys_last_write_wins() {
        let library_id = Uuid::now_v7();
        let target_id = Uuid::now_v7();
        let rows = coalesce_runtime_vector_target_upserts(&[
            RuntimeVectorTargetUpsertInput {
                library_id,
                target_kind: "entity".to_string(),
                target_id,
                provider_kind: "openai".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                dimensions: Some(1536),
                embedding_json: json!([0.1, 0.2]),
            },
            RuntimeVectorTargetUpsertInput {
                library_id,
                target_kind: "entity".to_string(),
                target_id,
                provider_kind: "openai".to_string(),
                model_name: "text-embedding-3-small".to_string(),
                dimensions: Some(1536),
                embedding_json: json!([0.9, 1.0]),
            },
        ]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].embedding_json, json!([0.9, 1.0]));
    }
}
