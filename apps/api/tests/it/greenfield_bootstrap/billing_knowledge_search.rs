use super::*;
use ironrag_backend::services::maintenance::{
    baseline_delta, billing_document_read_schema::migrate_billing_document_read_schema,
    billing_knowledge_search_schema::migrate_billing_knowledge_search_schema as migrate,
};

const DELTA_SQL: &str = include_str!("../../../ops/apply-billing-knowledge-search-schema.sql");

async fn predecessor(pool: &PgPool) -> Result<()> {
    billing_document_read::predecessor(pool).await?;
    let applied = migrate_billing_document_read_schema(pool, true).await?;
    assert!(applied.schema_applied && !applied.already_current);
    assert_eq!(
        hex::encode(baseline_checksum(pool).await?),
        baseline_delta::BASELINE_REVISION_BILLING_DOCUMENT_READ_SHA384
    );
    Ok(())
}

async fn retained_rows(pool: &PgPool) -> Result<Value> {
    let ledger = billing_document_read::retained_rows(pool).await?;
    let reads: Value = sqlx::query_scalar(
        "select coalesce(jsonb_agg(to_jsonb(t) order by id),'[]'::jsonb) from billing_document_read t",
    )
    .fetch_one(pool)
    .await?;
    Ok(serde_json::json!({"ledger":ledger,"reads":reads}))
}

