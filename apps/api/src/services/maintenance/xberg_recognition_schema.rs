//! One guarded policy cutover; historical extraction provenance is never rewritten.

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Mutex};

use anyhow::Context as _;
use futures::StreamExt as _;
use sha2::{Digest as _, Sha256};
use sqlx::{PgConnection, PgPool};

use super::baseline_delta::{
    AdditiveBaselineDeltaSpec, BaselineDeltaSetup, apply_additive_baseline_delta_with_setup,
};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_CONTENT_VISIBILITY_INTENT,
    REQUIRED_BASELINE_OBJECT_GROUPS_XBERG_RECOGNITION,
};

pub const PREDECESSOR_BASELINE_SHA384: &str =
    super::baseline_delta::BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384;

const PREDECESSOR_CONSTRAINT: &str = "CHECK (((recognition_policy ? 'rasterImageEngine'::text) AND ((recognition_policy ->> 'rasterImageEngine'::text) = ANY (ARRAY['docling'::text, 'vision'::text])) AND ((recognition_policy - 'rasterImageEngine'::text) = '{}'::jsonb)))";

pub(crate) const BASELINE_HISTORY_CERTIFICATE: &str =
    "select count(*)=1 and coalesce(bool_and(version=1 and success),false) from _sqlx_migrations";

pub(crate) const POLICY_STORAGE_CERTIFICATE: &str = "select exists (
    select 1 from information_schema.columns where table_schema='public'
    and table_name='catalog_library' and column_name='recognition_policy'
    and udt_name='jsonb' and is_nullable='NO'
    and column_default=$default$'{\"rasterImageEngine\": \"vision\"}'::jsonb$default$
    ) and exists (select 1 from pg_constraint c where c.conrelid='public.catalog_library'::regclass
    and c.conname='catalog_library_recognition_policy_object_check'
    and c.contype='c' and c.convalidated and not c.connoinherit
    and pg_get_constraintdef(c.oid)=$constraint$CHECK ((jsonb_typeof(recognition_policy) = 'object'::text))$constraint$)";

/// Exact catalog constraint and current policy certificate, including validation state.
pub(crate) const CURRENT_POLICY_CERTIFICATE: &str = "select exists (
    select 1 from pg_constraint c where c.conrelid='public.catalog_library'::regclass
    and c.conname='catalog_library_recognition_policy_raster_engine_check'
    and c.contype='c' and c.convalidated and not c.connoinherit
    and pg_get_constraintdef(c.oid) = $constraint$CHECK (((recognition_policy ? 'rasterImageEngine'::text) AND ((recognition_policy ->> 'rasterImageEngine'::text) = ANY (ARRAY['xberg'::text, 'vision'::text])) AND ((recognition_policy - 'rasterImageEngine'::text) = '{}'::jsonb)))$constraint$
    ) and not exists (select 1 from public.catalog_library where
        recognition_policy is null or jsonb_typeof(recognition_policy) <> 'object'
        or not recognition_policy ? 'rasterImageEngine'
        or recognition_policy->>'rasterImageEngine' is null
        or recognition_policy->>'rasterImageEngine' not in ('xberg','vision')
        or recognition_policy - 'rasterImageEngine' <> '{}'::jsonb)";

