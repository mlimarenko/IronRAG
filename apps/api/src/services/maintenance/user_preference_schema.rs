//! Guarded account-scoped telemetry consent preference.

use sqlx::PgPool;

use super::baseline_delta::{AdditiveBaselineDeltaSpec, apply_additive_baseline_delta};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH,
    REQUIRED_BASELINE_OBJECT_GROUPS_USER_PREFERENCE,
};

pub(crate) const TABLE_CERTIFICATE: &str = "select exists (
    select 1 from pg_class where oid=to_regclass('public.iam_user_preference')
    and relkind='r' and relpersistence='p'
) and (select count(*)=3 from pg_attribute
    where attrelid=to_regclass('public.iam_user_preference') and attnum>0 and not attisdropped
) and not exists (
    select 1 from (values
        (1,'principal_id','uuid',null::text),
        (2,'telemetry_consent','text',null::text),
        (3,'updated_at','timestamptz','now()')
    ) expected(position,name,type_name,default_expression)
    left join pg_attribute a on a.attrelid=to_regclass('public.iam_user_preference')
        and a.attnum=expected.position and not a.attisdropped
    left join pg_attrdef d on d.adrelid=a.attrelid and d.adnum=a.attnum
    where a.attname is distinct from expected.name
        or a.atttypid is distinct from to_regtype(expected.type_name)::oid
        or a.attnotnull is distinct from true or a.attidentity<>'' or a.attgenerated<>''
        or pg_get_expr(d.adbin,d.adrelid) is distinct from expected.default_expression
) and exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.iam_user_preference')
    and conname='iam_user_preference_pkey' and contype='p' and convalidated
    and not condeferrable and conkey=array[1]::smallint[]
)";

pub(crate) const CONSENT_CERTIFICATE: &str = "select exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.iam_user_preference')
    and conname='iam_user_preference_telemetry_consent_check' and contype='c'
    and convalidated and not condeferrable and conkey=array[2]::smallint[]
    and pg_get_constraintdef(oid) = 'CHECK ((telemetry_consent = ANY (ARRAY[''granted''::text, ''denied''::text])))'
) and exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.iam_user_preference')
    and conname='iam_user_preference_principal_id_fkey' and contype='f'
    and confrelid=to_regclass('public.iam_principal') and convalidated
    and not condeferrable and confdeltype='c' and confupdtype='a' and confmatchtype='s'
    and conkey=array[1]::smallint[] and confkey=array[
        (select attnum from pg_attribute where attrelid=to_regclass('public.iam_principal') and attname='id')
    ]
)";

pub(crate) const USER_PREFERENCE_DELTA: AdditiveBaselineDeltaSpec = AdditiveBaselineDeltaSpec {
    operator_command: "ironrag-maintenance migrate user-preference-schema",
    predecessor_sha384_hex:
        super::baseline_delta::BASELINE_REVISION_PROVIDER_SOURCE_GENERATION_FENCE_SHA384,
    target_sha384_hex: super::baseline_delta::BASELINE_REVISION_USER_PREFERENCE_SHA384,
    prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH,
    required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_USER_PREFERENCE,
    additive_sql: include_str!("../../../ops/apply-user-preference-schema.sql"),
    precondition: None,
    delta_present: (
        "select to_regclass('public.iam_user_preference') is not null
            or exists (select 1 from pg_constraint
                where conname in (
                    'iam_user_preference_pkey',
                    'iam_user_preference_telemetry_consent_check',
                    'iam_user_preference_principal_id_fkey'
                ))",
        "account telemetry preference DDL is already present under its predecessor checksum; refusing to stamp partial or pre-applied state",
    ),
    predecessor_error: "baseline checksum is not the exact user-preference predecessor; refusing to apply or stamp",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserPreferenceSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_user_preference_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<UserPreferenceSchemaMigrationReport> {
    let transition = apply_additive_baseline_delta(postgres, &USER_PREFERENCE_DELTA, apply).await?;
    Ok(UserPreferenceSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}
