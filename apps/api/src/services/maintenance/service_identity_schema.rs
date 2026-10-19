//! Additive live upgrade for the service-identity baseline change.

use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_IRG75, REQUIRED_BASELINE_OBJECT_GROUPS_PROVISIONING,
};
use crate::services::maintenance::baseline_delta::{
    AdditiveBaselineDeltaSpec, apply_additive_baseline_delta, bound_operator_lock_waits,
};

const ADDITIVE_SCHEMA_SQL: &str =
    include_str!("../../../ops/apply-integration-provisioning-schema.sql");
/// SHA-384 of the baseline revision that preceded the integration-provisioning
/// columns; a live database that has applied it records exactly this checksum.
const PREDECESSOR_BASELINE_SHA384: &str =
    crate::services::maintenance::baseline_delta::BASELINE_REVISION_IRG75_SHA384;
/// SHA-384 of the baseline revision this delta produces. It stamps this
/// revision and validates only the groups it declares.
const TARGET_BASELINE_SHA384: &str =
    crate::services::maintenance::baseline_delta::BASELINE_REVISION_PROVISIONING_SHA384;

/// The immutable integration-provisioning baseline delta: IRG-75 → the
/// provisioning revision.
pub(crate) const INTEGRATION_PROVISIONING_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate integration-provisioning-schema",
        predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
        target_sha384_hex: TARGET_BASELINE_SHA384,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_PROVISIONING,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_IRG75,
        additive_sql: ADDITIVE_SCHEMA_SQL,
        precondition: Some((
            "select to_regclass('public.iam_service_identity') is not null",
            "iam_service_identity is absent; run the prior service-identity migration before this upgrade",
        )),
        delta_present: (
            "select exists (
                 select 1 from information_schema.columns
                 where table_schema = 'public'
                   and (
                     (table_name = 'iam_service_identity' and column_name = 'provisioning_authority')
                     or (table_name = 'catalog_workspace' and column_name = 'provisioned_by_principal_id')
                     or (table_name = 'catalog_library' and column_name = 'provisioned_by_principal_id')
                   )
             )",
            "integration provisioning schema is partially or fully present with a stale baseline checksum; inspect and update it explicitly",
        ),
        predecessor_error: "baseline checksum is not the exact integration-provisioning predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationProvisioningSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_integration_provisioning_schema(
    postgres: &PgPool,
    apply: bool,
) -> anyhow::Result<IntegrationProvisioningSchemaMigrationReport> {
    let transition =
        apply_additive_baseline_delta(postgres, &INTEGRATION_PROVISIONING_DELTA, apply).await?;
    Ok(IntegrationProvisioningSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationProvisioningOwnershipReport {
    pub apply: bool,
    pub authority_principal_id: Uuid,
    pub workspace_id: Uuid,
    pub library_ids: Vec<Uuid>,
    pub assigned_workspace_count: u64,
    pub assigned_library_count: u64,
}

pub async fn assign_integration_provisioning_ownership(
    postgres: &PgPool,
    apply: bool,
    authority_principal_id: Uuid,
    workspace_id: Uuid,
    mut library_ids: Vec<Uuid>,
) -> anyhow::Result<IntegrationProvisioningOwnershipReport> {
    library_ids.sort_unstable();
    let supplied_count = library_ids.len();
    library_ids.dedup();
    anyhow::ensure!(library_ids.len() == supplied_count, "duplicate library ids are not allowed");

    let mut transaction = postgres.begin().await?;
    lock_and_validate_provisioning_authority(&mut transaction, authority_principal_id).await?;
    load_active_workspace(&mut transaction, workspace_id).await?;
    validate_workspace_library_set(&mut transaction, workspace_id, &library_ids).await?;

    if !apply {
        transaction.rollback().await?;
        return Ok(IntegrationProvisioningOwnershipReport {
            apply,
            authority_principal_id,
            workspace_id,
            library_ids,
            assigned_workspace_count: 0,
            assigned_library_count: 0,
        });
    }

    let assigned_library_count = grant_provisioning_ownership(
        &mut transaction,
        authority_principal_id,
        workspace_id,
        &library_ids,
    )
    .await?;
    transaction.commit().await?;

    Ok(IntegrationProvisioningOwnershipReport {
        apply,
        authority_principal_id,
        workspace_id,
        library_ids,
        assigned_workspace_count: 1,
        assigned_library_count,
    })
}

/// Take the operator locks and require `authority_principal_id` to be an active
/// root integration provisioner.
async fn lock_and_validate_provisioning_authority(
    conn: &mut PgConnection,
    authority_principal_id: Uuid,
) -> anyhow::Result<()> {
    bound_operator_lock_waits(conn).await?;
    sqlx::query("select pg_advisory_xact_lock(902_094)").execute(&mut *conn).await?;
    sqlx::query("lock table catalog_workspace, catalog_library in access exclusive mode")
        .execute(&mut *conn)
        .await?;

    let authority = sqlx::query_scalar::<_, Uuid>(
        "select identity.principal_id
           from iam_service_identity identity
           join iam_principal principal on principal.id = identity.principal_id
           join iam_api_token token on token.principal_id = identity.principal_id
           where identity.principal_id = $1
             and principal.principal_kind = 'worker'
             and principal.parent_principal_id is null
             and identity.provisioning_authority = 'integration'
             and identity.deleted_at is null
             and token.status = 'active'
             and (token.expires_at is null or token.expires_at > now())
         for update of identity, principal, token",
    )
    .bind(authority_principal_id)
    .fetch_optional(&mut *conn)
    .await?;
    anyhow::ensure!(authority.is_some(), "authority is not an active root integration provisioner");
    Ok(())
}

/// Lock the target workspace and require it to be active and unowned.
async fn load_active_workspace(conn: &mut PgConnection, workspace_id: Uuid) -> anyhow::Result<()> {
    let workspace: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "select id, provisioned_by_principal_id
         from catalog_workspace
         where id = $1 and lifecycle_state = 'active'
         for update",
    )
    .bind(workspace_id)
    .fetch_optional(&mut *conn)
    .await?;
    anyhow::ensure!(
        matches!(workspace, Some((_, None))),
        "workspace is absent, inactive, or already owned"
    );
    Ok(())
}

/// Lock the workspace's library set and require it to equal `library_ids`
/// exactly, with every library active and unowned.
async fn validate_workspace_library_set(
    conn: &mut PgConnection,
    workspace_id: Uuid,
    library_ids: &[Uuid],
) -> anyhow::Result<()> {
    let workspace_libraries: Vec<(Uuid, String, Option<Uuid>)> = sqlx::query_as(
        "select id, lifecycle_state::text, provisioned_by_principal_id
         from catalog_library
         where workspace_id = $1
         order by id
         for update",
    )
    .bind(workspace_id)
    .fetch_all(&mut *conn)
    .await?;
    anyhow::ensure!(
        workspace_libraries.iter().map(|(id, _, _)| *id).eq(library_ids.iter().copied()),
        "explicit library ids must equal the complete library set of the workspace"
    );
    anyhow::ensure!(
        workspace_libraries
            .iter()
            .all(|(_, lifecycle_state, owner)| lifecycle_state == "active" && owner.is_none()),
        "every workspace library must be active and have no provisioning owner"
    );
    Ok(())
}

/// Stamp the provisioning owner on the workspace and its libraries and grant the
/// authority `workspace_read`. Returns the number of libraries updated.
async fn grant_provisioning_ownership(
    conn: &mut PgConnection,
    authority_principal_id: Uuid,
    workspace_id: Uuid,
    library_ids: &[Uuid],
) -> anyhow::Result<u64> {
    sqlx::query("alter table catalog_workspace disable trigger trg_catalog_workspace_provisioning_owner_immutable")
        .execute(&mut *conn)
        .await?;
    sqlx::query("alter table catalog_library disable trigger trg_catalog_library_provisioning_owner_immutable")
        .execute(&mut *conn)
        .await?;
    let workspace_update = sqlx::query(
        "update catalog_workspace set provisioned_by_principal_id = $1
         where id = $2 and lifecycle_state = 'active' and provisioned_by_principal_id is null",
    )
    .bind(authority_principal_id)
    .bind(workspace_id)
    .execute(&mut *conn)
    .await?;
    let library_update = sqlx::query(
        "update catalog_library set provisioned_by_principal_id = $1
         where id = any($2) and workspace_id = $3 and lifecycle_state = 'active'
           and provisioned_by_principal_id is null",
    )
    .bind(authority_principal_id)
    .bind(library_ids)
    .bind(workspace_id)
    .execute(&mut *conn)
    .await?;
    anyhow::ensure!(
        workspace_update.rows_affected() == 1,
        "workspace ownership update count changed"
    );
    anyhow::ensure!(
        library_update.rows_affected() == library_ids.len() as u64,
        "library ownership update count changed"
    );
    sqlx::query("alter table catalog_library enable trigger trg_catalog_library_provisioning_owner_immutable")
        .execute(&mut *conn)
        .await?;
    sqlx::query("alter table catalog_workspace enable trigger trg_catalog_workspace_provisioning_owner_immutable")
        .execute(&mut *conn)
        .await?;

    let grant_count = sqlx::query(
        "insert into iam_grant (
           id, principal_id, resource_kind, resource_id, permission_kind,
           granted_by_principal_id, granted_at, expires_at
         ) values ($1, $2, 'workspace', $3, 'workspace_read', $2, now(), null)
         on conflict (principal_id, resource_kind, resource_id, permission_kind)
         do nothing",
    )
    .bind(Uuid::now_v7())
    .bind(authority_principal_id)
    .bind(workspace_id)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    anyhow::ensure!(grant_count <= 1, "workspace catalog grant count changed");
    Ok(library_update.rows_affected())
}