pub(crate) const XBERG_RECOGNITION_DELTA: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
    operator_command: "ironrag-maintenance migrate xberg-recognition-schema",
    predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
    target_sha384_hex: super::baseline_delta::BASELINE_REVISION_XBERG_RECOGNITION_SHA384,
    required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_XBERG_RECOGNITION,
    prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_CONTENT_VISIBILITY_INTENT,
    additive_sql: include_str!("../../../ops/apply-xberg-recognition-schema.sql"),
    precondition: None,
    delta_present: (
        "select exists (select 1 from public.catalog_library where recognition_policy->>'rasterImageEngine'='xberg') or exists (select 1 from pg_constraint where conrelid='public.catalog_library'::regclass and conname='catalog_library_recognition_policy_raster_engine_check' and position('xberg' in pg_get_constraintdef(oid))>0)",
        "Xberg policy or constraint already present under predecessor checksum; refusing partial/pre-applied cutover",
    ),
    predecessor_error: "Xberg recognition requires the exact content-visibility-intent predecessor baseline",
};

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct XbergRecognitionSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_xberg_recognition_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<XbergRecognitionSchemaMigrationReport> {
    let setup = RecognitionPreservation::default();
    let transition = apply_additive_baseline_delta_with_setup(
        postgres,
        &XBERG_RECOGNITION_DELTA,
        apply,
        Some(&setup),
    )
    .await?;
    Ok(XbergRecognitionSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}

async fn public_tables(connection: &mut PgConnection) -> anyhow::Result<Vec<String>> {
    Ok(sqlx::query_scalar("select relname::text from pg_class where relnamespace='public'::regnamespace and relkind in ('r','p') order by relname::text collate \"C\"")
        .fetch_all(connection).await?)
}

/// Called before predecessor object/policy checks. All public row owners are
/// quiesced, including history, credentials and every dimensional vector shard.
/// The shared five-second lock timeout bounds each lock acquisition.
pub(crate) async fn lock_preservation_owners(connection: &mut PgConnection) -> anyhow::Result<()> {
    for table in public_tables(connection).await? {
        let quoted = table.replace('"', "\"\"");
        sqlx::raw_sql(sqlx::AssertSqlSafe(format!(
            "LOCK TABLE ONLY public.\"{quoted}\" IN SHARE ROW EXCLUSIVE MODE"
        )))
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

type PreservationInventory = BTreeMap<String, (u64, String)>;

#[derive(Default)]
struct RecognitionPreservation {
    before: Mutex<Option<PreservationInventory>>,
}

impl BaselineDeltaSetup for RecognitionPreservation {
    fn prepare<'a>(
        &'a self,
        connection: &'a mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let history_valid: bool = sqlx::query_scalar(BASELINE_HISTORY_CERTIFICATE)
                .fetch_one(&mut *connection)
                .await?;
            anyhow::ensure!(
                history_valid,
                "recognition cutover requires exactly one successful baseline migration record"
            );
            let storage_valid: bool =
                sqlx::query_scalar(POLICY_STORAGE_CERTIFICATE).fetch_one(&mut *connection).await?;
            anyhow::ensure!(
                storage_valid,
                "recognition policy storage/default/object constraint is unknown or partial"
            );
            let exact: bool = sqlx::query_scalar("select exists (select 1 from pg_constraint c where c.conrelid='public.catalog_library'::regclass and c.conname='catalog_library_recognition_policy_raster_engine_check' and c.contype='c' and c.convalidated and not c.connoinherit and pg_get_constraintdef(c.oid)=$1)")
                .bind(PREDECESSOR_CONSTRAINT).fetch_one(&mut *connection).await?;
            anyhow::ensure!(exact, "recognition predecessor constraint is unknown or partial");
            let valid: bool = sqlx::query_scalar("select not exists (select 1 from public.catalog_library where recognition_policy is null or jsonb_typeof(recognition_policy)<>'object' or not recognition_policy ? 'rasterImageEngine' or recognition_policy->>'rasterImageEngine' is null or recognition_policy->>'rasterImageEngine' not in ('docling','vision') or recognition_policy - 'rasterImageEngine' <> '{}'::jsonb)")
                .fetch_one(&mut *connection).await?;
            anyhow::ensure!(valid, "recognition predecessor contains unknown or partial policies");
            let idle: bool = sqlx::query_scalar("select not exists (select 1 from content_mutation where mutation_state in ('accepted','running')) and not exists (select 1 from ingest_job where queue_state in ('queued','leased','paused')) and not exists (select 1 from ingest_attempt where attempt_state in ('leased','running'))")
                .fetch_one(&mut *connection).await?;
            anyhow::ensure!(
                idle,
                "stop/drain API, workers, importers and maintenance writers before recognition cutover"
            );
            let before = preservation_inventory(connection).await?;
            *self
                .before
                .lock()
                .map_err(|_| anyhow::anyhow!("preservation inventory poisoned"))? = Some(before);
            Ok(())
        })
    }

    fn verify<'a>(
        &'a self,
        connection: &'a mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let after = preservation_inventory(connection).await?;
            let before = self
                .before
                .lock()
                .map_err(|_| anyhow::anyhow!("preservation inventory poisoned"))?;
            anyhow::ensure!(
                before.as_ref() == Some(&after),
                "recognition cutover preservation inventory changed; rolling back without stamping"
            );
            Ok(())
        })
    }
}

/// Streams sorted SHA-256 row digests, not row values, with constant client
/// memory. The only normalized field is this hop's current library policy.
/// Publication pointers, admission identities, opaque extraction provenance,
/// source keys/checksums, credentials and vector payloads remain byte-exact JSON.
async fn preservation_inventory(
    connection: &mut PgConnection,
) -> anyhow::Result<PreservationInventory> {
    let mut inventory = BTreeMap::new();
    for table in public_tables(connection).await? {
        if table == "_sqlx_migrations" {
            // The shared transaction owns and CAS-stamps this one row after verification.
            continue;
        }
        let quoted = table.replace('"', "\"\"");
        let row = if table == "catalog_library" {
            "case when t.recognition_policy->>'rasterImageEngine'='docling' then jsonb_set(to_jsonb(t), '{recognition_policy,rasterImageEngine}', '\"xberg\"'::jsonb, false) else to_jsonb(t) end"
        } else {
            "to_jsonb(t)"
        };
        let query = format!(
            "select encode(digest(({row})::text,'sha256'),'hex') as fingerprint from only public.\"{quoted}\" t order by 1"
        );
        let mut rows =
            sqlx::query_scalar::<_, String>(sqlx::AssertSqlSafe(query)).fetch(&mut *connection);
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        while let Some(row) = rows.next().await {
            digest.update(row.context("read preservation row digest")?.as_bytes());
            count += 1;
        }
        inventory.insert(table, (count, hex::encode(digest.finalize())));
    }
    // Certify typed vector dimensions even when a shard has no rows.
    let columns: Vec<String> = sqlx::query_scalar("select c.relname||':'||a.attname||':'||format_type(a.atttypid,a.atttypmod) from pg_class c join pg_attribute a on a.attrelid=c.oid where c.relnamespace='public'::regnamespace and c.relkind in ('r','p') and a.attnum>0 and not a.attisdropped order by c.relname::text collate \"C\",a.attnum")
        .fetch_all(connection).await?;
    let mut digest = Sha256::new();
    for column in &columns {
        digest.update(column.as_bytes());
        digest.update([0]);
    }
    inventory.insert(
        "schema:column-types".into(),
        (columns.len() as u64, hex::encode(digest.finalize())),
    );
    Ok(inventory)
}
