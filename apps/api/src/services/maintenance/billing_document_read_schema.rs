//! Additive native-read attribution using the existing provider billing ledger.

use sqlx::PgPool;

use super::baseline_delta::{AdditiveBaselineDeltaSpec, apply_additive_baseline_delta};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_DOCUMENT_READ,
    REQUIRED_BASELINE_OBJECT_GROUPS_VISION_RECOGNITION,
};

pub(crate) const ENUM_CERTIFICATE: &str = "select coalesce((
    select array_agg(enumlabel::text order by enumsortorder) = array[
        'ingest_attempt','query_execution','graph_extraction_attempt',
        'binding_validation','mcp_document_read']
    from pg_enum where enumtypid=to_regtype('public.billing_owning_execution_kind')
), false)";

pub(crate) const TABLE_CERTIFICATE: &str = "select exists (
    select 1 from pg_class where oid=to_regclass('public.billing_document_read')
    and relkind='r' and relpersistence='p'
) and (select count(*)=6 from pg_attribute
    where attrelid=to_regclass('public.billing_document_read') and attnum>0 and not attisdropped
) and not exists (
    select 1 from (values
        (1,'id','uuid',null::text), (2,'workspace_id','uuid',null::text),
        (3,'library_id','uuid',null::text), (4,'document_id','uuid',null::text),
        (5,'revision_id','uuid',null::text), (6,'created_at','timestamptz','now()')
    ) expected(position,name,type_name,default_expression)
    left join pg_attribute a on a.attrelid=to_regclass('public.billing_document_read')
        and a.attnum=expected.position and not a.attisdropped
    left join pg_attrdef d on d.adrelid=a.attrelid and d.adnum=a.attnum
    where a.attname is distinct from expected.name
        or a.atttypid is distinct from to_regtype(expected.type_name)::oid
        or a.attnotnull is distinct from true or a.attidentity<>'' or a.attgenerated<>''
        or pg_get_expr(d.adbin,d.adrelid) is distinct from expected.default_expression
) and exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.billing_document_read')
    and conname='billing_document_read_pkey' and contype='p' and convalidated
    and not condeferrable and conkey=array[1]::smallint[]
)";

pub(crate) const SCOPE_CERTIFICATE: &str = "select (
    select count(*)=1 from pg_constraint
    where conrelid=to_regclass('public.billing_document_read') and contype='f'
) and exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.billing_document_read')
    and conname='billing_document_read_library_scope_fkey' and contype='f'
    and confrelid=to_regclass('public.catalog_library') and convalidated
    and not condeferrable and confdeltype='c' and confupdtype='a' and confmatchtype='s'
    and conkey=array[3,2]::smallint[] and confkey=array[
        (select attnum from pg_attribute where attrelid=to_regclass('public.catalog_library') and attname='id'),
        (select attnum from pg_attribute where attrelid=to_regclass('public.catalog_library') and attname='workspace_id')
    ]
)";

pub(crate) const INDEX_CERTIFICATE: &str = "select exists (
    select 1 from pg_index i join pg_class c on c.oid=i.indexrelid
    join pg_am am on am.oid=c.relam
    where i.indexrelid=to_regclass('public.idx_billing_document_read_library_scope')
    and i.indrelid=to_regclass('public.billing_document_read') and am.amname='btree'
    and i.indisvalid and i.indisready and i.indislive and not i.indisunique
    and i.indnkeyatts=2 and i.indnatts=2 and i.indkey[0]=3 and i.indkey[1]=2
    and i.indexprs is null and i.indpred is null
)";

pub(crate) const BILLING_DOCUMENT_READ_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate billing-document-read-schema",
        predecessor_sha384_hex: super::baseline_delta::BASELINE_REVISION_VISION_RECOGNITION_SHA384,
        target_sha384_hex: super::baseline_delta::BASELINE_REVISION_BILLING_DOCUMENT_READ_SHA384,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_VISION_RECOGNITION,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_DOCUMENT_READ,
        additive_sql: include_str!("../../../ops/apply-billing-document-read-schema.sql"),
        // The predecessor certificates predate this owner and do not certify
        // the billing enum. Accept its exact old or new shape here; the shared
        // stale-DDL and recorded-target checks distinguish those two states.
        precondition: Some((
            "select coalesce((select array_agg(enumlabel::text order by enumsortorder) in (
                array['ingest_attempt','query_execution','graph_extraction_attempt','binding_validation'],
                array['ingest_attempt','query_execution','graph_extraction_attempt','binding_validation','mcp_document_read']
            ) from pg_enum where enumtypid=to_regtype('public.billing_owning_execution_kind')), false)",
            "billing owner enum does not match a certified native-read predecessor or target",
        )),
        delta_present: (
            "select to_regclass('public.billing_document_read') is not null
                or to_regclass('public.idx_billing_document_read_library_scope') is not null
                or exists (select 1 from pg_enum
                    where enumtypid=to_regtype('public.billing_owning_execution_kind')
                    and enumlabel='mcp_document_read')",
            "native-read billing DDL is already present under its predecessor checksum; refusing to stamp partial or pre-applied state",
        ),
        predecessor_error: "baseline checksum is not the exact native-read billing predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingDocumentReadSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_billing_document_read_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<BillingDocumentReadSchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &BILLING_DOCUMENT_READ_DELTA, apply).await?;
    Ok(BillingDocumentReadSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}
