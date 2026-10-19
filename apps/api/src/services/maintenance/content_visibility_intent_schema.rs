//! Guarded schema transition from the immutable admission-policy baseline.

use std::{collections::BTreeMap, future::Future, path::Path, pin::Pin};

use anyhow::Context as _;
use ironrag_contracts::documents::{
    ImportAdmissionProvenance, ReplacementIdentity, RevisionAdmissionIdentity,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use sqlx::{PgConnection, PgPool};
use tokio::io::AsyncReadExt as _;
use uuid::Uuid;

use super::baseline_delta::{
    AdditiveBaselineDeltaSpec, BaselineDeltaSetup, apply_additive_baseline_delta_with_setup,
};
use crate::infra::persistence::{
    REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY,
    REQUIRED_BASELINE_OBJECT_GROUPS_CONTENT_VISIBILITY_INTENT,
};
use crate::services::content::storage::{BoundedRevisionSource, ContentStorageService};

pub const PREDECESSOR_BASELINE_SHA384: &str =
    super::baseline_delta::BASELINE_REVISION_ADMISSION_VISIBILITY_POLICY_SHA384;

// This is a one-time upgrade audit, never a runtime visibility resolver.
const PRECONDITION: &str = "select case when to_regclass('public.content_visibility_intent') is not null then true else
    not exists (select 1 from content_mutation where mutation_state in ('accepted', 'running'))
    and not exists (select 1 from content_mutation m where to_jsonb(m)->>'idempotency_key' is not null
        and to_jsonb(m)->>'idempotency_scope' is null and requested_by_principal_id is null)
    and not exists (select 1 from content_mutation_item where item_state not in ('applied','failed','conflicted','skipped'))
    and not exists (select 1 from ingest_job where queue_state in ('queued', 'leased', 'paused'))
    and not exists (select 1 from ingest_attempt where attempt_state in ('leased', 'running'))
    and not exists (
        select 1 from ingest_job j
        left join content_mutation m on m.id=j.mutation_id
        left join content_mutation_item i on i.id=j.mutation_item_id
        left join content_document d on d.id=i.document_id
        where j.job_kind='content_mutation' and (
            m.id is null or i.id is null or d.id is null or i.mutation_id<>m.id
            or (m.workspace_id,m.library_id)<>(j.workspace_id,j.library_id)
            or (d.workspace_id,d.library_id)<>(j.workspace_id,j.library_id)
            or j.knowledge_document_id is distinct from d.id
            or j.knowledge_revision_id is distinct from i.result_revision_id
        )
    )
    and not exists (select 1 from ingest_attempt a left join ingest_job j on j.id=a.job_id where j.id is null)
    and not exists (
        select 1 from ingest_job j
        left join content_mutation_item i on i.id=j.mutation_item_id
        left join content_document d on d.id=i.document_id
        left join content_mutation m on m.id=j.mutation_id
        where j.mutation_item_id is not null and (i.id is null or d.id is null or m.id is null
            or i.mutation_id<>m.id or (m.workspace_id,m.library_id)<>(j.workspace_id,j.library_id)
            or (d.workspace_id,d.library_id)<>(j.workspace_id,j.library_id)
            or j.knowledge_document_id is distinct from d.id)
    )
    and not exists (
        select 1 from content_revision r left join content_document d on d.id = r.document_id
        where d.id is null or (r.workspace_id, r.library_id) <> (d.workspace_id, d.library_id)
           or r.revision_number < 0
    )
    and not exists (
        select 1 from content_mutation_item i
        left join content_mutation m on m.id = i.mutation_id
        left join content_document d on d.id = i.document_id
        left join content_revision r on r.id = i.result_revision_id
        left join content_revision b on b.id = i.base_revision_id
        where m.id is null or d.id is null or (m.workspace_id, m.library_id) <> (d.workspace_id, d.library_id)
           or (i.result_revision_id is not null and (r.id is null or r.document_id <> i.document_id))
           or (i.base_revision_id is not null and (b.id is null or b.document_id <> i.document_id))
    )
    and not exists (
        select 1 from content_document_head h
        left join content_document d on d.id = h.document_id
        left join content_revision r on r.id = h.readable_revision_id
        left join content_revision a on a.id = h.active_revision_id
        left join content_mutation m on m.id = h.latest_mutation_id
        where d.id is null
           or (h.active_revision_id is not null and (a.id is null or a.document_id <> d.id))
            or (h.latest_mutation_id is not null and (m.id is null or (m.workspace_id, m.library_id) <> (d.workspace_id, d.library_id)))
            or (h.latest_successful_attempt_id is not null and not exists (
                select 1 from ingest_attempt success join ingest_job j on j.id=success.job_id
                where success.id=h.latest_successful_attempt_id and success.attempt_state='succeeded'
                  and (j.workspace_id,j.library_id)=(d.workspace_id,d.library_id)
                  and j.knowledge_document_id=d.id
            ))
           or (h.readable_revision_id is not null and (
               r.id is null or r.document_id <> d.id
                or (r.workspace_id,r.library_id)<>(d.workspace_id,d.library_id)
            ))
    )
    and not exists (
        select 1 from content_mutation suppressed
        left join content_mutation_item i on i.mutation_id = suppressed.id
        left join content_revision r on r.id = i.result_revision_id
        where to_jsonb(suppressed)->>'visibility_policy' = 'suppressed_until_ready'
          and (i.id is null or r.id is null or not exists (
              select 1 from content_document_head h
              join content_revision visible on visible.id = h.readable_revision_id
              join ingest_attempt success on success.id = h.latest_successful_attempt_id
              join ingest_job j on j.id = success.job_id
              join content_mutation_item applied on applied.id = j.mutation_item_id
              where h.document_id = i.document_id and visible.document_id = i.document_id
                and visible.revision_number >= r.revision_number and applied.item_state = 'applied'
                and applied.document_id = i.document_id and applied.result_revision_id = visible.id
                and success.attempt_state = 'succeeded'
          ))
    )
    and not exists (select 1 from content_mutation m where to_jsonb(m)->'request_identity' is not null
        and to_jsonb(m)->'request_identity' <> 'null'::jsonb
        and jsonb_typeof(to_jsonb(m)->'request_identity') <> 'object')
end";

pub(crate) const CONTENT_VISIBILITY_INTENT_DELTA: AdditiveBaselineDeltaSpec =
    AdditiveBaselineDeltaSpec {
        operator_command: "ironrag-maintenance migrate content-visibility-intent-schema",
        predecessor_sha384_hex: PREDECESSOR_BASELINE_SHA384,
        target_sha384_hex:
            super::baseline_delta::BASELINE_REVISION_CONTENT_VISIBILITY_INTENT_SHA384,
        required_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_CONTENT_VISIBILITY_INTENT,
        prerequisite_object_groups: REQUIRED_BASELINE_OBJECT_GROUPS_ADMISSION_VISIBILITY_POLICY,
        additive_sql: include_str!("../../../ops/apply-content-visibility-intent-schema.sql"),
        precondition: Some((
            PRECONDITION,
            "visibility schema preflight refuses active work, orphan references, scope mismatch or unresolved historical suppression",
        )),
        delta_present: (
            "select to_regclass('public.content_visibility_intent') is not null or to_regclass('public.content_mutation_reference') is not null or to_regclass('public.content_mutation_predecessor_audit') is not null or exists (select 1 from information_schema.columns where table_schema = 'public' and ((table_name = 'content_document' and column_name = 'revision_sequence') or (table_name='content_mutation' and column_name='request_identity') or (table_name='content_revision' and column_name='admission_identity')))",
            "visibility schema objects are already present under a stale checksum; refusing to stamp",
        ),
        predecessor_error: "baseline checksum is not the exact content-visibility-intent predecessor; refusing to apply or stamp",
    };

#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentVisibilityIntentSchemaMigrationReport {
    pub apply: bool,
    pub already_current: bool,
    pub schema_applied: bool,
}

