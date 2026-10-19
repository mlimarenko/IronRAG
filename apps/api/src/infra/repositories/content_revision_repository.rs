use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres};
use uuid::Uuid;

use super::{
    content_document_repository::lock_document_by_id_with_executor,
    content_knowledge_projection_repository::{
        MaterializeKnowledgeDocumentOutcome,
        materialize_knowledge_document_from_current_head_with_transaction,
    },
};

#[derive(Debug, Clone, FromRow)]
pub struct ContentRevisionRow {
    pub id: Uuid,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_number: i32,
    pub parent_revision_id: Option<Uuid>,
    pub content_source_kind: String,
    pub checksum: String,
    pub mime_type: String,
    pub byte_size: i64,
    pub title: Option<String>,
    pub language_code: Option<String>,
    pub source_uri: Option<String>,
    pub document_hint: Option<String>,
    pub storage_key: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
}

macro_rules! content_revision_projection {
    () => {
        "id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind::text as content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            document_hint,
            storage_key,
            created_by_principal_id,
            created_at"
    };
}

#[derive(Debug, Clone)]
pub enum UpdateRevisionDocumentHintOutcome {
    Updated(Box<ContentRevisionRow>),
    RevisionNotFound,
    KnowledgeProjectionNotFound,
    VisibilityIntentRequired,
}

#[derive(Debug, Clone)]
pub enum UpdateRevisionStorageKeyOutcome {
    Updated(Box<ContentRevisionRow>),
    RevisionNotFound,
    KnowledgeProjectionNotFound,
}
#[derive(Debug, Clone)]
pub enum CreateContentRevisionOutcome {
    Created(Box<ContentRevisionRow>),
    DocumentNotFound,
    DocumentDeleted,
    ProjectionUnavailable,
}
#[derive(Debug, Clone)]
pub struct NewContentRevision<'a> {
    pub admission_identity: Option<&'a ironrag_contracts::documents::RevisionAdmissionIdentity>,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub revision_number: i32,
    pub parent_revision_id: Option<Uuid>,
    pub content_source_kind: &'a str,
    pub checksum: &'a str,
    pub mime_type: &'a str,
    pub byte_size: i64,
    pub title: Option<&'a str>,
    pub language_code: Option<&'a str>,
    pub source_uri: Option<&'a str>,
    pub document_hint: Option<&'a str>,
    pub storage_key: Option<&'a str>,
    pub created_by_principal_id: Option<Uuid>,
}

/// Production append input. Revision identity and ancestry are deliberately
/// absent: the repository derives them while holding the document lock.
#[derive(Debug, Clone)]
pub struct NewContentRevisionProjection<'a> {
    pub admission_identity: Option<&'a ironrag_contracts::documents::RevisionAdmissionIdentity>,
    pub document_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub content_source_kind: &'a str,
    pub checksum: &'a str,
    pub mime_type: &'a str,
    pub byte_size: i64,
    pub title: Option<&'a str>,
    pub language_code: Option<&'a str>,
    pub source_uri: Option<&'a str>,
    pub document_hint: Option<&'a str>,
    pub storage_key: Option<&'a str>,
    pub created_by_principal_id: Option<Uuid>,
}
pub async fn list_revisions_by_document(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Vec<ContentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "select
            ",
        content_revision_projection!(),
        "
         from content_revision
         where document_id = $1
         order by revision_number desc, created_at desc"
    ))
    .bind(document_id)
    .fetch_all(postgres)
    .await
}

