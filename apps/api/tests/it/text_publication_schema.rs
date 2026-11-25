use anyhow::{Context, Result};
use ironrag_backend::{
    app::config::Settings,
    services::maintenance::{
        baseline_delta::{
            BASELINE_REVISION_TEXT_PUBLICATION_SHA384, BASELINE_REVISION_USER_PREFERENCE_SHA384,
        },
        text_publication_schema::migrate_text_publication_schema,
        user_preference_schema::migrate_user_preference_schema,
    },
};
use sha2::{Digest as _, Sha384};
use sqlx::{PgPool, postgres::PgPoolOptions};

use crate::temp_postgres_database::TempPostgresDatabase;

const BASELINE: &str = include_str!("../../migrations/0001_init.sql");

fn predecessor_sql() -> String {
    let sql = BASELINE.replace(
        "r.admission_identity->'replacement'->>'fileName' AS source_file_name,\n       k.vector_state, k.vector_ready_at, k.graph_state, k.graph_ready_at",
        "r.admission_identity->'replacement'->>'fileName' AS source_file_name",
    );
    let sql = sql.replace(
        "      AND r.admission_identity->'replacement'->>'checksum' = r.checksum",
        "      AND k.vector_state = 'ready' AND k.vector_ready_at IS NOT NULL\n      AND k.graph_state IN ('ready', 'graph_degraded')\n      AND r.admission_identity->'replacement'->>'checksum' = r.checksum",
    );
    assert_eq!(
        hex::encode(Sha384::digest(sql.as_bytes())),
        BASELINE_REVISION_USER_PREFERENCE_SHA384
    );
    sql
}

async fn fixture(wrong_checksum: bool) -> Result<(TempPostgresDatabase, PgPool)> {
    let settings = Settings::from_env()?;
    let database = TempPostgresDatabase::create(&settings.database_url, "text_publication").await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let directory = tempfile::tempdir()?;
    let mut sql = predecessor_sql();
    if wrong_checksum {
        sql.push_str("\n-- isolated unknown baseline revision\n");
    }
    std::fs::write(directory.path().join("0001_init.sql"), sql)?;
    sqlx::migrate::Migrator::new(directory.path()).await?.run(&pool).await?;
    Ok((database, pool))
}

async fn checksum(pool: &PgPool) -> Result<String> {
    Ok(sqlx::query_scalar("select encode(checksum,'hex') from _sqlx_migrations where version=1")
        .fetch_one(pool)
        .await?)
}

async fn view_definition(pool: &PgPool) -> Result<String> {
    Ok(sqlx::query_scalar("select pg_get_viewdef('public.content_current_revision'::regclass)")
        .fetch_one(pool)
        .await?)
}

async fn replace_view(pool: &PgPool, definition: &str) -> Result<()> {
    // The definition comes from PostgreSQL's own view deparser in an isolated fixture.
    let sql = format!("CREATE OR REPLACE VIEW public.content_current_revision AS {definition}");
    sqlx::raw_sql(sqlx::AssertSqlSafe(sql)).execute(pool).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires isolated local postgres with pgvector and event-trigger privileges"]
async fn guarded_text_publication_transition_and_refusals() -> Result<()> {
    let (database, pool) = fixture(false).await?;
    let result: Result<()> = async {
        let predecessor = checksum(&pool).await?;
        assert_eq!(predecessor, BASELINE_REVISION_USER_PREFERENCE_SHA384);
        let pending = migrate_text_publication_schema(&pool, false).await?;
        assert!(!pending.schema_applied && !pending.already_current);
        assert_eq!(checksum(&pool).await?, predecessor);

        let original = view_definition(&pool).await?;
        for fence in ["sourceIdentity", "visibilityIntentId", "materializationToken"] {
            let tampered = original.replace(fence, "invalidFence");
            assert_ne!(tampered, original, "fixture must alter {fence}");
            replace_view(&pool, &tampered).await?;
            for apply in [false, true] {
                assert!(migrate_text_publication_schema(&pool, apply).await.is_err());
                assert_eq!(checksum(&pool).await?, predecessor);
            }
            replace_view(&pool, &original).await?;
        }

        // Inject an extra owned column only during CREATE VIEW. The predecessor
        // certificate passes, but target validation must roll back DDL and stamp.
        sqlx::raw_sql(
            "CREATE FUNCTION public.test_text_publication_drift() RETURNS event_trigger
             LANGUAGE plpgsql AS $$ BEGIN
               ALTER TABLE public.content_revision ADD COLUMN test_transition_drift boolean;
             END $$;
             CREATE EVENT TRIGGER test_text_publication_drift ON ddl_command_end
             WHEN TAG IN ('CREATE VIEW') EXECUTE FUNCTION public.test_text_publication_drift()",
        )
        .execute(&pool)
        .await?;
        assert!(migrate_text_publication_schema(&pool, true).await.is_err());
        assert_eq!(checksum(&pool).await?, predecessor);
        assert_eq!(view_definition(&pool).await?, original);
        let drift: bool = sqlx::query_scalar(
            "select exists(select 1 from pg_attribute where attrelid='public.content_revision'::regclass and attname='test_transition_drift' and not attisdropped)",
        )
        .fetch_one(&pool)
        .await?;
        assert!(!drift, "target certificate failure must roll back the injected DDL");
        sqlx::raw_sql("DROP EVENT TRIGGER test_text_publication_drift; DROP FUNCTION public.test_text_publication_drift()")
            .execute(&pool).await?;

        let applied = migrate_text_publication_schema(&pool, true).await?;
        assert!(applied.schema_applied && !applied.already_current);
        assert_eq!(checksum(&pool).await?, BASELINE_REVISION_TEXT_PUBLICATION_SHA384);
        for apply in [false, true] {
            let current = migrate_text_publication_schema(&pool, apply).await?;
            assert!(current.already_current && !current.schema_applied);
            let historical = migrate_user_preference_schema(&pool, apply).await?;
            assert!(historical.already_current && !historical.schema_applied);
        }

        let current_view = view_definition(&pool).await?;
        for fence in ["sourceIdentity", "visibilityIntentId", "materializationToken"] {
            let tampered = current_view.replace(fence, "invalidFence");
            assert_ne!(tampered, current_view, "fixture must alter {fence}");
            replace_view(&pool, &tampered).await?;
            for apply in [false, true] {
                assert!(migrate_text_publication_schema(&pool, apply).await.is_err());
                assert!(migrate_user_preference_schema(&pool, apply).await.is_err());
                assert_eq!(checksum(&pool).await?, BASELINE_REVISION_TEXT_PUBLICATION_SHA384);
            }
            replace_view(&pool, &current_view).await?;
        }
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result.context("guarded transition fixture failed")?;

    let (database, pool) = fixture(true).await?;
    let result: Result<()> = async {
        let unknown = checksum(&pool).await?;
        assert_ne!(unknown, BASELINE_REVISION_USER_PREFERENCE_SHA384);
        for apply in [false, true] {
            assert!(migrate_text_publication_schema(&pool, apply).await.is_err());
            assert_eq!(checksum(&pool).await?, unknown);
        }
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result.context("unknown checksum fixture failed")
}