pub async fn migrate_content_visibility_intent_schema(
    postgres: &PgPool,
    apply: bool,
    storage: &ContentStorageService,
    admission: Option<&CertifiedImportAdmission>,
) -> anyhow::Result<ContentVisibilityIntentSchemaMigrationReport> {
    let setup = ImportAdmissionSetup { storage, admission };
    let transition = apply_additive_baseline_delta_with_setup(
        postgres,
        &CONTENT_VISIBILITY_INTENT_DELTA,
        apply,
        Some(&setup),
    )
    .await?;
    Ok(ContentVisibilityIntentSchemaMigrationReport {
        apply,
        already_current: transition.already_current,
        schema_applied: transition.schema_applied,
    })
}

// Explicit per-revision scope, descriptors and storage bindings are required
// even for large predecessor libraries; keep the input bounded, not implicit.
const MAX_CERTIFICATE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_SOURCE_BYTES: usize = 64 * 1024 * 1024;

/// An operator-reviewed certificate, pinned separately from its file contents.
/// These pins are attestations supplied by the operator, not signatures or proof
/// that a backup exists. Admission checks retained rows and the declared evidence.
#[derive(Debug)]
pub struct CertifiedImportAdmission {
    provenance: ImportAdmissionProvenance,
    revisions: BTreeMap<Uuid, CertifiedRevision>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ImportCertificate {
    recovery_snapshot_id: String,
    source_baseline_sha384: String,
    admitted_by: String,
    revisions: Vec<CertifiedRevision>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CertifiedRevision {
    revision_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    storage_key: Option<String>,
    source_evidence: SourceEvidence,
    replacement: ReplacementIdentity,
}

/// Snapshot evidence attests retained materialized text, never original file bytes.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum SourceEvidence {
    SourceBlob,
    #[serde(rename_all = "camelCase")]
    SnapshotRevision {
        knowledge_revision_present: bool,
        normalized_text_sha256: Option<String>,
        text_checksum: Option<String>,
        text_state: Option<String>,
        text_readable_at: Option<chrono::DateTime<chrono::Utc>>,
    },
}

impl CertifiedImportAdmission {
    /// Reads a bounded raw certificate and verifies independently supplied pins.
    /// The digest covers raw row inputs, before admission provenance is added;
    /// no certificate contains its own digest.
    ///
    /// # Errors
    /// Refuses malformed, noncanonical, duplicate or unpinned certificates.
    pub async fn from_file(
        path: &Path,
        expected_sha256: &str,
        accepted_recovery_snapshot_id: &str,
        source_baseline_sha384: &str,
        admitted_by: &str,
    ) -> anyhow::Result<Self> {
        let file =
            tokio::fs::File::open(path).await.context("open import admission certificate")?;
        let mut bytes = Vec::new();
        file.take(MAX_CERTIFICATE_BYTES + 1).read_to_end(&mut bytes).await?;
        anyhow::ensure!(
            bytes.len() as u64 <= MAX_CERTIFICATE_BYTES,
            "import admission certificate exceeds 256 MiB limit"
        );
        Self::parse(
            &bytes,
            expected_sha256,
            accepted_recovery_snapshot_id,
            source_baseline_sha384,
            admitted_by,
        )
    }

