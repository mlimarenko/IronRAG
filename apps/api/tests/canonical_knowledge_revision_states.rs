//! PostgreSQL coverage for the forward-only knowledge projection state migration.

use std::{borrow::Cow, path::Path};

use anyhow::{Context, Result};
use ironrag_backend::app::config::Settings;
use sqlx::{PgPool, migrate::Migrator, postgres::PgPoolOptions};
use uuid::Uuid;

struct TempDatabase {
    name: String,
    admin_url: String,
    database_url: String,
}

impl TempDatabase {
    async fn create(base_database_url: &str) -> Result<Self> {
        let admin_url = replace_database_name(base_database_url, "postgres")?;
        let database_name = format!("canonical_revision_states_{}", Uuid::now_v7().simple());
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin_url)
            .await
            .context("connect projection-state migration admin postgres")?;

        terminate_database_connections(&admin_pool, &database_name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("drop stale projection-state database {database_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!("create database \"{database_name}\"")))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("create projection-state database {database_name}"))?;
        admin_pool.close().await;

        Ok(Self {
            database_url: replace_database_name(base_database_url, &database_name)?,
            admin_url,
            name: database_name,
        })
    }

    async fn drop(self) -> Result<()> {
        let admin_pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.admin_url)
            .await
            .context("reconnect projection-state migration admin postgres")?;
        terminate_database_connections(&admin_pool, &self.name).await?;
        sqlx::query(sqlx::AssertSqlSafe(format!("drop database if exists \"{}\"", self.name)))
            .execute(&admin_pool)
            .await
            .with_context(|| format!("drop projection-state database {}", self.name))?;
        admin_pool.close().await;
        Ok(())
    }
}

struct MigrationFixture {
    pool: PgPool,
    temp_database: TempDatabase,
}

impl MigrationFixture {
    async fn create() -> Result<Self> {
        let settings = Settings::from_env().context("load projection-state migration settings")?;
        let temp_database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .connect(&temp_database.database_url)
            .await
            .context("connect projection-state migration postgres")?;
        let migrations = Migrator::new(Path::new("./migrations"))
            .await
            .context("load projection-state migrations")?;
        baseline_migrator(&migrations)
            .run(&pool)
            .await
            .context("apply public migration baseline through 0015")?;
        Ok(Self { pool, temp_database })
    }

    async fn cleanup(self) -> Result<()> {
        self.pool.close().await;
        self.temp_database.drop().await
    }
}

fn baseline_migrator(source: &Migrator) -> Migrator {
    Migrator {
        migrations: Cow::Owned(
            source
                .iter()
                .filter(|migration| (1..=15).contains(&migration.version))
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
        table_name: Cow::Borrowed("_sqlx_migrations"),
        create_schemas: Cow::Borrowed(&[]),
    }
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

async fn terminate_database_connections(postgres: &PgPool, database_name: &str) -> Result<()> {
    sqlx::query(
        "select pg_terminate_backend(pid)
         from pg_stat_activity
         where datname = $1
           and pid <> pg_backend_pid()",
    )
    .bind(database_name)
    .execute(postgres)
    .await
    .with_context(|| format!("terminate connections for {database_name}"))?;
    Ok(())
}

async fn seed_projection_states(pool: &PgPool, values_sql: &str) -> Result<()> {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    sqlx::query(
        "insert into knowledge_document (
            document_id,
            workspace_id,
            library_id,
            external_key,
            title,
            document_state,
            active_revision_id,
            readable_revision_id,
            latest_revision_no,
            created_at,
            updated_at
         ) values ($1, $2, $3, $4, $5, 'active', null, null, null, now(), now())",
    )
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .bind(format!("projection-state-{document_id}"))
    .bind("Projection state fixture")
    .execute(pool)
    .await
    .context("seed projection-state knowledge document")?;

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "insert into knowledge_revision (
            revision_id,
            workspace_id,
            library_id,
            document_id,
            revision_number,
            revision_state,
            revision_kind,
            storage_ref,
            source_uri,
            document_hint,
            mime_type,
            checksum,
            title,
            byte_size,
            normalized_text,
            text_checksum,
            image_checksum,
            text_state,
            vector_state,
            graph_state,
            text_readable_at,
            vector_ready_at,
            graph_ready_at,
            superseded_by_revision_id,
            created_at
         )
         select
            gen_random_uuid(),
            '{workspace_id}'::uuid,
            '{library_id}'::uuid,
            '{document_id}'::uuid,
            row_number() over (),
            'ready',
            'upload',
            null,
            null,
            null,
            'text/plain',
            md5(row_number() over ()::text),
            'Projection state fixture',
            0,
            '',
            null,
            null,
            fixture.text_state,
            fixture.vector_state,
            fixture.graph_state,
            null,
            null,
            null,
            null,
            now()
         from (
            values {values_sql}
         ) as fixture(text_state, vector_state, graph_state)"
    )))
    .execute(pool)
    .await
    .context("seed historical projection states")?;
    Ok(())
}

