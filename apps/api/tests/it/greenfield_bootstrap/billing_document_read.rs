use super::*;
use ironrag_backend::services::maintenance::{
    baseline_delta, billing_document_read_schema::migrate_billing_document_read_schema as migrate,
    vision_recognition_schema::migrate_vision_recognition_schema,
};

const PREDECESSOR_SQL: &str = include_str!("../../fixtures/schema/vision_recognition.sql");
const DELTA_SQL: &str = include_str!("../../../ops/apply-billing-document-read-schema.sql");

pub(super) async fn predecessor(pool: &PgPool) -> Result<()> {
    assert_eq!(
        hex::encode(Sha384::digest(PREDECESSOR_SQL.as_bytes())),
        baseline_delta::BASELINE_REVISION_VISION_RECOGNITION_SHA384
    );
    sqlx::raw_sql(PREDECESSOR_SQL).execute(pool).await?;
    record_baseline_checksum(pool, baseline_delta::BASELINE_REVISION_VISION_RECOGNITION_SHA384)
        .await
}

pub(super) async fn retained_rows(pool: &PgPool) -> Result<Value> {
    Ok(sqlx::query_scalar(
        "select jsonb_build_object(
        'workspaces',(select jsonb_agg(to_jsonb(t) order by id) from catalog_workspace t),
        'libraries',(select jsonb_agg(to_jsonb(t) order by id) from catalog_library t),
        'documents',(select jsonb_agg(to_jsonb(t) order by id) from content_document t),
        'revisions',(select jsonb_agg(to_jsonb(t) order by id) from content_revision t),
        'providers',(select jsonb_agg(to_jsonb(t) order by id) from ai_provider_catalog t),
        'models',(select jsonb_agg(to_jsonb(t) order by id) from ai_model_catalog t),
        'calls',(select jsonb_agg(to_jsonb(t) order by id) from billing_provider_call t),
        'usage',(select jsonb_agg(to_jsonb(t) order by id) from billing_usage t),
        'charges',(select jsonb_agg(to_jsonb(t) order by id) from billing_charge t),
        'costs',(select jsonb_agg(to_jsonb(t) order by id) from billing_execution_cost t))",
    )
    .fetch_one(pool)
    .await?)
}

pub(super) async fn seed_retained_rows(pool: &PgPool) -> Result<(Uuid, Uuid)> {
    let workspace =
        catalog_repository::create_workspace(pool, "billing-hop", "Billing hop", None).await?;
    let library = catalog_repository::create_library(
        pool,
        workspace.id,
        "billing-hop",
        "Billing hop",
        None,
        None,
    )
    .await?;
    let provider = Uuid::now_v7();
    let model = Uuid::now_v7();
    let call = Uuid::now_v7();
    let owner = Uuid::now_v7();
    let usage = Uuid::now_v7();
    let document = Uuid::now_v7();
    sqlx::query("insert into content_document(id,workspace_id,library_id,external_key,source_file_name) values($1,$2,$3,'retained','retained.png')")
        .bind(document).bind(workspace.id).bind(library.id).execute(pool).await?;
    sqlx::query("insert into content_revision(id,document_id,workspace_id,library_id,revision_number,content_source_kind,checksum,mime_type,byte_size,storage_key) values($1,$2,$3,$4,1,'upload','retained-checksum','image/png',17,'retained-source-key')")
        .bind(Uuid::now_v7()).bind(document).bind(workspace.id).bind(library.id).execute(pool).await?;
    sqlx::query("insert into ai_provider_catalog(id,provider_kind,display_name,api_style) values($1,'billing-hop','Billing hop',enum_first(null::ai_provider_api_style))")
        .bind(provider).execute(pool).await?;
    sqlx::query("insert into ai_model_catalog(id,provider_catalog_id,model_name,metadata_json) values($1,$2,'synthetic-preserved-model','{}')")
        .bind(model).bind(provider).execute(pool).await?;
    sqlx::query("insert into billing_provider_call(id,workspace_id,library_id,owning_execution_kind,owning_execution_id,provider_catalog_id,model_catalog_id,call_kind,call_state,completed_at) values($1,$2,$3,'binding_validation',$4,$5,$6,'vision','completed',now())")
        .bind(call).bind(workspace.id).bind(library.id).bind(owner).bind(provider).bind(model).execute(pool).await?;
    sqlx::query("insert into billing_usage(id,provider_call_id,usage_kind,billing_unit,quantity) values($1,$2,'prompt_tokens','per_1m_input_tokens',17)")
        .bind(usage).bind(call).execute(pool).await?;
    sqlx::query("insert into billing_charge(usage_id,currency_code,unit_price,total_price) values($1,'USD',1000,0.017)")
        .bind(usage).execute(pool).await?;
    sqlx::query("insert into billing_execution_cost(owning_execution_kind,owning_execution_id,total_cost,currency_code,provider_call_count,workspace_id,library_id) values('binding_validation',$1,0.017,'USD',1,$2,$3)")
        .bind(owner).bind(workspace.id).bind(library.id).execute(pool).await?;
    Ok((workspace.id, library.id))
}

