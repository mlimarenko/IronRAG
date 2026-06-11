//! Idempotent, per-library recompute of structural document parentage and
//! roles over existing content rows.
//!
//! Connectors that predate the parentage contract never declared a
//! `parent_external_key`, so their attachment documents have a null
//! `parent_document_id` and the default `primary` role. This pass recovers the
//! relationship structurally and finalizes the typed role, with no re-ingest:
//!
//! 1. **Declared-key re-resolution.** Rows whose `parent_external_key` is set
//!    but `parent_document_id` is still null are re-resolved against the
//!    current document set (covers out-of-order connector sync / partial
//!    re-crawl / import remap).
//! 2. **Structural recovery.** Rows with no declared key but an attachment-style
//!    structural source (`/download/attachments/<page-id>/...` in the external
//!    key, document hint, or source uri) are matched to the same-library
//!    document that exposes that page id as its own structural identity.
//! 3. **Role finalization.** Every child's role is set from the canonical
//!    media-class classifier via
//!    [`crate::domains::content::derive_document_role`]: a raster-image child
//!    becomes `attached_context`; any other child stays an `attachment` peer.
//!
//! The pass is idempotent: a clean re-run resolves and updates nothing. The
//! hard gate is `resolvable_but_null_image_children == 0` — every raster-image
//! attachment whose parent exists in the library must end up attached. Image
//! children whose parent page is genuinely absent (e.g. a partial crawl) are
//! reported separately as orphans and are not part of the gate, because there
//! is no parent to attach.

use std::collections::HashMap;

use anyhow::Context;
use uuid::Uuid;

use crate::domains::content::{
    attachment_parent_page_id, derive_document_role, revision_is_raster_image,
    structural_source_numeric_ids,
};

/// Outcome of one document-parentage backfill pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentParentBackfillReport {
    /// Documents examined as potential children (have an attachment-style
    /// structural source or a declared parent key).
    pub scanned: usize,
    /// Children that ended this pass with a resolved `parent_document_id`.
    pub resolved: usize,
    /// Children whose typed role was written (this pass changed the role).
    pub role_set: usize,
    /// Raster-image children that are structurally attachments whose parent
    /// page genuinely does not exist in the library (true orphans). Not part of
    /// the hard gate — there is no parent to attach.
    pub orphan_image_children: usize,
    /// HARD GATE. Raster-image attachment children whose parent exists in the
    /// library yet ended the pass with a null `parent_document_id`. Must be 0.
    pub unresolved_image_children: usize,
}

/// One child-candidate document with the structural signals the resolver needs.
#[derive(Debug, sqlx::FromRow)]
struct ChildCandidateRow {
    id: Uuid,
    external_key: String,
    parent_external_key: Option<String>,
    document_role: String,
    /// Declared MIME of the readable/active revision, for media-class derivation.
    mime_type: Option<String>,
    /// Structural source carrying the `/download/attachments/<id>/` shape, if any.
    document_hint: Option<String>,
    source_uri: Option<String>,
}

/// One non-attachment document and the structural-source blob the resolver
/// scans for page-identity ids.
#[derive(Debug, sqlx::FromRow)]
struct ParentCandidateRow {
    id: Uuid,
    external_key: String,
    document_hint: Option<String>,
    source_uri: Option<String>,
}