    fn parse(
        bytes: &[u8],
        expected_sha256: &str,
        accepted_recovery_snapshot_id: &str,
        source_baseline_sha384: &str,
        admitted_by: &str,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            hex::encode(Sha256::digest(bytes)) == expected_sha256,
            "import admission certificate raw SHA-256 does not match independent operator pin"
        );
        // This recovery contract is for the full schema57 source, not the
        // destructive intent baseline or an arbitrary snapshot schema.
        anyhow::ensure!(
            source_baseline_sha384
                == super::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384,
            "import admission requires the full schema57 source baseline SHA-384"
        );
        let certificate: ImportCertificate =
            serde_json::from_slice(bytes).context("invalid import admission certificate")?;
        anyhow::ensure!(
            certificate.recovery_snapshot_id == accepted_recovery_snapshot_id
                && certificate.source_baseline_sha384 == source_baseline_sha384
                && certificate.admitted_by == admitted_by,
            "import admission certificate metadata does not match independent operator pins"
        );
        let provenance = ImportAdmissionProvenance {
            recovery_snapshot_id: certificate.recovery_snapshot_id,
            identity_certificate_sha256: expected_sha256.to_owned(),
            source_baseline_sha384: certificate.source_baseline_sha384,
            admitted_by: certificate.admitted_by,
        };
        let mut revisions = BTreeMap::new();
        // Require nullable metadata explicitly; serde's Option alone would turn
        // omission into null and certify inputs the operator never supplied.
        let raw: serde_json::Value = serde_json::from_slice(bytes)?;
        let rows = raw["revisions"].as_array().context("certificate revisions must be an array")?;
        for (revision, row) in certificate.revisions.into_iter().zip(rows) {
            anyhow::ensure!(
                row.get("storageKey").is_some(),
                "certificate requires explicit storageKey, including null"
            );
            for field in ["title", "languageCode", "sourceUri", "documentHint"] {
                anyhow::ensure!(
                    row["replacement"].get(field).is_some(),
                    "import certificate replacement requires explicit {field}, including null"
                );
            }
            let identity = RevisionAdmissionIdentity::CertifiedImport {
                replacement: revision.replacement.clone(),
                provenance: provenance.clone(),
            };
            anyhow::ensure!(
                identity.clone().normalize()? == identity,
                "import certificate identity must already be canonical"
            );
            match &revision.source_evidence {
                SourceEvidence::SourceBlob => anyhow::ensure!(
                    revision
                        .storage_key
                        .as_ref()
                        .is_some_and(|key| !key.is_empty() && !key.chars().any(char::is_control)),
                    "source_blob requires an explicit source blob key"
                ),
                SourceEvidence::SnapshotRevision {
                    knowledge_revision_present,
                    normalized_text_sha256,
                    text_checksum,
                    text_state,
                    text_readable_at,
                } => {
                    anyhow::ensure!(
                        revision.storage_key.is_none(),
                        "snapshot_revision requires null storageKey"
                    );
                    for field in
                        ["normalizedTextSha256", "textChecksum", "textState", "textReadableAt"]
                    {
                        anyhow::ensure!(
                            row["sourceEvidence"].get(field).is_some(),
                            "snapshot_revision requires explicit {field}, including null"
                        );
                    }
                    anyhow::ensure!(
                        normalized_text_sha256.as_ref().is_none_or(|digest| is_digest(digest, 64)),
                        "snapshot text digest must be lowercase SHA-256 without prefix"
                    );
                    anyhow::ensure!(
                        if *knowledge_revision_present {
                            text_state.is_some()
                        } else {
                            normalized_text_sha256.is_none()
                                && text_checksum.is_none()
                                && text_state.is_none()
                                && text_readable_at.is_none()
                        },
                        "snapshot knowledge presence and metadata disagree"
                    );
                }
            }
            anyhow::ensure!(
                revisions.insert(revision.revision_id, revision).is_none(),
                "import certificate contains duplicate revision IDs"
            );
        }
        // Validate provenance for empty certificates too.
        anyhow::ensure!(
            is_digest(accepted_recovery_snapshot_id, 64)
                && is_digest(expected_sha256, 64)
                && !admitted_by.trim().is_empty()
                && admitted_by == admitted_by.trim()
                && !admitted_by.chars().any(char::is_control),
            "import admission requires canonical snapshot, certificate and admitting operator evidence"
        );
        Ok(Self { provenance, revisions })
    }
}

