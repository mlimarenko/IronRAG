use super::*;
use ironrag_backend::services::maintenance::{
    baseline_delta, billing_document_read_schema::migrate_billing_document_read_schema,
    billing_knowledge_search_schema::migrate_billing_knowledge_search_schema,
    provider_source_generation_fence_schema::migrate_provider_source_generation_fence,
    user_preference_schema::migrate_user_preference_schema as migrate,
};

const DELTA_SQL: &str = include_str!("../../../ops/apply-user-preference-schema.sql");

async fn predecessor(pool: &PgPool) -> Result<()> {
    billing_document_read::predecessor(pool).await?;
    let documents = migrate_billing_document_read_schema(pool, true).await?;
    assert!(documents.schema_applied && !documents.already_current);
    assert_eq!(
        hex::encode(baseline_checksum(pool).await?),
        baseline_delta::BASELINE_REVISION_BILLING_DOCUMENT_READ_SHA384
    );
    let search = migrate_billing_knowledge_search_schema(pool, true).await?;
    assert!(search.schema_applied && !search.already_current);
    let fence = migrate_provider_source_generation_fence(pool, true).await?;
    assert!(fence.schema_applied && !fence.already_current);
    assert_eq!(
        hex::encode(baseline_checksum(pool).await?),
        baseline_delta::BASELINE_REVISION_PROVIDER_SOURCE_GENERATION_FENCE_SHA384
    );
    Ok(())
}

async fn preference_absent(pool: &PgPool) -> Result<bool> {
    Ok(sqlx::query_scalar("select to_regclass('public.iam_user_preference') is null")
        .fetch_one(pool)
        .await?)
}

#[tokio::test]
#[ignore = "requires local postgres service; uses an isolated disposable database"]
async fn additive_user_preference_schema_requires_exact_predecessor_and_cascades() -> Result<()> {
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        predecessor(&pool).await?;
        let predecessor_checksum = baseline_checksum(&pool).await?;

        let pending = migrate(&pool, false).await?;
        assert!(!pending.schema_applied && !pending.already_current);
        assert_eq!(baseline_checksum(&pool).await?, predecessor_checksum);
        assert!(preference_absent(&pool).await?, "dry-run must not create the table");

        // An unknown checksum is neither the predecessor nor a passed revision.
        sqlx::query("update _sqlx_migrations set checksum=$1 where version=1")
            .bind(vec![0_u8; 48])
            .execute(&pool)
            .await?;
        for apply in [false, true] {
            assert!(migrate(&pool, apply).await.is_err());
        }
        assert_eq!(baseline_checksum(&pool).await?, vec![0_u8; 48]);
        assert!(preference_absent(&pool).await?);
        sqlx::query("update _sqlx_migrations set checksum=$1 where version=1")
            .bind(&predecessor_checksum)
            .execute(&pool)
            .await?;

        // Pre-applied DDL under the exact predecessor is inspected, never stamped.
        sqlx::raw_sql(sqlx::AssertSqlSafe(DELTA_SQL)).execute(&pool).await?;
        for apply in [false, true] {
            assert!(migrate(&pool, apply).await.is_err());
        }
        assert_eq!(baseline_checksum(&pool).await?, predecessor_checksum);
        sqlx::raw_sql("drop table public.iam_user_preference").execute(&pool).await?;

        let applied = migrate(&pool, true).await?;
        assert!(applied.schema_applied && !applied.already_current);
        assert_eq!(
            hex::encode(baseline_checksum(&pool).await?),
            baseline_delta::BASELINE_REVISION_USER_PREFERENCE_SHA384
        );
        persistence::validate_baseline_objects_present(&pool).await?;

        for apply in [false, true] {
            let current = migrate(&pool, apply).await?;
            assert!(current.already_current && !current.schema_applied);
        }

        // The consent check admits only the two stored states.
        let principal = Uuid::now_v7();
        sqlx::query(
            "insert into iam_principal (id, principal_kind, display_label)
             values ($1, 'user', 'Consent Test')",
        )
        .bind(principal)
        .execute(&pool)
        .await?;
        let rejected = sqlx::query(
            "insert into iam_user_preference (principal_id, telemetry_consent)
             values ($1, 'unset')",
        )
        .bind(principal)
        .execute(&pool)
        .await
        .expect_err("unset must not be a stored consent value");
        assert_eq!(
            rejected.as_database_error().and_then(|error| error.code()).as_deref(),
            Some("23514")
        );

        sqlx::query(
            "insert into iam_user_preference (principal_id, telemetry_consent)
             values ($1, 'denied')",
        )
        .bind(principal)
        .execute(&pool)
        .await?;
        sqlx::query("delete from iam_principal where id=$1").bind(principal).execute(&pool).await?;
        let remaining: i64 =
            sqlx::query_scalar("select count(*) from iam_user_preference where principal_id=$1")
                .bind(principal)
                .fetch_one(&pool)
                .await?;
        assert_eq!(remaining, 0);
        Ok(())
    }
    .await;
    pool.close().await;
    database.drop().await?;
    result
}