pub async fn get_revision_by_id(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Option<ContentRevisionRow>, sqlx::Error> {
    get_revision_by_id_with_executor(postgres, revision_id).await
}

/// Reads the immutable replacement snapshot from the exact revision's owning
/// mutation item. Ambiguous or missing ownership never falls back to mutable
/// document metadata or a generated artifact path.
pub async fn get_revision_replacement_identity(
    postgres: &PgPool,
    revision_id: Uuid,
) -> Result<Option<ironrag_contracts::documents::ReplacementIdentity>, sqlx::Error> {
    let identity: Option<Option<serde_json::Value>> =
        sqlx::query_scalar("select admission_identity from content_revision where id = $1")
            .bind(revision_id)
            .fetch_optional(postgres)
            .await?;
    let Some(Some(identity)) = identity else {
        return Ok(None);
    };
    serde_json::from_value::<ironrag_contracts::documents::RevisionAdmissionIdentity>(identity)
        .map(|identity| Some(identity.replacement().clone()))
        .map_err(|error| sqlx::Error::Decode(Box::new(error)))
}

pub async fn get_revision_by_id_with_executor<'e, E>(
    executor: E,
    revision_id: Uuid,
) -> Result<Option<ContentRevisionRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "select
            ",
        content_revision_projection!(),
        "
         from content_revision
         where id = $1"
    ))
    .bind(revision_id)
    .fetch_optional(executor)
    .await
}

/// Reports whether any durable row still references `storage_key`.
///
/// The staged-blob reclaimer calls this before deleting a just-written blob,
/// while holding the per-library content storage advisory lock
/// (`content.library.storage:<library>`) that every admission also holds from
/// before its blob write until its commit. The lock, not merely a committed-row
/// scan, is what makes the answer safe: a concurrent admission of the identical
/// checksum-derived key cannot be between its write and its commit while the
/// lock is held here, so once this returns `false` no in-flight admission can
/// still commit a revision that shares the key.
///
/// The key space is content-addressed (`content/{ws}/{lib}/{digest}-{name}`) and
/// shared: a web snapshot written by the recursive crawl is recorded in
/// `content_web_discovered_page.snapshot_storage_key`, and the web-refetch path
/// stages the identical key through the same guard. A probe that looked only at
/// `content_revision` would delete a snapshot a discovered page still needs, so
/// every referencing table is scanned. The probe is bounded (`limit 1`). Errors
/// are not "unreferenced": the caller treats an error as "assume referenced" and
/// leaves the object.
pub async fn storage_key_is_referenced(
    postgres: &PgPool,
    storage_key: &str,
) -> Result<bool, sqlx::Error> {
    let referenced = sqlx::query_scalar::<_, i32>(
        "select 1 from content_revision where storage_key = $1 \
         union all \
         select 1 from content_web_discovered_page where snapshot_storage_key = $1 \
         limit 1",
    )
    .bind(storage_key)
    .fetch_optional(postgres)
    .await?;
    Ok(referenced.is_some())
}

pub async fn update_revision_storage_key(
    postgres: &PgPool,
    revision_id: Uuid,
    storage_key: Option<&str>,
) -> Result<UpdateRevisionStorageKeyOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((document_id, workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select document_id, workspace_id, library_id
         from content_revision
         where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::RevisionNotFound);
    };
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::RevisionNotFound);
    }
    let updated = sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "update content_revision
         set storage_key = $2
         where id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5
           and storage_key is distinct from $2
         returning
            ",
        content_revision_projection!()
    ))
    .bind(revision_id)
    .bind(storage_key)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let changed = updated.is_some();
    let revision = if let Some(updated) = updated {
        updated
    } else {
        get_revision_by_id_with_executor(&mut *transaction, revision_id)
            .await?
            .ok_or(sqlx::Error::RowNotFound)?
    };
    let projection_updated = sqlx::query(
        "update knowledge_revision
         set storage_ref = $2
         where revision_id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5",
    )
    .bind(revision_id)
    .bind(storage_key)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    if projection_updated.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::KnowledgeProjectionNotFound);
    }

    let materialized = materialize_knowledge_document_from_current_head_with_transaction(
        &mut transaction,
        document_id,
    )
    .await?;
    if materialized.outcome != MaterializeKnowledgeDocumentOutcome::Materialized {
        transaction.rollback().await?;
        return Ok(UpdateRevisionStorageKeyOutcome::KnowledgeProjectionNotFound);
    }
    if changed || materialized.changed {
        super::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            library_id,
        )
        .await?;
    }
    transaction.commit().await?;
    Ok(UpdateRevisionStorageKeyOutcome::Updated(Box::new(revision)))
}