fn is_digest(value: &str, length: usize) -> bool {
    value.len() == length
        && value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[derive(Debug, sqlx::FromRow)]
struct ReadableRevision {
    revision_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    storage_key: Option<String>,
    checksum: String,
    byte_size: i64,
    mime_type: String,
    title: Option<String>,
    language_code: Option<String>,
    source_uri: Option<String>,
    document_hint: Option<String>,
    knowledge_revision_present: bool,
    knowledge_scope_matches: bool,
    normalized_text_sha256: Option<String>,
    text_checksum: Option<String>,
    text_state: Option<String>,
    text_readable_at: Option<chrono::DateTime<chrono::Utc>>,
}

fn validate_revision_binding(
    certified: &CertifiedRevision,
    actual: &ReadableRevision,
) -> anyhow::Result<()> {
    let replacement = &certified.replacement;
    anyhow::ensure!(
        certified.revision_id == actual.revision_id
            && certified.document_id == actual.document_id
            && certified.workspace_id == actual.workspace_id
            && certified.library_id == actual.library_id
            && certified.storage_key == actual.storage_key
            && replacement.checksum == actual.checksum
            && replacement.byte_size == actual.byte_size
            && replacement.mime_type == actual.mime_type
            && replacement.title == actual.title
            && replacement.language_code == actual.language_code
            && replacement.source_uri == actual.source_uri
            && replacement.document_hint == actual.document_hint,
        "import certificate revision scope, descriptor, initial import hint or blob binding mismatch"
    );
    if let SourceEvidence::SnapshotRevision {
        knowledge_revision_present,
        normalized_text_sha256,
        text_checksum,
        text_state,
        text_readable_at,
    } = &certified.source_evidence
    {
        let digest = &actual.normalized_text_sha256;
        anyhow::ensure!(
            actual.storage_key.is_none()
                && *knowledge_revision_present == actual.knowledge_revision_present
                && actual.knowledge_scope_matches
                && normalized_text_sha256 == digest
                && *text_checksum == actual.text_checksum
                && *text_state == actual.text_state
                && *text_readable_at == actual.text_readable_at
                && digest.as_ref().is_none_or(|hash| Some(hash) == actual.text_checksum.as_ref()),
            "snapshot revision knowledge presence, scope, metadata or retained text digest mismatch"
        );
    }
    // Filename is deliberately absent here: it is explicit operator input,
    // never reverse-engineered from a storage key or inherited document name.
    Ok(())
}

fn validate_source_bytes(replacement: &ReplacementIdentity, bytes: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(
        i64::try_from(bytes.len())? == replacement.byte_size
            && format!("sha256:{}", hex::encode(Sha256::digest(bytes))) == replacement.checksum,
        "import certificate source blob checksum or size mismatch"
    );
    Ok(())
}

struct ImportAdmissionSetup<'a> {
    storage: &'a ContentStorageService,
    admission: Option<&'a CertifiedImportAdmission>,
}