async fn refuses_without_stamping(pool: &PgPool) -> Result<()> {
    let rows = retained_rows(pool).await?;
    let checksum = baseline_checksum(pool).await?;
    for apply in [false, true] {
        assert!(migrate(pool, apply).await.is_err());
        assert_eq!(baseline_checksum(pool).await?, checksum);
        assert_eq!(retained_rows(pool).await?, rows);
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires local postgres service; uses an isolated disposable database"]
async fn exact_predecessor_preserves_data_and_enforces_native_scope() -> Result<()> {
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        predecessor(&pool).await?;
        let (workspace, library) = seed_retained_rows(&pool).await?;
        let before = retained_rows(&pool).await?;
        let checksum = baseline_checksum(&pool).await?;
        let pending = migrate(&pool, false).await?;
        assert!(!pending.already_current && !pending.schema_applied);
        assert_eq!(baseline_checksum(&pool).await?, checksum);
        assert_eq!(retained_rows(&pool).await?, before);
        let absent: bool = sqlx::query_scalar("select to_regclass('public.billing_document_read') is null")
            .fetch_one(&pool).await?;
        assert!(absent);

        sqlx::query("update _sqlx_migrations set checksum=decode(repeat('00',48),'hex') where version=1").execute(&pool).await?;
        refuses_without_stamping(&pool).await?;
        sqlx::query("update _sqlx_migrations set checksum=$1 where version=1").bind(&checksum).execute(&pool).await?;
        sqlx::query("drop index idx_knowledge_chunk_primary_block_fk").execute(&pool).await?;
        refuses_without_stamping(&pool).await?;
        sqlx::query("create index idx_knowledge_chunk_primary_block_fk on knowledge_chunk(primary_block_id) where primary_block_id is not null").execute(&pool).await?;
        sqlx::query("alter type billing_owning_execution_kind rename value 'binding_validation' to 'unexpected_validation'").execute(&pool).await?;
        refuses_without_stamping(&pool).await?;
        sqlx::query("alter type billing_owning_execution_kind rename value 'unexpected_validation' to 'binding_validation'").execute(&pool).await?;

        let applied = migrate(&pool, true).await?;
        assert!(applied.schema_applied && !applied.already_current);
        assert_eq!(retained_rows(&pool).await?, before);
        assert_eq!(hex::encode(baseline_checksum(&pool).await?), baseline_delta::BASELINE_REVISION_BILLING_DOCUMENT_READ_SHA384);
        assert!(persistence::validate_baseline_objects_present(&pool).await.is_err(), "document-read revision is a frozen search-billing predecessor, not current readiness");
        for apply in [false, true] {
            let current = migrate(&pool, apply).await?;
            assert!(current.already_current && !current.schema_applied);
            let previous = migrate_vision_recognition_schema(&pool, apply).await?;
            assert!(previous.already_current && !previous.schema_applied);
        }

        // Source IDs may outlive purged revisions, but the tenant pair cannot drift.
        let read = Uuid::now_v7();
        let insert = "insert into billing_document_read(id,workspace_id,library_id,document_id,revision_id) values($1,$2,$3,$4,$5)";
        sqlx::query(insert).bind(read).bind(workspace).bind(library)
            .bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&pool).await?;
        let rejected = sqlx::query(insert).bind(Uuid::now_v7()).bind(Uuid::now_v7()).bind(library)
            .bind(Uuid::now_v7()).bind(Uuid::now_v7()).execute(&pool).await.expect_err("wrong workspace must not cross library scope");
        assert_eq!(rejected.as_database_error().and_then(|error| error.code()).as_deref(), Some("23503"));
        sqlx::query("delete from catalog_library where id=$1").bind(library).execute(&pool).await?;
        let remaining: i64 = sqlx::query_scalar("select count(*) from billing_document_read where id=$1")
            .bind(read).fetch_one(&pool).await?;
        assert_eq!(remaining, 0);
        Ok(())
    }.await;
    pool.close().await;
    database.drop().await?;
    result
}