pub async fn update_revision_document_hint(
    postgres: &PgPool,
    revision_id: Uuid,
    document_hint: Option<&str>,
) -> Result<UpdateRevisionDocumentHintOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some((document_id, workspace_id, library_id)) = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        "select document_id, workspace_id, library_id
             from content_revision
             where id = $1",
    )
    .bind(revision_id)
    .fetch_optional(&mut *transaction)
    .await?
    else {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::RevisionNotFound);
    };

    // Parent-first locking matches every other answer-visible content
    // transition. Persisted metadata, the query projection, and the cache
    // generation either commit together or remain entirely unchanged.
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut *transaction,
        workspace_id,
        library_id,
    )
    .await?
    {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::RevisionNotFound);
    }

    let protected: bool = sqlx::query_scalar("select exists (select 1 from content_document_head where document_id=$1 and visibility_intent_id is not null)")
        .bind(document_id).fetch_one(&mut *transaction).await?;
    if protected {
        return Ok(UpdateRevisionDocumentHintOutcome::VisibilityIntentRequired);
    }
    let current: bool = sqlx::query_scalar("select exists (select 1 from content_current_revision where document_id=$1 and revision_id=$2)")
        .bind(document_id).bind(revision_id).fetch_one(&mut *transaction).await?;
    if !current {
        let projection_exists: bool = sqlx::query_scalar("select exists (select 1 from knowledge_revision where revision_id=$1 and document_id=$2 and workspace_id=$3 and library_id=$4)")
            .bind(revision_id).bind(document_id).bind(workspace_id).bind(library_id)
            .fetch_one(&mut *transaction).await?;
        return Ok(if projection_exists {
            UpdateRevisionDocumentHintOutcome::RevisionNotFound
        } else {
            UpdateRevisionDocumentHintOutcome::KnowledgeProjectionNotFound
        });
    }

    let updated_revision = sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "update content_revision
         set document_hint = $2
         where id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5
         returning
            ",
        content_revision_projection!()
    ))
    .bind(revision_id)
    .bind(document_hint)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some(updated_revision) = updated_revision else {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::RevisionNotFound);
    };

    let knowledge_updated = sqlx::query(
        "update knowledge_revision
         set document_hint = $2
         where revision_id = $1
           and document_id = $3
           and workspace_id = $4
           and library_id = $5",
    )
    .bind(revision_id)
    .bind(document_hint)
    .bind(document_id)
    .bind(workspace_id)
    .bind(library_id)
    .execute(&mut *transaction)
    .await?;
    if knowledge_updated.rows_affected() != 1 {
        transaction.rollback().await?;
        return Ok(UpdateRevisionDocumentHintOutcome::KnowledgeProjectionNotFound);
    }

    super::catalog_repository::touch_library_source_truth_version_with_executor(
        &mut *transaction,
        library_id,
    )
    .await?;
    transaction.commit().await?;
    Ok(UpdateRevisionDocumentHintOutcome::Updated(Box::new(updated_revision)))
}

pub async fn list_revisions_by_ids(
    postgres: &PgPool,
    revision_ids: &[Uuid],
) -> Result<Vec<ContentRevisionRow>, sqlx::Error> {
    if revision_ids.is_empty() {
        return Ok(Vec::new());
    }

    sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "select
            ",
        content_revision_projection!(),
        "
         from content_revision
         where id = any($1)"
    ))
    .bind(revision_ids)
    .fetch_all(postgres)
    .await
}

pub async fn get_latest_revision_for_document(
    postgres: &PgPool,
    document_id: Uuid,
) -> Result<Option<ContentRevisionRow>, sqlx::Error> {
    sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "select
            ",
        content_revision_projection!(),
        "
         from content_revision
         where document_id = $1
         order by revision_number desc, created_at desc
         limit 1"
    ))
    .bind(document_id)
    .fetch_optional(postgres)
    .await
}