impl BaselineDeltaSetup for ImportAdmissionSetup<'_> {
    fn prepare<'a>(
        &'a self,
        connection: &'a mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // This query deliberately targets the historical predecessor schema;
            // runtime SQL is needed because the shipped schema retires its model.
            let revisions: Vec<ReadableRevision> = sqlx::query_as(
                "select r.id as revision_id, r.document_id, r.workspace_id, r.library_id,
                    r.storage_key, r.checksum, r.byte_size, r.mime_type, r.title,
                     r.language_code, r.source_uri, r.document_hint,
                     k.revision_id IS NOT NULL AS knowledge_revision_present,
                     (k.revision_id IS NULL OR (k.workspace_id,k.library_id,k.document_id) =
                         (r.workspace_id,r.library_id,r.document_id)) AS knowledge_scope_matches,
                     encode(sha256(convert_to(k.normalized_text, 'UTF8')), 'hex') AS normalized_text_sha256,
                     k.text_checksum, k.text_state, k.text_readable_at
                 from public.content_document_head h
                  join public.content_revision r on r.id = h.readable_revision_id
                  left join public.knowledge_revision k on k.revision_id = r.id
                 order by r.id",
            )
            .fetch_all(&mut *connection)
            .await?;
            let expected = self.admission.map(|admission| &admission.revisions);
            validate_readable_set(&revisions, expected)?;
            // Never reuse a session-global or public staging object. A preexisting
            // temporary object is refused rather than trusted or overwritten.
            sqlx::raw_sql("CREATE TEMP TABLE content_revision_import_admission (revision_id uuid PRIMARY KEY, admission_identity jsonb NOT NULL) ON COMMIT DROP")
                .execute(&mut *connection).await?;
            sqlx::raw_sql(
                "CREATE TEMP TABLE content_visibility_transition_heads ON COMMIT DROP AS
                 SELECT h.document_id, to_jsonb(h) AS original_head,
                     to_jsonb(r) AS original_revision,
                     (d.document_state = 'active' AND d.deleted_at IS NULL
                          AND k.text_state = 'text_readable' AND k.text_readable_at IS NOT NULL
                          AND (k.workspace_id,k.library_id,k.document_id) =
                              (d.workspace_id,d.library_id,d.id)) AS was_active_published
                 FROM public.content_document_head h
                 JOIN public.content_document d ON d.id = h.document_id
                  LEFT JOIN public.content_revision r ON r.id = h.readable_revision_id
                  LEFT JOIN public.knowledge_revision k ON k.revision_id = r.id",
            )
            .execute(&mut *connection)
            .await?;
            for revision in &revisions {
                let admission = self
                    .admission
                    .context("readable revisions require an import admission certificate")?;
                let certified = admission
                    .revisions
                    .get(&revision.revision_id)
                    .context("import certificate omits a readable revision")?;
                validate_revision_binding(certified, revision)?;
                if let SourceEvidence::SourceBlob = &certified.source_evidence {
                    anyhow::ensure!(
                        revision.byte_size >= 0 && revision.byte_size <= MAX_SOURCE_BYTES as i64,
                        "source blob exceeds bounded 64 MiB verifier; reviewed streaming byte-identity evidence is required before admission"
                    );
                    let key = certified
                        .storage_key
                        .as_deref()
                        .context("source_blob requires storageKey")?;
                    let source = self.storage.read_revision_source_bounded(key, MAX_SOURCE_BYTES)
                    .await.context("cannot certify readable source bytes: accessible revision-bound blob with exact checksum and size is required")?;
                    let BoundedRevisionSource::Complete(bytes) = source else {
                        anyhow::bail!(
                            "source blob exceeds bounded verifier; complete byte-identity evidence is required"
                        );
                    };
                    validate_source_bytes(&certified.replacement, &bytes)?;
                }
                let identity = RevisionAdmissionIdentity::CertifiedImport {
                    replacement: certified.replacement.clone(),
                    provenance: admission.provenance.clone(),
                };
                sqlx::query("insert into pg_temp.content_revision_import_admission (revision_id, admission_identity) values ($1, $2)")
                    .bind(revision.revision_id)
                    .bind(sqlx::types::Json(identity))
                    .execute(&mut *connection).await?;
            }
            Ok(())
        })
    }

    fn verify<'a>(
        &'a self,
        connection: &'a mut PgConnection,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // JSON containment compares every predecessor column, not a selected
            // subset of pointers. Added columns may exist, but no original head
            // pointer, summary, timestamp or readable revision field may change.
            let preserved: bool = sqlx::query_scalar(
                "SELECT
                   (SELECT count(*) FROM pg_temp.content_visibility_transition_heads)
                     = (SELECT count(*) FROM public.content_document_head)
                   AND NOT EXISTS (
                     SELECT 1 FROM pg_temp.content_visibility_transition_heads saved
                     LEFT JOIN public.content_document_head h ON h.document_id=saved.document_id
                     LEFT JOIN public.content_revision r ON r.id=h.readable_revision_id
                     WHERE h.document_id IS NULL
                        OR NOT (saved.original_head <@ to_jsonb(h))
                        OR (saved.original_revision IS NOT NULL AND
                            (r.id IS NULL OR NOT (saved.original_revision <@ to_jsonb(r))))
                   )
                   AND NOT EXISTS (
                     SELECT 1 FROM pg_temp.content_revision_import_admission admitted
                     LEFT JOIN public.content_revision r ON r.id=admitted.revision_id
                     WHERE r.id IS NULL OR r.admission_identity IS DISTINCT FROM admitted.admission_identity
                   )
                   AND NOT EXISTS (
                     (SELECT saved.document_id, (saved.original_head->>'readable_revision_id')::uuid
                        FROM pg_temp.content_visibility_transition_heads saved WHERE saved.was_active_published
                      EXCEPT SELECT document_id, revision_id FROM public.content_current_revision)
                     UNION ALL
                     (SELECT document_id, revision_id FROM public.content_current_revision
                      EXCEPT SELECT saved.document_id, (saved.original_head->>'readable_revision_id')::uuid
                        FROM pg_temp.content_visibility_transition_heads saved WHERE saved.was_active_published)
                   )",
            ).fetch_one(&mut *connection).await?;
            anyhow::ensure!(
                preserved,
                "visibility transition lost or changed predecessor heads, readable revisions, admitted identities or active published view membership; refusing to stamp"
            );
            // Force the newly enqueued readable-head guards to run before stamp.
            sqlx::raw_sql("SET CONSTRAINTS ALL IMMEDIATE").execute(&mut *connection).await?;
            Ok(())
        })
    }
}

