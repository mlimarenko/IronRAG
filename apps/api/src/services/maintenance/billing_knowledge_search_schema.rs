//! Guarded standalone knowledge-search attribution in the existing billing ledger.

use sqlx::PgPool;

use super::baseline_delta::{AdditiveBaselineDeltaSpec, apply_additive_baseline_delta};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_DOCUMENT_READ,
    REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH,
};

pub(crate) const ENUM_CERTIFICATE: &str = "select coalesce((
    select array_agg(enumlabel::text order by enumsortorder) = array[
        'ingest_attempt','query_execution','graph_extraction_attempt',
        'binding_validation','mcp_document_read','knowledge_search']
    from pg_enum where enumtypid=to_regtype('public.billing_owning_execution_kind')
), false)";

pub(crate) const TABLE_CERTIFICATE: &str = "select exists (
    select 1 from pg_class where oid=to_regclass('public.billing_knowledge_search')
    and relkind='r' and relpersistence='p'
) and (select count(*)=4 from pg_attribute
    where attrelid=to_regclass('public.billing_knowledge_search') and attnum>0 and not attisdropped
) and not exists (
    select 1 from (values
        (1,'id','uuid',null::text), (2,'workspace_id','uuid',null::text),
        (3,'library_id','uuid',null::text), (4,'created_at','timestamptz','now()')
    ) expected(position,name,type_name,default_expression)
    left join pg_attribute a on a.attrelid=to_regclass('public.billing_knowledge_search')
        and a.attnum=expected.position and not a.attisdropped
    left join pg_attrdef d on d.adrelid=a.attrelid and d.adnum=a.attnum
    where a.attname is distinct from expected.name
        or a.atttypid is distinct from to_regtype(expected.type_name)::oid
        or a.attnotnull is distinct from true or a.attidentity<>'' or a.attgenerated<>''
        or pg_get_expr(d.adbin,d.adrelid) is distinct from expected.default_expression
) and exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.billing_knowledge_search')
    and conname='billing_knowledge_search_pkey' and contype='p' and convalidated
    and not condeferrable and conkey=array[1]::smallint[]
)";

pub(crate) const SCOPE_CERTIFICATE: &str = "select (
    select count(*)=1 from pg_constraint
    where conrelid=to_regclass('public.billing_knowledge_search') and contype='f'
) and exists (
    select 1 from pg_constraint where conrelid=to_regclass('public.billing_knowledge_search')
    and conname='billing_knowledge_search_library_scope_fkey' and contype='f'
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
    where i.indexrelid=to_regclass('public.idx_billing_knowledge_search_library_scope')
    and i.indrelid=to_regclass('public.billing_knowledge_search') and am.amname='btree'
    and i.indisvalid and i.indisready and i.indislive and not i.indisunique
    and i.indnkeyatts=2 and i.indnatts=2 and i.indkey[0]=3 and i.indkey[1]=2
    and i.indexprs is null and i.indpred is null
)";

pub(crate) const BILLING_KNOWLEDGE_SEARCH_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate billing-knowledge-search-schema",
        predecessor_sha384_hex:
            super::baseline_delta::BASELINE_REVISION_BILLING_DOCUMENT_READ_SHA384,
        target_sha384_hex: super::baseline_delta::BASELINE_REVISION_BILLING_KNOWLEDGE_SEARCH_SHA384,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_DOCUMENT_READ,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_BILLING_KNOWLEDGE_SEARCH,
        additive_sql: include_str!("../../../ops/apply-billing-knowledge-search-schema.sql"),
        precondition: None,
        delta_present: (
            "select to_regclass('public.billing_knowledge_search') is not null
                or to_regclass('public.idx_billing_knowledge_search_library_scope') is not null
                or exists (select 1 from pg_enum
                    where enumtypid=to_regtype('public.billing_owning_execution_kind')
                    and enumlabel='knowledge_search')",
            "knowledge-search billing DDL is already present under its predecessor checksum; refusing to stamp partial or pre-applied state",
        ),
        predecessor_error: "baseline checksum is not the exact knowledge-search billing predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BillingKnowledgeSearchSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_billing_knowledge_search_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<BillingKnowledgeSearchSchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &BILLING_KNOWLEDGE_SEARCH_DELTA, apply).await?;
    Ok(BillingKnowledgeSearchSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}