pub async fn create_revision(
    postgres: &PgPool,
    new_revision: &NewContentRevision<'_>,
) -> Result<ContentRevisionRow, sqlx::Error> {
    create_revision_with_executor(postgres, new_revision).await
}

/// Appends the next current revision and its query projection atomically.
///
/// The document parent lock serializes revision numbering, so two concurrent
/// appenders cannot derive the same revision number from a stale pre-read.
pub async fn create_revision_with_projection(
    postgres: &PgPool,
    new_revision: &NewContentRevisionProjection<'_>,
) -> Result<CreateContentRevisionOutcome, sqlx::Error> {
    let mut transaction = postgres.begin().await?;
    let Some(document) = lock_revision_document(&mut transaction, new_revision).await? else {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::DocumentNotFound);
    };
    if document.document_state == "deleted" || document.deleted_at.is_some() {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::DocumentDeleted);
    }
    let materialized = materialize_knowledge_document_from_current_head_with_transaction(
        &mut transaction,
        document.id,
    )
    .await?;
    if materialized.outcome != MaterializeKnowledgeDocumentOutcome::Materialized {
        transaction.rollback().await?;
        return Ok(CreateContentRevisionOutcome::ProjectionUnavailable);
    }

    let latest = sqlx::query_as::<_, (Uuid, i32)>(
        "select id, revision_number
         from content_revision
         where document_id = $1
           and workspace_id = $2
           and library_id = $3
         order by revision_number desc, created_at desc
         limit 1",
    )
    .bind(document.id)
    .bind(document.workspace_id)
    .bind(document.library_id)
    .fetch_optional(&mut *transaction)
    .await?;
    let revision_number =
        allocate_revision_number_with_executor(&mut *transaction, document.id).await?;
    let derived_revision = NewContentRevision {
        admission_identity: new_revision.admission_identity,
        document_id: document.id,
        workspace_id: document.workspace_id,
        library_id: document.library_id,
        revision_number,
        parent_revision_id: latest.map(|(id, _)| id),
        content_source_kind: new_revision.content_source_kind,
        checksum: new_revision.checksum,
        mime_type: new_revision.mime_type,
        byte_size: new_revision.byte_size,
        title: new_revision.title,
        language_code: new_revision.language_code,
        source_uri: new_revision.source_uri,
        document_hint: new_revision.document_hint,
        storage_key: new_revision.storage_key,
        created_by_principal_id: new_revision.created_by_principal_id,
    };
    let revision = create_revision_with_executor(&mut *transaction, &derived_revision).await?;
    sqlx::query(
        "insert into knowledge_revision (
            revision_id, workspace_id, library_id, document_id, revision_number,
            revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type,
            checksum, title, byte_size, normalized_text, text_checksum, image_checksum,
            text_state, vector_state, graph_state, text_readable_at, vector_ready_at,
            graph_ready_at, superseded_by_revision_id, created_at
         ) values (
            $1, $2, $3, $4, $5,
            'accepted', $6, $7, $8, $9, $10,
            $11, $12, $13, null, null, null,
            'accepted', 'accepted', 'accepted', null, null,
            null, null, $14
         )",
    )
    .bind(revision.id)
    .bind(revision.workspace_id)
    .bind(revision.library_id)
    .bind(revision.document_id)
    .bind(i64::from(revision.revision_number))
    .bind(&revision.content_source_kind)
    .bind(&revision.storage_key)
    .bind(&revision.source_uri)
    .bind(&revision.document_hint)
    .bind(&revision.mime_type)
    .bind(&revision.checksum)
    .bind(&revision.title)
    .bind(revision.byte_size)
    .bind(revision.created_at)
    .execute(&mut *transaction)
    .await?;

    if materialized.changed {
        super::catalog_repository::touch_library_source_truth_version_with_executor(
            &mut *transaction,
            document.library_id,
        )
        .await?;
    }

    transaction.commit().await?;
    Ok(CreateContentRevisionOutcome::Created(Box::new(revision)))
}

