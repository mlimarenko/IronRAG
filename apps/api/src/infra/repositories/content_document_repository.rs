use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct ContentDocumentRow {
    pub id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: String,
    pub document_state: String,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub deleted_at: Option<DateTime<Utc>>,
    pub parent_document_id: Option<Uuid>,
    pub parent_external_key: Option<String>,
    pub document_role: String,
}

macro_rules! content_document_projection {
    () => {
        "id,
            workspace_id,
            library_id,
            external_key,
            document_state::text as document_state,
            created_by_principal_id,
            created_at,
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role"
    };
}

#[derive(Debug, Clone)]
pub struct NewContentDocument<'a> {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub external_key: &'a str,
    pub document_state: &'a str,
    pub created_by_principal_id: Option<Uuid>,
    /// Declared structural parent key from the connector (the page a dependent
    /// rides with). Resolved to `parent_document_id` at admission when the
    /// parent already exists, otherwise left pending for the resolver.
    pub parent_external_key: Option<&'a str>,
    /// Resolved parent document id, when the parent was already
    /// admitted at creation time.
    pub parent_document_id: Option<Uuid>,
    /// Typed role decided at admission: `primary`, `attachment`, or
    /// `attached_context`.
    pub document_role: &'a str,
}
pub async fn list_documents_by_library(
    postgres: &PgPool,
    library_id: Uuid,
) -> Result<Vec<ContentDocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "select
            ",
        content_document_projection!(),
        "
         from content_document
         where library_id = $1
         order by created_at desc"
    ))
    .bind(library_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_document_by_id(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    get_document_by_id_with_executor(postgres, document_id).await
}

pub async fn get_document_by_id_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<Option<ContentDocumentRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "select
            ",
        content_document_projection!(),
        "
         from content_document
         where id = $1"
    ))
    .bind(document_id)
    .fetch_optional(executor)
    .await
}

/// Loads and locks one persisted document row for a lifecycle transition.
///
/// Concurrent delete requests serialize on this row so only the transaction
/// that observes the non-deleted state creates the durable outbox event.
pub async fn lock_document_by_id_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<Option<ContentDocumentRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "select
            ",
        content_document_projection!(),
        "
         from content_document
         where id = $1
         for update"
    ))
    .bind(document_id)
    .fetch_optional(executor)
    .await
}

pub async fn get_document_by_external_key(
    postgres: &PgPool,
    library_id: Uuid,
    external_key: &str,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "select
            ",
        content_document_projection!(),
        "
         from content_document
         where library_id = $1
           and external_key = $2
         order by created_at desc, id desc
         limit 1"
    ))
    .bind(library_id)
    .bind(external_key)
    .fetch_optional(postgres)
    .await
}

/// Lists the non-deleted child documents that resolve to `parent_document_id`
/// in `library_id`. Used by the delete-cascade lifecycle to act on a parent's
/// children. Multi-parent occurrence attribution is out of scope: a child has
/// exactly one resolved (first-resolved) `parent_document_id`.
pub async fn list_active_children_by_parent(
    postgres: &PgPool,
    library_id: Uuid,
    parent_document_id: Uuid,
) -> Result<Vec<ContentDocumentRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "select
            ",
        content_document_projection!(),
        "
         from content_document
         where library_id = $1
           and parent_document_id = $2
           and document_state <> 'deleted'
           and deleted_at is null
         order by created_at desc, id desc"
    ))
    .bind(library_id)
    .bind(parent_document_id)
    .fetch_all(postgres)
    .await
}