async fn apply_migration_0016(pool: &PgPool) -> Result<()> {
    let mut transaction = pool.begin().await?;
    sqlx::raw_sql(include_str!("../migrations/0016_canonical_knowledge_revision_states.sql"))
        .execute(&mut *transaction)
        .await
        .context("apply canonical projection-state migration")?;
    transaction.commit().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn migration_0016_rewrites_historical_states_and_enforces_canonical_values() -> Result<()> {
    let fixture = MigrationFixture::create().await?;
    seed_projection_states(
        &fixture.pool,
        "('readable', 'vector_ready', 'graph_ready'),
         ('ready', 'ready', 'ready'),
         ('text_readable', 'processing', 'graph_degraded')",
    )
    .await?;

    apply_migration_0016(&fixture.pool).await?;

    let states = sqlx::query_as::<_, (String, String, String)>(
        "select text_state, vector_state, graph_state
         from knowledge_revision
         order by text_state, vector_state, graph_state",
    )
    .fetch_all(&fixture.pool)
    .await?;
    assert_eq!(
        states,
        vec![
            ("text_readable".into(), "processing".into(), "graph_degraded".into()),
            ("text_readable".into(), "ready".into(), "ready".into()),
            ("text_readable".into(), "ready".into(), "ready".into()),
        ]
    );

    let mut transaction = fixture.pool.begin().await?;
    for (text_state, vector_state, graph_state) in [
        ("ready", "ready", "ready"),
        ("accepted", "vector_ready", "ready"),
        ("accepted", "ready", "graph_ready"),
    ] {
        sqlx::query("savepoint invalid_projection_state").execute(&mut *transaction).await?;
        let rejected = sqlx::query(
            "insert into knowledge_revision (
                revision_id,
                workspace_id,
                library_id,
                document_id,
                revision_number,
                revision_state,
                revision_kind,
                mime_type,
                checksum,
                byte_size,
                text_state,
                vector_state,
                graph_state,
                created_at
             )
             select
                gen_random_uuid(),
                workspace_id,
                library_id,
                document_id,
                coalesce(latest_revision_no, 0) + 100,
                'ready',
                'upload',
                'text/plain',
                md5(gen_random_uuid()::text),
                0,
                $1,
                $2,
                $3,
                now()
             from knowledge_document
             limit 1",
        )
        .bind(text_state)
        .bind(vector_state)
        .bind(graph_state)
        .execute(&mut *transaction)
        .await;
        assert!(
            rejected.is_err(),
            "historical projection state must be rejected: {text_state}/{vector_state}/{graph_state}"
        );
        sqlx::query("rollback to savepoint invalid_projection_state")
            .execute(&mut *transaction)
            .await?;
    }

    transaction.rollback().await?;
    fixture.cleanup().await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service"]
async fn migration_0016_aborts_without_rewriting_unknown_states() -> Result<()> {
    let fixture = MigrationFixture::create().await?;
    seed_projection_states(&fixture.pool, "('unknown', 'ready', 'ready')").await?;

    let mut transaction = fixture.pool.begin().await?;
    let migration =
        sqlx::raw_sql(include_str!("../migrations/0016_canonical_knowledge_revision_states.sql"))
            .execute(&mut *transaction)
            .await;
    assert!(migration.is_err(), "unknown projection states must fail closed");

    transaction.rollback().await?;
    fixture.cleanup().await?;
    Ok(())
}