/// Resolves and locks a revision's complete owner scope before any allocation.
async fn lock_revision_document(
    transaction: &mut sqlx::Transaction<'_, Postgres>,
    input: &NewContentRevisionProjection<'_>,
) -> Result<Option<super::content_document_repository::ContentDocumentRow>, sqlx::Error> {
    if !super::catalog_repository::lock_library_for_lifecycle_event_with_executor(
        &mut **transaction,
        input.workspace_id,
        input.library_id,
    )
    .await?
    {
        return Ok(None);
    }
    let document = lock_document_by_id_with_executor(&mut **transaction, input.document_id).await?;
    Ok(document.filter(|document| {
        (document.workspace_id, document.library_id) == (input.workspace_id, input.library_id)
    }))
}

pub async fn create_revision_with_executor<'e, E>(
    executor: E,
    new_revision: &NewContentRevision<'_>,
) -> Result<ContentRevisionRow, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    if let Some(identity) = new_revision.admission_identity {
        let normalized = identity
            .clone()
            .normalize()
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        let replacement = identity.replacement();
        if normalized != *identity
            || replacement.checksum != new_revision.checksum
            || replacement.mime_type != new_revision.mime_type
            || replacement.byte_size != new_revision.byte_size
            || replacement.title.as_deref() != new_revision.title
            || replacement.language_code.as_deref() != new_revision.language_code
            || replacement.source_uri.as_deref() != new_revision.source_uri
            || replacement.document_hint.as_deref() != new_revision.document_hint
        {
            return Err(sqlx::Error::Protocol(
                "revision admission must match normalized source metadata".to_owned(),
            ));
        }
    }
    sqlx::query_as::<_, ContentRevisionRow>(concat!(
        "with retained_counter as (
            update content_document set revision_sequence = greatest(revision_sequence, $5)
            where id = $2 returning id
         ) insert into content_revision (
            id,
            document_id,
            workspace_id,
            library_id,
            revision_number,
            parent_revision_id,
            content_source_kind,
            checksum,
            mime_type,
            byte_size,
            title,
            language_code,
            source_uri,
            document_hint,
            storage_key,
            created_by_principal_id,
            admission_identity,
            created_at
        )
        values (
            $1,
            $2,
            $3,
            $4,
            $5,
            $6,
            $7::content_source_kind,
            $8,
            $9,
            $10,
            $11,
            $12,
            $13,
            $14,
            $15,
            $16,
            $17,
            now()
        )
        returning
            ",
        content_revision_projection!()
    ))
    .bind(Uuid::now_v7())
    .bind(new_revision.document_id)
    .bind(new_revision.workspace_id)
    .bind(new_revision.library_id)
    .bind(new_revision.revision_number)
    .bind(new_revision.parent_revision_id)
    .bind(new_revision.content_source_kind)
    .bind(new_revision.checksum)
    .bind(new_revision.mime_type)
    .bind(new_revision.byte_size)
    .bind(new_revision.title)
    .bind(new_revision.language_code)
    .bind(new_revision.source_uri)
    .bind(new_revision.document_hint)
    .bind(new_revision.storage_key)
    .bind(new_revision.created_by_principal_id)
    .bind(new_revision.admission_identity.map(sqlx::types::Json))
    .fetch_one(executor)
    .await
}

/// Allocates from retained document state, never from prunable revision history.
/// The caller holds the owning library/document lifecycle locks and includes
/// this update in the same transaction as the revision insertion.
///
/// # Errors
/// Missing documents and integer exhaustion fail closed without saturating or
/// reusing a previously allocated revision number.
pub async fn allocate_revision_number_with_executor<'e, E>(
    executor: E,
    document_id: Uuid,
) -> Result<i32, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar(
        "update content_document set revision_sequence = revision_sequence + 1
         where id = $1 and revision_sequence < 2147483647
         returning revision_sequence",
    )
    .bind(document_id)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| {
        sqlx::Error::Protocol("document revision sequence is absent or exhausted".to_owned())
    })
}
