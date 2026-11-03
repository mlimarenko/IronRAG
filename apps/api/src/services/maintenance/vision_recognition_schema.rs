//! Guarded singleton Vision policy hop. Historical extraction provenance is untouched.

use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicU64, Ordering},
};

use sqlx::{PgConnection, PgPool};

use super::baseline_delta::{
    AdditiveBaselineDeltaSpec, BaselineDeltaSetup, apply_additive_baseline_delta_with_setup,
};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_VISION_RECOGNITION,
    REQUIRED_BASELINE_OBJECT_GROUPS_XBERG_RECOGNITION,
};

pub const PREDECESSOR_BASELINE_SHA384: &str =
    super::baseline_delta::BASELINE_REVISION_XBERG_RECOGNITION_SHA384;

pub(crate) const CURRENT_POLICY_CERTIFICATE: &str = "select exists (
    select 1 from pg_constraint c where c.conrelid='public.catalog_library'::regclass
    and c.conname='catalog_library_recognition_policy_raster_engine_check'
    and c.contype='c' and c.convalidated and not c.connoinherit
    and pg_get_constraintdef(c.oid)=$constraint$CHECK ((recognition_policy = '{\"rasterImageEngine\": \"vision\"}'::jsonb))$constraint$
    ) and not exists (select 1 from public.catalog_library
        where recognition_policy is distinct from '{\"rasterImageEngine\":\"vision\"}'::jsonb)";

pub(crate) const VISION_RECOGNITION_DELTA: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
    operator_command: "ironrag-maintenance migrate vision-recognition-schema",
    predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
    target_sha384_hex: super::baseline_delta::BASELINE_REVISION_VISION_RECOGNITION_SHA384,
    required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_VISION_RECOGNITION,
    prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_XBERG_RECOGNITION,
    additive_sql: include_str!("../../../ops/apply-vision-recognition-schema.sql"),
    precondition: None,
    delta_present: (
        "select exists (select 1 from pg_constraint where conrelid='public.catalog_library'::regclass and conname='catalog_library_recognition_policy_raster_engine_check' and pg_get_constraintdef(oid)=$constraint$CHECK ((recognition_policy = '{\"rasterImageEngine\": \"vision\"}'::jsonb))$constraint$)",
        "Vision constraint already present under predecessor checksum; refusing partial/pre-applied cutover",
    ),
    predecessor_error: "Vision recognition requires the exact frozen Xberg recognition predecessor baseline",
};

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VisionRecognitionSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
    pub policies_changed: u64,
}

pub async fn migrate_vision_recognition_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<VisionRecognitionSchemaMigrationReport> {
    let preservation = PolicyPreservation::default();
    let transition = apply_additive_baseline_delta_with_setup(
        postgres,
        &VISION_RECOGNITION_DELTA,
        apply,
        Some(&preservation),
    )
    .await?;
    Ok(VisionRecognitionSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
        policies_changed: if transition.schema_applied {
            preservation.conversions.load(Ordering::Relaxed)
        } else {
            0
        },
    })
}

/// Lock only the row owner this hop mutates, before checking predecessor objects.
/// The shared transaction supplies the advisory lock and bounded lock timeout.
pub(crate) async fn lock_policy_owner(connection: &mut PgConnection) -> anyhow::Result<()> {
    sqlx::raw_sql("LOCK TABLE public.catalog_library IN SHARE ROW EXCLUSIVE MODE")
        .execute(connection)
        .await?;
    Ok(())
}

#[derive(Default)]
struct PolicyPreservation {
    conversions: AtomicU64,
}

impl BaselineDeltaSetup for PolicyPreservation {
    fn prepare<'a>(
        &'a self,
        connection: &'a mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Reject user code that could mutate another owner or suppress/rewrite an UPDATE.
            // Internal FK triggers remain safe: no key or reference column is changed.
            let safe: bool = sqlx::query_scalar("select not exists (select 1 from pg_trigger t where tgrelid='public.catalog_library'::regclass and not tgisinternal and (tgtype::int & 16) <> 0 and (cardinality(tgattr::smallint[])=0 or (select attnum from pg_attribute where attrelid=t.tgrelid and attname='recognition_policy')=any(tgattr::smallint[]))) and not exists (select 1 from pg_rewrite where ev_class='public.catalog_library'::regclass) and not exists (select 1 from pg_inherits where inhparent='public.catalog_library'::regclass or inhrelid='public.catalog_library'::regclass)")
                .fetch_one(&mut *connection).await?;
            anyhow::ensure!(
                safe,
                "catalog policy owner has unexpected triggers, rules or inheritance; refusing cutover"
            );
            let idle: bool = sqlx::query_scalar("select not exists (select 1 from content_mutation where mutation_state in ('accepted','running')) and not exists (select 1 from ingest_job where queue_state in ('queued','leased','paused')) and not exists (select 1 from ingest_attempt where attempt_state in ('leased','running'))")
                .fetch_one(&mut *connection).await?;
            anyhow::ensure!(
                idle,
                "stop/drain API, workers, importers and maintenance writers before Vision cutover"
            );
            // Per-row certificate permits only this exact substitution. It also catches
            // insert/delete, timestamp changes and unexpected effects on other libraries.
            sqlx::raw_sql("CREATE TEMP TABLE vision_policy_before ON COMMIT DROP AS SELECT id, recognition_policy = '{\"rasterImageEngine\":\"xberg\"}'::jsonb AS needs_conversion, jsonb_set(to_jsonb(t), '{recognition_policy}', '{\"rasterImageEngine\":\"vision\"}'::jsonb, false) AS expected FROM public.catalog_library t")
                .execute(&mut *connection).await?;
            let count: i64 = sqlx::query_scalar(
                "select count(*) from pg_temp.vision_policy_before where needs_conversion",
            )
            .fetch_one(&mut *connection)
            .await?;
            self.conversions.store(u64::try_from(count)?, Ordering::Relaxed);
            Ok(())
        })
    }

    fn verify<'a>(
        &'a self,
        connection: &'a mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let preserved: bool = sqlx::query_scalar("select not exists (select 1 from pg_temp.vision_policy_before b full join public.catalog_library t on t.id=b.id where b.id is null or t.id is null or to_jsonb(t) is distinct from b.expected)")
                .fetch_one(connection).await?;
            anyhow::ensure!(
                preserved,
                "Vision policy per-row preservation certificate changed; rolling back without stamping"
            );
            Ok(())
        })
    }
}
