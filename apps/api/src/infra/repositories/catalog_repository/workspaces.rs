use super::deletion::{
    LockedCatalogDeleteTarget, delete_locked_catalog_target, lock_catalog_delete_blockers,
};
use super::*;

pub async fn list_workspaces(postgres: &PgPool) -> Result<Vec<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         order by created_at desc",
    )
    .fetch_all(postgres)
    .await
}

pub async fn list_workspace_ids(postgres: &PgPool) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar::<_, Uuid>("select id from catalog_workspace").fetch_all(postgres).await
}

pub async fn get_workspace_by_id(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         where id = $1",
    )
    .bind(workspace_id)
    .fetch_optional(postgres)
    .await
}

pub async fn get_workspace_by_slug(
    postgres: &PgPool,
    slug: &str,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         where slug = $1",
    )
    .bind(slug)
    .fetch_optional(postgres)
    .await
}

pub async fn get_workspace_by_slug_with_transaction(
    transaction: &mut PgConnection,
    slug: &str,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at
         from catalog_workspace
         where slug = $1
         for update",
    )
    .bind(slug)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn create_workspace(
    postgres: &PgPool,
    slug: &str,
    display_name: &str,
    created_by_principal_id: Option<Uuid>,
) -> Result<CatalogWorkspaceRow, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "insert into catalog_workspace (
            id,
            slug,
            display_name,
            lifecycle_state,
            created_by_principal_id,
            created_at,
            updated_at
        )
        values ($1, $2, $3, 'active', $4, now(), now())
        returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(slug)
    .bind(display_name)
    .bind(created_by_principal_id)
    .fetch_one(postgres)
    .await
}

pub async fn create_workspace_with_transaction(
    transaction: &mut PgConnection,
    slug: &str,
    display_name: &str,
    created_by_principal_id: Option<Uuid>,
    provisioned_by_principal_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "insert into catalog_workspace (
            id, slug, display_name, lifecycle_state, created_by_principal_id,
            provisioned_by_principal_id, created_at, updated_at
         ) values ($1, $2, $3, 'active', $4, $5, now(), now())
         on conflict (slug) do nothing
         returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(Uuid::now_v7())
    .bind(slug)
    .bind(display_name)
    .bind(created_by_principal_id)
    .bind(provisioned_by_principal_id)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn lock_provisioned_workspace(
    transaction: &mut PgConnection,
    workspace_id: Uuid,
    provisioned_by_principal_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state,
            created_at, updated_at
         from catalog_workspace
         where id = $1
           and provisioned_by_principal_id = $2
           and lifecycle_state = 'active'
           and not exists (
             select 1
             from ops_async_operation
             where subject_kind = 'catalog_workspace'
               and subject_id = catalog_workspace.id
               and operation_kind = 'delete_workspace'
               and status in ('accepted', 'processing')
           )
         for update",
    )
    .bind(workspace_id)
    .bind(provisioned_by_principal_id)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn count_provisioned_workspaces_with_transaction(
    transaction: &mut PgConnection,
    provisioned_by_principal_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "select count(*)
         from catalog_workspace
         where provisioned_by_principal_id = $1",
    )
    .bind(provisioned_by_principal_id)
    .fetch_one(&mut *transaction)
    .await
}

pub async fn lock_provisioned_workspace_for_deletion(
    transaction: &mut PgConnection,
    workspace_id: Uuid,
    provisioned_by_principal_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "select id, slug, display_name, lifecycle_state::text as lifecycle_state,
            created_at, updated_at
         from catalog_workspace
         where id = $1
           and provisioned_by_principal_id = $2
           and lifecycle_state = 'active'
         for update",
    )
    .bind(workspace_id)
    .bind(provisioned_by_principal_id)
    .fetch_optional(&mut *transaction)
    .await
}

pub async fn lock_workspace_libraries_owned_by_provisioner(
    transaction: &mut PgConnection,
    workspace_id: Uuid,
    provisioned_by_principal_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let owners = sqlx::query_scalar::<_, Option<Uuid>>(
        "select provisioned_by_principal_id
         from catalog_library
         where workspace_id = $1
         order by id
         for update",
    )
    .bind(workspace_id)
    .fetch_all(&mut *transaction)
    .await?;
    Ok(owners.into_iter().all(|owner| owner == Some(provisioned_by_principal_id)))
}

pub async fn update_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
    slug: &str,
    display_name: &str,
    lifecycle_state: &str,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "update catalog_workspace
         set slug = $2,
             display_name = $3,
             lifecycle_state = $4::catalog_workspace_lifecycle_state,
             updated_at = now()
         where id = $1
         returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(workspace_id)
    .bind(slug)
    .bind(display_name)
    .bind(lifecycle_state)
    .fetch_optional(postgres)
    .await
}

pub async fn archive_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<Option<CatalogWorkspaceRow>, sqlx::Error> {
    sqlx::query_as::<_, CatalogWorkspaceRow>(
        "update catalog_workspace
         set lifecycle_state = 'archived',
             updated_at = now()
         where id = $1
         returning id, slug, display_name, lifecycle_state::text as lifecycle_state, created_at, updated_at",
    )
    .bind(workspace_id)
    .fetch_optional(postgres)
    .await
}

pub async fn delete_workspace(
    postgres: &PgPool,
    workspace_id: Uuid,
) -> Result<CatalogWorkspaceDeleteOutcome, sqlx::Error> {
    repair_vector_shard_cascades_before_catalog_delete(postgres).await?;
    let mut transaction = postgres.begin().await?;
    // AI configuration statements take this transaction lock in a BEFORE
    // STATEMENT trigger, before locking binding/account/model rows. Take it
    // before the catalog parent so delete and generation invalidation share
    // one global lock order instead of parent -> child / child -> parent.
    sqlx::query(
        "select pg_advisory_xact_lock(
            hashtextextended('ironrag:ai-config-generation', 0)
         )",
    )
    .execute(&mut *transaction)
    .await?;
    let locked_workspace_id = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_workspace
         where id = $1
         for update",
    )
    .bind(workspace_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if locked_workspace_id.is_none() {
        transaction.rollback().await?;
        return Ok(CatalogWorkspaceDeleteOutcome::NotFound);
    }

    // A workspace delete cascades through every library. Lock each library so
    // a lifecycle event or delivery job cannot acquire its FK key-share lock
    // after the guard has inspected that library.
    let locked_library_ids = sqlx::query_scalar::<_, Uuid>(
        "select id
         from catalog_library
         where workspace_id = $1
         order by id
         for update",
    )
    .bind(workspace_id)
    .fetch_all(&mut *transaction)
    .await?;
    let blockers = lock_catalog_delete_blockers(&mut transaction, &locked_library_ids).await?;
    if !blockers.is_empty() {
        transaction.rollback().await?;
        return Ok(CatalogWorkspaceDeleteOutcome::Blocked(blockers));
    }

    delete_locked_catalog_target(
        &mut transaction,
        &locked_library_ids,
        LockedCatalogDeleteTarget::Workspace(workspace_id),
    )
    .await?;
    transaction.commit().await?;
    Ok(CatalogWorkspaceDeleteOutcome::Deleted)
}