async fn refuses_without_stamping(pool: &PgPool) -> Result<()> {
    let before = retained_rows(pool).await?;
    let checksum = baseline_checksum(pool).await?;
    for apply in [false, true] {
        assert!(migrate(pool, apply).await.is_err());
        assert_eq!(baseline_checksum(pool).await?, checksum);
        assert_eq!(retained_rows(pool).await?, before);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service; uses an isolated disposable database"]
async fn exact_predecessor_preserves_billing_and_certifies_search_scope() -> Result<()> {
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        predecessor(&pool).await?;
        let (workspace, library) = billing_document_read::seed_retained_rows(&pool).await?;
        sqlx::query("insert into billing_document_read(id,workspace_id,library_id,document_id,revision_id) values($1,$2,$3,$4,$5)")
            .bind(Uuid::now_v7()).bind(workspace).bind(library).bind(Uuid::now_v7()).bind(Uuid::now_v7())
            .execute(&pool).await?;
        let before = retained_rows(&pool).await?;
        let checksum = baseline_checksum(&pool).await?;
        let pending = migrate(&pool, false).await?;
        assert!(!pending.schema_applied && !pending.already_current);
        assert_eq!(baseline_checksum(&pool).await?, checksum);
        assert_eq!(retained_rows(&pool).await?, before);
        let absent: bool = sqlx::query_scalar("select to_regclass('public.billing_knowledge_search') is null")
            .fetch_one(&pool).await?;
        assert!(absent);

        sqlx::query("update _sqlx_migrations set checksum=decode(repeat('00',48),'hex') where version=1")
            .execute(&pool).await?;
        refuses_without_stamping(&pool).await?;
        sqlx::query("update _sqlx_migrations set checksum=$1 where version=1").bind(&checksum).execute(&pool).await?;
        sqlx::query("drop index idx_billing_document_read_library_scope").execute(&pool).await?;
        refuses_without_stamping(&pool).await?;
        sqlx::query("create index idx_billing_document_read_library_scope on billing_document_read(library_id,workspace_id)")
            .execute(&pool).await?;

        let applied = migrate(&pool, true).await?;
        assert!(applied.schema_applied && !applied.already_current);
        assert_eq!(retained_rows(&pool).await?, before);
        assert_eq!(hex::encode(baseline_checksum(&pool).await?), baseline_delta::BASELINE_REVISION_BILLING_KNOWLEDGE_SEARCH_SHA384);
        assert!(persistence::validate_baseline_objects_present(&pool).await.is_err(), "knowledge-search revision is a frozen user-preference predecessor, not current readiness");
        for apply in [false, true] {
            let current = migrate(&pool, apply).await?;
            assert!(current.already_current && !current.schema_applied);
            let previous = migrate_billing_document_read_schema(&pool, apply).await?;
            assert!(previous.already_current && !previous.schema_applied, "a historical enum precondition must not reject a certified later revision");
        }
        let search = Uuid::now_v7();
        sqlx::query("insert into billing_knowledge_search(id,workspace_id,library_id) values($1,$2,$3)")
            .bind(search).bind(workspace).bind(library).execute(&pool).await?;
        let error = sqlx::query("insert into billing_knowledge_search(id,workspace_id,library_id) values($1,$2,$3)")
            .bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(library).execute(&pool).await.expect_err("cross-tenant owner must refuse");
        assert_eq!(error.as_database_error().and_then(|error| error.code()).as_deref(), Some("23503"));
        sqlx::query("delete from catalog_library where id=$1").bind(library).execute(&pool).await?;
        let count: i64 = sqlx::query_scalar("select count(*) from billing_knowledge_search where id=$1")
            .bind(search).fetch_one(&pool).await?;
        assert_eq!(count, 0);
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service; uses isolated disposable databases"]
async fn partial_or_preapplied_search_ddl_is_never_stamped() -> Result<()> {
    let settings = Settings::from_env()?;
    for ddl in [
        "create table public.billing_knowledge_search(marker text)",
        "alter type public.billing_owning_execution_kind add value 'knowledge_search'",
        "create index idx_billing_knowledge_search_library_scope on catalog_library(id)",
        DELTA_SQL,
    ] {
        let database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
        let result = async {
            predecessor(&pool).await?;
            billing_document_read::seed_retained_rows(&pool).await?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(ddl)).execute(&pool).await?;
            refuses_without_stamping(&pool).await
        }
        .await;
        pool.close().await;
        database.drop().await?;
        result?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service; uses an isolated disposable database"]
async fn current_readiness_and_historical_commands_refuse_search_object_drift() -> Result<()> {
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        persistence::run_postgres_migrations(&pool).await?;
        persistence::validate_baseline_objects_present(&pool).await?;
        for (drift, restore) in [
            ("alter type billing_owning_execution_kind rename value 'knowledge_search' to 'unexpected_search'",
             "alter type billing_owning_execution_kind rename value 'unexpected_search' to 'knowledge_search'"),
            ("alter table billing_knowledge_search alter column workspace_id drop not null",
             "alter table billing_knowledge_search alter column workspace_id set not null"),
            ("alter table billing_knowledge_search drop constraint billing_knowledge_search_pkey",
             "alter table billing_knowledge_search add constraint billing_knowledge_search_pkey primary key(id)"),
            ("alter table billing_knowledge_search drop constraint billing_knowledge_search_library_scope_fkey",
             "alter table billing_knowledge_search add constraint billing_knowledge_search_library_scope_fkey foreign key(library_id,workspace_id) references catalog_library(id,workspace_id) on delete cascade"),
            ("alter table billing_knowledge_search drop constraint billing_knowledge_search_library_scope_fkey; alter table billing_knowledge_search add constraint billing_knowledge_search_library_scope_fkey foreign key(library_id,workspace_id) references catalog_library(id,workspace_id) on delete restrict",
             "alter table billing_knowledge_search drop constraint billing_knowledge_search_library_scope_fkey; alter table billing_knowledge_search add constraint billing_knowledge_search_library_scope_fkey foreign key(library_id,workspace_id) references catalog_library(id,workspace_id) on delete cascade"),
            ("alter table billing_knowledge_search alter column created_at set default statement_timestamp()",
             "alter table billing_knowledge_search alter column created_at set default now()"),
            ("drop index idx_billing_knowledge_search_library_scope",
             "create index idx_billing_knowledge_search_library_scope on billing_knowledge_search(library_id,workspace_id)"),
            ("drop index idx_billing_knowledge_search_library_scope; create index idx_billing_knowledge_search_library_scope on billing_knowledge_search(workspace_id,library_id)",
             "drop index idx_billing_knowledge_search_library_scope; create index idx_billing_knowledge_search_library_scope on billing_knowledge_search(library_id,workspace_id)"),
            ("drop table billing_knowledge_search", DELTA_SQL.split_once(';').expect("enum statement precedes table DDL").1),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(drift)).execute(&pool).await?;
            assert!(persistence::validate_baseline_objects_present(&pool).await.is_err());
            refuses_without_stamping(&pool).await?;
            for apply in [false, true] {
                assert!(migrate_billing_document_read_schema(&pool, apply).await.is_err(), "historical commands must check the current revision certificate");
            }
            sqlx::raw_sql(sqlx::AssertSqlSafe(restore)).execute(&pool).await?;
            persistence::validate_baseline_objects_present(&pool).await?;
        }
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result
}