/// Runs the document-parentage backfill for one library. Idempotent.
///
/// # Errors
/// Returns an error if any underlying query fails.
pub async fn backfill_document_parents(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<DocumentParentBackfillReport> {
    let parent_index = build_parent_page_index(pool, library_id).await?;
    let external_key_index = build_external_key_index(pool, library_id).await?;
    let children = load_child_candidates(pool, library_id).await?;

    let mut report = DocumentParentBackfillReport::default();
    for child in children {
        report.scanned += 1;

        // 1) Declared key re-resolution; 2) structural recovery fallback.
        let (resolved_parent_id, resolved_parent_key) =
            resolve_child_parent(&child, &parent_index, &external_key_index);

        let has_parent = resolved_parent_id.is_some();
        let is_raster_image =
            revision_is_raster_image(Some(child.external_key.as_str()), child.mime_type.as_deref());
        let target_role = derive_document_role(has_parent, is_raster_image).to_string();

        // Gate accounting: only raster-image children that are structurally
        // attachments count toward the gate / orphan split.
        let structural_page_id = child_attachment_page_id(&child);
        if is_raster_image && structural_page_id.is_some() {
            if has_parent {
                // attached this pass or already attached — fine
            } else if parent_page_exists(&structural_page_id, &parent_index) {
                report.unresolved_image_children += 1;
            } else {
                report.orphan_image_children += 1;
            }
        }

        let role_changed = target_role != child.document_role;
        // Apply unconditionally: both writes are idempotent (same values on a
        // no-op re-run), and the update mirrors onto `knowledge_document`, which
        // can be stale even when `content_document` is already correct (e.g. a
        // re-run after a prior content-plane-only backfill). Gating on
        // content-plane change alone would leave the knowledge-plane mirror —
        // the row retrieval actually reads — unsynced.
        update_child_parentage(
            pool,
            child.id,
            resolved_parent_id,
            resolved_parent_key.as_deref(),
            &target_role,
        )
        .await?;
        if has_parent {
            report.resolved += 1;
        }
        if role_changed {
            report.role_set += 1;
        }
    }
    Ok(report)
}

/// `page-id -> parent_document_id` over non-attachment documents, deterministic
/// (lowest document id wins a collision; any parent suffices per the canonical
/// first-resolved rule).
async fn build_parent_page_index(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<HashMap<String, Uuid>> {
    let rows = load_parent_candidates(pool, library_id).await?;
    let mut index: HashMap<String, Uuid> = HashMap::new();
    for row in rows {
        for value in
            structural_source_values(&row.external_key, &row.document_hint, &row.source_uri)
        {
            for page_id in structural_source_numeric_ids(value) {
                index
                    .entry(page_id)
                    .and_modify(|existing| {
                        if row.id < *existing {
                            *existing = row.id;
                        }
                    })
                    .or_insert(row.id);
            }
        }
    }
    Ok(index)
}

/// `external_key -> document_id` over all non-deleted documents, for declared
/// `parent_external_key` re-resolution.
async fn build_external_key_index(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<HashMap<String, Uuid>> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        "select id, external_key
         from content_document
         where library_id = $1 and document_state <> 'deleted'",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
    .context("failed to load external-key index for document-parentage backfill")?;
    let mut index: HashMap<String, Uuid> = HashMap::new();
    for (id, external_key) in rows {
        index
            .entry(external_key)
            .and_modify(|existing| {
                if id < *existing {
                    *existing = id;
                }
            })
            .or_insert(id);
    }
    Ok(index)
}

async fn load_child_candidates(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<Vec<ChildCandidateRow>> {
    sqlx::query_as::<_, ChildCandidateRow>(
        "select
            d.id,
            d.external_key,
            d.parent_external_key,
            d.document_role,
            r.mime_type,
            r.document_hint,
            r.source_uri
         from content_document d
         left join content_document_head h on h.document_id = d.id
         left join content_revision r on r.id = coalesce(h.readable_revision_id, h.active_revision_id)
         where d.library_id = $1
           and d.document_state <> 'deleted'
           and (
                d.parent_external_key is not null
                or d.external_key like '%/download/attachments/%'
                or r.document_hint like '%/download/attachments/%'
                or r.source_uri like '%/download/attachments/%'
           )",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
    .context("failed to load child candidates for document-parentage backfill")
}

async fn load_parent_candidates(
    pool: &sqlx::PgPool,
    library_id: Uuid,
) -> anyhow::Result<Vec<ParentCandidateRow>> {
    sqlx::query_as::<_, ParentCandidateRow>(
        "select
            d.id,
            d.external_key,
            r.document_hint,
            r.source_uri
         from content_document d
         left join content_document_head h on h.document_id = d.id
         left join content_revision r on r.id = coalesce(h.readable_revision_id, h.active_revision_id)
         where d.library_id = $1
           and d.document_state <> 'deleted'
           and (r.document_hint is null or r.document_hint not like '%/download/attachments/%')
           and d.external_key not like '%/download/attachments/%'",
    )
    .bind(library_id)
    .fetch_all(pool)
    .await
    .context("failed to load parent candidates for document-parentage backfill")
}

async fn update_child_parentage(
    pool: &sqlx::PgPool,
    document_id: Uuid,
    parent_document_id: Option<Uuid>,
    parent_external_key: Option<&str>,
    document_role: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "update content_document
         set parent_document_id = $2,
             parent_external_key = $3,
             document_role = $4
         where id = $1",
    )
    .bind(document_id)
    .bind(parent_document_id)
    .bind(parent_external_key)
    .bind(document_role)
    .execute(pool)
    .await
    .context("failed to update document parentage during backfill")?;
    // Mirror the parentage onto the knowledge-plane row that retrieval reads
    // (`knowledge_document`). Without this the demotion signal never reaches the
    // query path: the document index is hydrated from `knowledge_document`, so a
    // content-plane-only update would leave every document `primary` at query
    // time. No-op when the document has not been promoted yet (the promote path
    // stamps the role from the same canonical derivation).
    sqlx::query(
        "update knowledge_document
         set parent_document_id = $2,
             document_role = $3
         where document_id = $1",
    )
    .bind(document_id)
    .bind(parent_document_id)
    .bind(document_role)
    .execute(pool)
    .await
    .context("failed to mirror document parentage onto knowledge_document during backfill")?;
    Ok(())
}

/// Resolves a child's parent id + declared key from (1) its declared
/// `parent_external_key`, then (2) the structural attachment page id. Returns
/// the resolved `(parent_document_id, parent_external_key)`, either of which can
/// be `None` when nothing resolves.
fn resolve_child_parent(
    child: &ChildCandidateRow,
    parent_index: &HashMap<String, Uuid>,
    external_key_index: &HashMap<String, Uuid>,
) -> (Option<Uuid>, Option<String>) {
    // 1) Declared key takes precedence: re-resolve it against current docs.
    if let Some(key) =
        child.parent_external_key.as_deref().map(str::trim).filter(|value| !value.is_empty())
    {
        let resolved = external_key_index.get(key).copied();
        return (resolved, Some(key.to_string()));
    }

    // 2) Structural recovery from the attachment page id.
    if let Some(page_id) = child_attachment_page_id(child) {
        if let Some(parent_id) = parent_index.get(&page_id).copied() {
            return (Some(parent_id), None);
        }
    }
    (None, None)
}

/// The attachment page id encoded in any of the child's structural sources, via
/// the canonical [`attachment_parent_page_id`] parser.
fn child_attachment_page_id(child: &ChildCandidateRow) -> Option<String> {
    structural_source_values(&child.external_key, &child.document_hint, &child.source_uri)
        .into_iter()
        .find_map(attachment_parent_page_id)
}

/// `true` when the child's structural page id resolves to an existing parent —
/// i.e. the child is resolvable, distinguishing a resolver miss (gate) from a
/// true orphan.
fn parent_page_exists(page_id: &Option<String>, parent_index: &HashMap<String, Uuid>) -> bool {
    page_id.as_deref().is_some_and(|id| parent_index.contains_key(id))
}

/// Collects the non-empty structural source values of a document in a fixed
/// order (external key, document hint, source uri).
fn structural_source_values<'a>(
    external_key: &'a str,
    document_hint: &'a Option<String>,
    source_uri: &'a Option<String>,
) -> Vec<&'a str> {
    let mut values = vec![external_key];
    if let Some(hint) = document_hint.as_deref() {
        values.push(hint);
    }
    if let Some(uri) = source_uri.as_deref() {
        values.push(uri);
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    fn child(
        external_key: &str,
        parent_external_key: Option<&str>,
        document_role: &str,
        mime_type: Option<&str>,
        document_hint: Option<&str>,
    ) -> ChildCandidateRow {
        ChildCandidateRow {
            id: Uuid::now_v7(),
            external_key: external_key.to_string(),
            parent_external_key: parent_external_key.map(ToString::to_string),
            document_role: document_role.to_string(),
            mime_type: mime_type.map(ToString::to_string),
            document_hint: document_hint.map(ToString::to_string),
            source_uri: None,
        }
    }

    #[test]
    fn declared_key_resolves_when_parent_exists() {
        let parent_id = Uuid::now_v7();
        let external_key_index = HashMap::from([("parent-key".to_string(), parent_id)]);
        let parent_index = HashMap::new();
        let row = child("child-key", Some("parent-key"), "primary", Some("image/png"), None);

        let (resolved_id, resolved_key) =
            resolve_child_parent(&row, &parent_index, &external_key_index);
        assert_eq!(resolved_id, Some(parent_id));
        assert_eq!(resolved_key.as_deref(), Some("parent-key"));
    }

    #[test]
    fn declared_key_stays_pending_when_parent_absent() {
        let external_key_index = HashMap::new();
        let parent_index = HashMap::new();
        let row = child("child-key", Some("parent-key"), "primary", Some("image/png"), None);

        let (resolved_id, resolved_key) =
            resolve_child_parent(&row, &parent_index, &external_key_index);
        // Parent not present yet: id stays null but the declared key is kept so
        // a later pass can re-resolve it.
        assert_eq!(resolved_id, None);
        assert_eq!(resolved_key.as_deref(), Some("parent-key"));
    }

    #[test]
    fn structural_recovery_matches_parent_page_id() {
        let parent_id = Uuid::now_v7();
        let parent_index = HashMap::from([("4242".to_string(), parent_id)]);
        let external_key_index = HashMap::new();
        let row = child(
            "opaque-child-key",
            None,
            "primary",
            Some("image/png"),
            Some("https://host.invalid/download/attachments/4242/diagram.png"),
        );

        let (resolved_id, resolved_key) =
            resolve_child_parent(&row, &parent_index, &external_key_index);
        // Recovered structurally: parent id resolves, no declared key invented.
        assert_eq!(resolved_id, Some(parent_id));
        assert_eq!(resolved_key, None);
    }

    #[test]
    fn structural_recovery_yields_orphan_when_parent_page_absent() {
        let parent_index = HashMap::new();
        let external_key_index = HashMap::new();
        let row = child(
            "opaque-child-key",
            None,
            "primary",
            Some("image/png"),
            Some("https://host.invalid/download/attachments/9999/diagram.png"),
        );

        let (resolved_id, _) = resolve_child_parent(&row, &parent_index, &external_key_index);
        assert_eq!(resolved_id, None);
        // The page id is still extractable, distinguishing orphan from miss.
        assert_eq!(child_attachment_page_id(&row).as_deref(), Some("9999"));
        assert!(!parent_page_exists(&Some("9999".to_string()), &parent_index));
    }

    #[test]
    fn idempotent_when_child_already_attached_and_role_final() {
        let parent_id = Uuid::now_v7();
        let parent_index = HashMap::from([("4242".to_string(), parent_id)]);
        let external_key_index = HashMap::new();
        // A raster-image child already attached and already typed attached_context.
        let row = child(
            "diagram.png",
            None,
            "attached_context",
            Some("image/png"),
            Some("https://host.invalid/download/attachments/4242/diagram.png"),
        );

        let (resolved_id, resolved_key) =
            resolve_child_parent(&row, &parent_index, &external_key_index);
        let is_raster_image =
            revision_is_raster_image(Some(row.external_key.as_str()), row.mime_type.as_deref());
        let target_role = derive_document_role(resolved_id.is_some(), is_raster_image).to_string();

        // Re-resolving an already-attached child reproduces the same canonical
        // parent + role, so a re-run writes the same values (idempotent). The
        // apply itself is unconditional now (it must always mirror onto
        // knowledge_document), so idempotency is a property of the resolution,
        // not of a skip.
        assert_eq!(resolved_id, Some(parent_id), "parent must re-resolve to the same id");
        assert_eq!(resolved_key, None, "structural recovery invents no declared key");
        assert_eq!(target_role, "attached_context", "role must re-derive identically");
    }

    #[test]
    fn non_image_child_is_attachment_image_child_is_attached_context() {
        let parent_id = Uuid::now_v7();
        let parent_index = HashMap::from([("4242".to_string(), parent_id)]);
        let external_key_index = HashMap::new();

        let pdf_child = child(
            "manual.pdf",
            None,
            "primary",
            Some("application/pdf"),
            Some("https://host.invalid/download/attachments/4242/manual.pdf"),
        );
        let (pdf_parent, _) = resolve_child_parent(&pdf_child, &parent_index, &external_key_index);
        let pdf_role = derive_document_role(
            pdf_parent.is_some(),
            revision_is_raster_image(
                Some(pdf_child.external_key.as_str()),
                pdf_child.mime_type.as_deref(),
            ),
        );
        assert_eq!(pdf_role, "attachment");

        let png_child = child(
            "diagram.png",
            None,
            "primary",
            Some("image/png"),
            Some("https://host.invalid/download/attachments/4242/diagram.png"),
        );
        let (png_parent, _) = resolve_child_parent(&png_child, &parent_index, &external_key_index);
        let png_role = derive_document_role(
            png_parent.is_some(),
            revision_is_raster_image(
                Some(png_child.external_key.as_str()),
                png_child.mime_type.as_deref(),
            ),
        );
        assert_eq!(png_role, "attached_context");
    }
}