#[tokio::test]
#[ignore = "requires local postgres service; uses isolated disposable databases"]
async fn partial_or_preapplied_native_ddl_is_never_stamped() -> Result<()> {
    let settings = Settings::from_env()?;
    for ddl in [
        "create table public.billing_document_read(marker text)",
        "alter type public.billing_owning_execution_kind add value 'mcp_document_read'",
        "create index idx_billing_document_read_library_scope on catalog_library(id)",
        DELTA_SQL,
    ] {
        let database = TempDatabase::create(&settings.database_url).await?;
        let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
        let result = async {
            predecessor(&pool).await?;
            seed_retained_rows(&pool).await?;
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
async fn current_readiness_refuses_native_object_drift() -> Result<()> {
    let settings = Settings::from_env()?;
    let database = TempDatabase::create(&settings.database_url).await?;
    let pool = PgPoolOptions::new().max_connections(4).connect(&database.database_url).await?;
    let result = async {
        persistence::run_postgres_migrations(&pool).await?;
        persistence::validate_baseline_objects_present(&pool).await?;
        for (drift, restore) in [
            ("alter type billing_owning_execution_kind rename value 'mcp_document_read' to 'unexpected_read'",
             "alter type billing_owning_execution_kind rename value 'unexpected_read' to 'mcp_document_read'"),
            ("alter table billing_document_read alter column document_id drop not null",
             "alter table billing_document_read alter column document_id set not null"),
            ("alter table billing_document_read drop constraint billing_document_read_pkey",
             "alter table billing_document_read add constraint billing_document_read_pkey primary key(id)"),
            ("alter table billing_document_read drop constraint billing_document_read_library_scope_fkey",
             "alter table billing_document_read add constraint billing_document_read_library_scope_fkey foreign key(library_id,workspace_id) references catalog_library(id,workspace_id) on delete cascade"),
            ("alter table billing_document_read drop constraint billing_document_read_library_scope_fkey; alter table billing_document_read add constraint billing_document_read_library_scope_fkey foreign key(library_id,workspace_id) references catalog_library(id,workspace_id) on delete restrict",
             "alter table billing_document_read drop constraint billing_document_read_library_scope_fkey; alter table billing_document_read add constraint billing_document_read_library_scope_fkey foreign key(library_id,workspace_id) references catalog_library(id,workspace_id) on delete cascade"),
            ("alter table billing_document_read add constraint unexpected_source_fk foreign key(document_id) references content_document(id)",
             "alter table billing_document_read drop constraint unexpected_source_fk"),
            ("alter table billing_document_read alter column created_at set default statement_timestamp()",
             "alter table billing_document_read alter column created_at set default now()"),
            ("drop index idx_billing_document_read_library_scope",
             "create index idx_billing_document_read_library_scope on billing_document_read(library_id,workspace_id)"),
            ("drop index idx_billing_document_read_library_scope; create index idx_billing_document_read_library_scope on billing_document_read(workspace_id,library_id)",
             "drop index idx_billing_document_read_library_scope; create index idx_billing_document_read_library_scope on billing_document_read(library_id,workspace_id)"),
            ("drop table billing_document_read", DELTA_SQL.split_once(';').expect("enum statement precedes table DDL").1),
        ] {
            sqlx::raw_sql(sqlx::AssertSqlSafe(drift)).execute(&pool).await?;
            assert!(persistence::validate_baseline_objects_present(&pool).await.is_err());
            refuses_without_stamping(&pool).await?;
            for apply in [false, true] {
                assert!(migrate_vision_recognition_schema(&pool, apply).await.is_err(), "historical commands must validate the recorded current certificate");
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