fn validate_readable_set(
    revisions: &[ReadableRevision],
    certificate: Option<&BTreeMap<Uuid, CertifiedRevision>>,
) -> anyhow::Result<()> {
    if revisions.is_empty() && certificate.is_none() {
        return Ok(());
    }
    let certificate = certificate.context(
        "readable predecessor revisions require --import-admission-manifest and independent certificate, accepted recovery snapshot, source baseline and operator pins, including in dry-run",
    )?;
    anyhow::ensure!(
        revisions.len() == certificate.len()
            && revisions.iter().all(|revision| certificate.contains_key(&revision.revision_id)),
        "import certificate must cover exactly the predecessor readable revision IDs"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replacement() -> ReplacementIdentity {
        ReplacementIdentity {
            checksum: format!("sha256:{}", hex::encode(Sha256::digest(b"certified bytes"))),
            byte_size: 15,
            mime_type: "text/plain".into(),
            file_name: "operator-selected.txt".into(),
            title: Some("Admitted title".into()),
            language_code: Some("en".into()),
            source_uri: Some("https://example.invalid/source".into()),
            document_hint: Some("Initial import hint".into()),
        }
    }

    fn certificate_value() -> serde_json::Value {
        serde_json::json!({
            "recoverySnapshotId": "a".repeat(64),
            "sourceBaselineSha384": super::super::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384,
            "admittedBy": "operator@example.invalid",
            "revisions": [{
                "revisionId": Uuid::from_u128(1),
                "documentId": Uuid::from_u128(2),
                "workspaceId": Uuid::from_u128(3),
                "libraryId": Uuid::from_u128(4),
                "storageKey": "content/retained-source-object",
                "sourceEvidence": {"kind": "source_blob"},
                "replacement": replacement(),
            }],
        })
    }

    fn parse_value(value: &serde_json::Value) -> anyhow::Result<CertifiedImportAdmission> {
        let bytes = serde_json::to_vec(value)?;
        CertifiedImportAdmission::parse(
            &bytes,
            &hex::encode(Sha256::digest(&bytes)),
            &"a".repeat(64),
            super::super::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384,
            "operator@example.invalid",
        )
    }

    fn readable() -> ReadableRevision {
        let replacement = replacement();
        ReadableRevision {
            revision_id: Uuid::from_u128(1),
            document_id: Uuid::from_u128(2),
            workspace_id: Uuid::from_u128(3),
            library_id: Uuid::from_u128(4),
            storage_key: Some("content/retained-source-object".into()),
            checksum: replacement.checksum,
            byte_size: replacement.byte_size,
            mime_type: replacement.mime_type,
            title: replacement.title,
            language_code: replacement.language_code,
            source_uri: replacement.source_uri,
            document_hint: replacement.document_hint,
            knowledge_revision_present: false,
            knowledge_scope_matches: true,
            normalized_text_sha256: None,
            text_checksum: None,
            text_state: None,
            text_readable_at: None,
        }
    }

    #[test]
    fn raw_certificate_digest_and_independent_metadata_pins_are_required() -> anyhow::Result<()> {
        let value = certificate_value();
        let bytes = serde_json::to_vec(&value)?;
        let digest = hex::encode(Sha256::digest(&bytes));
        let baseline = super::super::baseline_delta::BASELINE_REVISION_DECISION_CATALOG_SHA384;
        let parse = |bytes: &[u8], hash: &str, snapshot: &str, source: &str, operator: &str| {
            CertifiedImportAdmission::parse(bytes, hash, snapshot, source, operator)
        };
        let admitted =
            parse(&bytes, &digest, &"a".repeat(64), baseline, "operator@example.invalid")?;
        assert_eq!(admitted.provenance.identity_certificate_sha256, digest);
        let mut reserialized = bytes.clone();
        reserialized.push(b' ');
        assert!(
            parse(&reserialized, &digest, &"a".repeat(64), baseline, "operator@example.invalid")
                .is_err()
        );
        assert!(
            parse(&bytes, &digest, &"b".repeat(64), baseline, "operator@example.invalid").is_err()
        );
        assert!(
            parse(
                &bytes,
                &digest,
                &"a".repeat(64),
                PREDECESSOR_BASELINE_SHA384,
                "operator@example.invalid"
            )
            .is_err()
        );
        assert!(parse(&bytes, &digest, &"a".repeat(64), baseline, "other-operator").is_err());
        Ok(())
    }

    #[test]
    fn certificate_rejects_missing_filename_metadata_duplicates_and_noncanonical_inputs() {
        for field in ["fileName", "title", "languageCode", "sourceUri", "documentHint"] {
            let mut value = certificate_value();
            value["revisions"][0]["replacement"].as_object_mut().unwrap().remove(field);
            assert!(parse_value(&value).is_err(), "omitted {field} must refuse admission");
        }
        let mut value = certificate_value();
        let duplicate = value["revisions"][0].clone();
        value["revisions"].as_array_mut().unwrap().push(duplicate);
        assert!(parse_value(&value).is_err());
        let mut value = certificate_value();
        value["revisions"][0]["replacement"]["mimeType"] = " Text/Plain ".into();
        assert!(parse_value(&value).is_err());
        let mut value = certificate_value();
        value["revisions"][0]["replacement"]["historicalFilename"] = "not accepted".into();
        assert!(parse_value(&value).is_err());
    }

    #[test]
    fn certificate_nullable_metadata_must_be_explicit_but_can_be_null() -> anyhow::Result<()> {
        let mut value = certificate_value();
        for field in ["title", "languageCode", "sourceUri", "documentHint"] {
            value["revisions"][0]["replacement"][field] = serde_json::Value::Null;
        }
        let admission = parse_value(&value)?;
        let identity = &admission.revisions[&Uuid::from_u128(1)].replacement;
        assert!(identity.title.is_none() && identity.document_hint.is_none());
        Ok(())
    }

    #[test]
    fn exact_readable_set_refuses_missing_extra_and_absent_certificate() -> anyhow::Result<()> {
        let admission = parse_value(&certificate_value())?;
        let actual = [readable()];
        validate_readable_set(&actual, Some(&admission.revisions))?;
        assert!(validate_readable_set(&actual, None).is_err());
        assert!(validate_readable_set(&[], Some(&admission.revisions)).is_err());
        let mut other = readable();
        other.revision_id = Uuid::from_u128(9);
        assert!(validate_readable_set(&[other], Some(&admission.revisions)).is_err());
        let mut empty = certificate_value();
        empty["revisions"] = serde_json::json!([]);
        let empty = parse_value(&empty)?;
        assert!(validate_readable_set(&actual, Some(&empty.revisions)).is_err());
        validate_readable_set(&[], Some(&empty.revisions))?;
        validate_readable_set(&[], None)?;
        Ok(())
    }

    #[test]
    fn every_revision_scope_descriptor_and_blob_binding_is_checked() -> anyhow::Result<()> {
        let admission = parse_value(&certificate_value())?;
        let certified = &admission.revisions[&Uuid::from_u128(1)];
        validate_revision_binding(certified, &readable())?;
        for field in 0..12 {
            let mut actual = readable();
            match field {
                0 => actual.revision_id = Uuid::from_u128(99),
                1 => actual.document_id = Uuid::from_u128(99),
                2 => actual.workspace_id = Uuid::from_u128(99),
                3 => actual.library_id = Uuid::from_u128(99),
                4 => actual.storage_key = None,
                5 => actual.checksum = "sha256:unverified".into(),
                6 => actual.byte_size += 1,
                7 => actual.mime_type = "text/html".into(),
                8 => actual.title = None,
                9 => actual.language_code = None,
                10 => actual.source_uri = None,
                _ => actual.document_hint = None,
            }
            assert!(
                validate_revision_binding(certified, &actual).is_err(),
                "binding field {field}"
            );
        }
        // This name cannot be derived from the blob key; admission preserves the
        // filename explicitly selected in the reviewed certificate instead.
        assert_eq!(certified.replacement.file_name, "operator-selected.txt");
        Ok(())
    }

    #[test]
    fn snapshot_evidence_binds_retained_text_without_claiming_original_bytes() -> anyhow::Result<()>
    {
        let mut value = snapshot_certificate_value();
        let admission = parse_value(&value)?;
        let certified = &admission.revisions[&Uuid::from_u128(1)];
        let mut actual = snapshot_readable();
        validate_revision_binding(certified, &actual)?;
        // The retained normalized text is not the original file payload or digest.
        assert!(
            validate_source_bytes(&certified.replacement, b"retained materialized text").is_err()
        );
        for field in 0..7 {
            let mut changed = snapshot_readable();
            match field {
                0 => {
                    changed.normalized_text_sha256 =
                        Some(hex::encode(Sha256::digest(b"changed text")))
                }
                1 => changed.text_checksum = Some("b".repeat(64)),
                2 => changed.knowledge_revision_present = false,
                3 => changed.text_state = Some("accepted".into()),
                4 => changed.text_readable_at = None,
                5 => changed.knowledge_scope_matches = false,
                _ => changed.storage_key = Some("mutable/projection/locator".into()),
            }
            assert!(
                validate_revision_binding(certified, &changed).is_err(),
                "snapshot field {field}"
            );
        }
        // A freshly pinned but false digest still fails against retained text.
        value["revisions"][0]["sourceEvidence"]["normalizedTextSha256"] =
            serde_json::json!("b".repeat(64));
        let false_admission = parse_value(&value)?;
        assert!(
            validate_revision_binding(&false_admission.revisions[&actual.revision_id], &actual)
                .is_err()
        );
        actual.normalized_text_sha256 = None;
        assert!(validate_revision_binding(certified, &actual).is_err());
        Ok(())
    }

    fn snapshot_certificate_value() -> serde_json::Value {
        let mut value = certificate_value();
        value["revisions"][0]["storageKey"] = serde_json::Value::Null;
        value["revisions"][0]["sourceEvidence"] = serde_json::json!({
            "kind": "snapshot_revision",
            "knowledgeRevisionPresent": true,
            "normalizedTextSha256": hex::encode(Sha256::digest("retained materialized text".as_bytes())),
            "textChecksum": hex::encode(Sha256::digest("retained materialized text".as_bytes())),
            "textState": "text_readable",
            "textReadableAt": "2026-10-19T00:00:00Z",
        });
        value
    }

    fn snapshot_readable() -> ReadableRevision {
        let mut actual = readable();
        actual.storage_key = None;
        actual.knowledge_revision_present = true;
        actual.normalized_text_sha256 =
            Some(hex::encode(Sha256::digest(b"retained materialized text")));
        actual.text_checksum = actual.normalized_text_sha256.clone();
        actual.text_state = Some("text_readable".into());
        actual.text_readable_at = Some("2026-10-19T00:00:00Z".parse().unwrap());
        actual
    }

    #[test]
    fn snapshot_missing_projection_is_metadata_only_and_presence_is_exact() -> anyhow::Result<()> {
        let mut value = snapshot_certificate_value();
        let evidence = &mut value["revisions"][0]["sourceEvidence"];
        evidence["knowledgeRevisionPresent"] = serde_json::json!(false);
        for field in ["normalizedTextSha256", "textChecksum", "textState", "textReadableAt"] {
            evidence[field] = serde_json::Value::Null;
        }
        let admission = parse_value(&value)?;
        let certified = &admission.revisions[&Uuid::from_u128(1)];
        let mut actual = readable();
        actual.storage_key = None;
        validate_revision_binding(certified, &actual)?;
        assert!(validate_revision_binding(certified, &snapshot_readable()).is_err());
        // Explicitly retained knowledge without text is different from no row.
        actual.knowledge_revision_present = true;
        actual.text_state = Some("failed".into());
        assert!(validate_revision_binding(certified, &actual).is_err());
        value["revisions"][0]["sourceEvidence"]["knowledgeRevisionPresent"] =
            serde_json::json!(true);
        value["revisions"][0]["sourceEvidence"]["textState"] = serde_json::json!("failed");
        let admission = parse_value(&value)?;
        validate_revision_binding(&admission.revisions[&actual.revision_id], &actual)?;
        Ok(())
    }

    #[test]
    fn source_evidence_and_nullable_snapshot_fields_must_be_explicit() {
        for field in ["storageKey", "sourceEvidence"] {
            let mut value = certificate_value();
            value["revisions"][0].as_object_mut().unwrap().remove(field);
            assert!(parse_value(&value).is_err(), "missing {field}");
            let mut value = certificate_value();
            value["revisions"][0][field] = serde_json::Value::Null;
            assert!(parse_value(&value).is_err(), "null {field}");
        }
        for field in [
            "kind",
            "knowledgeRevisionPresent",
            "normalizedTextSha256",
            "textChecksum",
            "textState",
            "textReadableAt",
        ] {
            let mut value = snapshot_certificate_value();
            value["revisions"][0]["sourceEvidence"].as_object_mut().unwrap().remove(field);
            assert!(parse_value(&value).is_err(), "missing {field}");
        }
        let mut value = snapshot_certificate_value();
        value["revisions"][0]["storageKey"] = serde_json::json!("mutable/projection/locator");
        assert!(parse_value(&value).is_err());
        let mut value = snapshot_certificate_value();
        value["revisions"][0]["sourceEvidence"]["knowledgeRevisionPresent"] =
            serde_json::json!(false);
        assert!(parse_value(&value).is_err());
    }

    #[test]
    fn source_metadata_or_blob_existence_does_not_prove_byte_identity() -> anyhow::Result<()> {
        let descriptor = replacement();
        validate_source_bytes(&descriptor, b"certified bytes")?;
        assert!(validate_source_bytes(&descriptor, b"different bytes").is_err());
        assert!(validate_source_bytes(&descriptor, b"certified byte").is_err());
        assert!(validate_source_bytes(&descriptor, b"").is_err());
        Ok(())
    }
}