/// Detaches a child document from its resolved parent by clearing
/// `parent_document_id` while preserving the declared `parent_external_key`.
/// Used when a parent is deleted but the child (role `attachment`) stays alive
/// as a peer document; keeping the declared key lets the resolver re-attach if
/// the parent reappears.
pub async fn detach_document_parent(
    postgres: &PgPool,
    library_id: Uuid,
    document_id: Uuid,
) -> Result<(), sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let parent_locked = sqlx::query_scalar::<_, Uuid>(
        "select library.id
         from content_document as document
         join catalog_library as library on library.id = document.library_id
         where document.id = $1
           and library.id = $2
         for no key update of library",
    )
    .bind(document_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if parent_locked.is_none() {
        transaction.rollback().await?;
        return Ok(());
    }
    sqlx::query(
        "update content_document
         set parent_document_id = null
         where id = $1 and library_id = $2",
    )
    .bind(document_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    sqlx::query(
        "update knowledge_document
         set parent_document_id = null,
             updated_at = now()
         where document_id = $1 and library_id = $2",
    )
    .bind(document_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await?;
    transaction.commit().await?;
    Ok(())
}

pub async fn update_document_state(
    postgres: &PgPool,
    document_id: Uuid,
    document_state: &str,
    deleted_at: Option<DateTime<Utc>>,
) -> Result<Option<ContentDocumentRow>, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    // Read scope without a child lock, then acquire the library parent before
    // mutating the document. Catalog deletion uses the same parent-first order.
    let Some(document_scope) =
        get_document_by_id_with_executor(&mut *transaction, document_id).await?
    else {
        transaction.rollback().await?;
        return Ok(None);
    };
    let parent_locked = super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        document_scope.workspace_id,
        document_scope.library_id,
    )
    .await?;
    if !parent_locked {
        transaction.rollback().await?;
        return Ok(None);
    }
    let updated = update_document_state_with_executor(
        &mut *transaction,
        document_id,
        document_state,
        deleted_at,
    )
    .await?;
    if let Some(updated_document) = updated.as_ref() {
        // Keep the retrieval projection's visibility fence in the same
        // transaction. This path is used by unrecoverable-document cleanup,
        // where no later head-promotion step exists to repair a stale active
        // knowledge row.
        sqlx::query(
            "update knowledge_document
             set document_state = $2,
                 deleted_at = $3,
                 updated_at = now()
             where document_id = $1 and library_id = $4",
        )
        .bind(document_id)
        .bind(&updated_document.document_state)
        .bind(updated_document.deleted_at)
        .bind(updated_document.library_id)
        .execute(&mut *transaction)
        .await?;
        super::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            document_scope.library_id,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(updated)
}

pub async fn update_document_state_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
    document_state: &str,
    deleted_at: Option<DateTime<Utc>>,
) -> Result<Option<ContentDocumentRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "update content_document
         set document_state = $2::content_document_state,
             deleted_at = $3
         where id = $1
         returning
            ",
        content_document_projection!()
    ))
    .bind(document_id)
    .bind(document_state)
    .bind(deleted_at)
    .fetch_optional(executor)
    .await
}

/// Finds a live document whose latest source-bearing, mutation-owned revision
/// matches the checksum. Unadmitted historical sources cannot prove duplicates.
/// Canonical upload admission holds its library lock across this lookup and
/// document creation. Prefer readable candidates, then the earliest document.
pub async fn find_active_document_by_library_checksum<'e, E>(
    executor: E,
    library_id: Uuid,
    checksum: &str,
) -> Result<Option<Uuid>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    // Match against each document's LATEST revision only. Matching any
    // historical revision produced false positives: a document whose
    // older revision briefly equalled another body (e.g. a site's
    // login-required placeholder served transiently for many URLs)
    // would collide forever, even after its own content diverged. The
    // DISTINCT ON pins us to "is this the same body RIGHT NOW".
    sqlx::query_scalar::<_, Uuid>(
        "with latest_revision as (
             select distinct on (r.document_id)
                 r.document_id,
                 r.checksum, r.id, r.storage_key
             from content_revision r
             order by r.document_id, r.revision_number desc, r.created_at desc, r.id desc
         )
         select d.id
         from content_document d
         join latest_revision lr on lr.document_id = d.id
         left join content_document_head h on h.document_id = d.id
         where d.library_id = $1
           and lr.checksum = $2
           and lr.storage_key is not null
           and exists (select 1 from content_mutation_item i where i.document_id=d.id and i.result_revision_id=lr.id)
           and d.document_state <> 'deleted'
           and d.deleted_at is null
         order by (h.readable_revision_id is not null) desc,
                  d.created_at asc,
                  d.id asc
         limit 1",
    )
    .bind(library_id)
    .bind(checksum)
    .fetch_optional(executor)
    .await
}

pub async fn create_document(
    postgres: &PgPool,
    new_document: &NewContentDocument<'_>,
) -> Result<ContentDocumentRow, sqlx::Error> {
    create_document_with_executor(postgres, new_document).await
}

/// Creates the primary document, its empty head, and the query-facing
/// document projection in one transaction.
///
/// `knowledge_document.file_name` is derived only from primary metadata:
/// the readable/active revision storage key when one exists, otherwise the
/// document external key. A storage key generated by `ContentStorageService`
/// has a `<sha256>-<file-name>` basename; the digest prefix is stripped. This
/// makes rematerialization deterministic and prevents a stale projection from
/// becoming an accidental source of truth.
pub async fn create_document_with_executor<'e, E>(
    executor: E,
    new_document: &NewContentDocument<'_>,
) -> Result<ContentDocumentRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentDocumentRow>(concat!(
        "insert into content_document (
            id,
            workspace_id,
            library_id,
            external_key,
            document_state,
            created_by_principal_id,
            created_at,
            deleted_at,
            parent_document_id,
            parent_external_key,
            document_role
        )
        values ($1, $2, $3, $4, $5::content_document_state, $6, now(), null, $7, $8, $9)
        returning
            ",
        content_document_projection!()
    ))
    .bind(Uuid::now_v7())
    .bind(new_document.workspace_id)
    .bind(new_document.library_id)
    .bind(new_document.external_key)
    .bind(new_document.document_state)
    .bind(new_document.created_by_principal_id)
    .bind(new_document.parent_document_id)
    .bind(new_document.parent_external_key)
    .bind(new_document.document_role)
    .fetch_one(executor)
    .await
}
