//! PostgreSQL coverage for the clean-v1 baseline knowledge revision states.

use anyhow::{Context, Result};
use ironrag_backend::app::config::Settings;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn baseline_enforces_authoritative_knowledge_revision_projection_states() -> Result<()> {
    let settings = Settings::from_env().context("load baseline migration settings")?;
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&settings.database_url)
        .await
        .context("connect baseline migration postgres")?;
    let database_name = format!("authoritative_revision_states_{}", Uuid::now_v7().simple());
    let database_url = replace_database_name(&settings.database_url, &database_name)?;

    sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
        .execute(&pool)
        .await
        .context("create baseline migration database")?;
    pool.close().await;

    let baseline_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .context("connect baseline migration database")?;
    let result = async {
        sqlx::raw_sql(include_str!("../../migrations/0001_init.sql"))
            .execute(&baseline_pool)
            .await
            .context("apply clean-v1 baseline")?;
        for (text_state, vector_state, graph_state) in [
            ("ready", "ready", "ready"),
            ("accepted", "vector_ready", "ready"),
            ("accepted", "ready", "graph_ready"),
        ] {
            let statement = format!(
                "insert into knowledge_revision (
                    revision_id, workspace_id, library_id, document_id, revision_number,
                    revision_state, revision_kind, mime_type, checksum, byte_size,
                    text_state, vector_state, graph_state, created_at
                 ) values (
                    gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), gen_random_uuid(), 1,
                    'ready', 'upload', 'text/plain', md5(gen_random_uuid()::text), 0,
                    '{text_state}', '{vector_state}', '{graph_state}', now()
                 )"
            );
            let rejected =
                sqlx::query(sqlx::AssertSqlSafe(statement)).execute(&baseline_pool).await;
            assert!(
                rejected.is_err(),
                "baseline must reject non-authoritative projection states: \
                 text={text_state}, vector={vector_state}, graph={graph_state}"
            );
        }
        Ok::<(), anyhow::Error>(())
    }
    .await;

    baseline_pool.close().await;
    let admin_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&settings.database_url)
        .await
        .context("reconnect baseline migration admin postgres")?;
    sqlx::query(
        "select pg_terminate_backend(pid) from pg_stat_activity
         where datname = $1 and pid <> pg_backend_pid()",
    )
    .bind(&database_name)
    .execute(&admin_pool)
    .await?;
    sqlx::query(sqlx::AssertSqlSafe(format!("drop database \"{database_name}\"")))
        .execute(&admin_pool)
        .await?;
    admin_pool.close().await;
    result
}

fn replace_database_name(database_url: &str, new_database: &str) -> Result<String> {
    let (without_query, query_suffix) = database_url
        .split_once('?')
        .map_or((database_url, None), |(prefix, suffix)| (prefix, Some(suffix)));
    let slash_index = without_query
        .rfind('/')
        .with_context(|| format!("database URL is missing database name: {database_url}"))?;
    let mut rebuilt = format!("{}{new_database}", &without_query[..=slash_index]);
    if let Some(query) = query_suffix {
        rebuilt.push('?');
        rebuilt.push_str(query);
    }
    Ok(rebuilt)
}
