#[path = "support/content_lifecycle_support.rs"]
mod content_lifecycle_support;

use anyhow::{Context, Result};
use uuid::Uuid;

use content_lifecycle_support::{ContentLifecycleFixture, revision_command};

use rustrag_backend::services::content::service::CreateDocumentCommand;

#[tokio::test]
#[ignore = "requires local postgres with canonical extensions"]
async fn canonical_content_lifecycle_preserves_logical_document_identity_and_revision_lineage()
-> Result<()> {
    let fixture = ContentLifecycleFixture::create().await?;

    let result = async {
        let external_key = format!("logical-doc-{}", Uuid::now_v7());
        let document = fixture
            .state
            .canonical_services
            .content
            .create_document(
                &fixture.state,
                CreateDocumentCommand {
                    workspace_id: fixture.workspace_id,
                    library_id: fixture.library_id,
                    external_key: Some(external_key.clone()),
                    file_name: None,
                    created_by_principal_id: None,
                },
            )
            .await
            .context("failed to create canonical content document")?;
        assert_eq!(document.workspace_id, fixture.workspace_id);
        assert_eq!(document.library_id, fixture.library_id);
        assert_eq!(document.external_key, external_key);
        assert_eq!(document.document_state, "active");

        let knowledge_document = fixture
            .state
            .arango_document_store
            .get_document(document.id)
            .await
            .context("failed to load knowledge document shell for content lifecycle")?
            .context("knowledge document shell missing from arango")?;
        assert_eq!(knowledge_document.external_key, external_key);
        assert_eq!(knowledge_document.document_state, "active");

        let first_revision = fixture
            .state
            .canonical_services
            .content
            .create_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "upload",
                    "sha256:lifecycle-upload",
                    "Initial Upload",
                    Some("file:///initial.txt"),
                ),
            )
            .await
            .context("failed to create initial revision")?;
        let appended_revision = fixture
            .state
            .canonical_services
            .content
            .append_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "append",
                    "sha256:lifecycle-append",
                    "Appended Revision",
                    None,
                ),
            )
            .await
            .context("failed to append revision")?;
        let replaced_revision = fixture
            .state
            .canonical_services
            .content
            .replace_revision(
                &fixture.state,
                revision_command(
                    document.id,
                    "replace",
                    "sha256:lifecycle-replace",
                    "Replacement Revision",
                    Some("file:///replacement.txt"),
                ),
            )
            .await
            .context("failed to replace revision")?;

        assert_eq!(first_revision.revision_number, 1);
        assert_eq!(appended_revision.revision_number, 2);
        assert_eq!(replaced_revision.revision_number, 3);
        assert_eq!(appended_revision.parent_revision_id, Some(first_revision.id));
        assert_eq!(replaced_revision.parent_revision_id, Some(appended_revision.id));
        assert_eq!(appended_revision.document_id, document.id);
        assert_eq!(replaced_revision.document_id, document.id);

        let revisions = fixture
            .state
            .canonical_services
            .content
            .list_revisions(&fixture.state, document.id)
            .await
            .context("failed to list canonical revisions")?;
        assert_eq!(revisions.len(), 3);
        assert_eq!(
            revisions.iter().map(|revision| revision.id).collect::<Vec<_>>(),
            vec![replaced_revision.id, appended_revision.id, first_revision.id]
        );

        let knowledge_revisions = fixture
            .state
            .arango_document_store
            .list_revisions_by_document(document.id)
            .await
            .context("failed to list knowledge revisions for content lifecycle")?;
        assert_eq!(
            knowledge_revisions.iter().map(|revision| revision.revision_id).collect::<Vec<_>>(),
            vec![replaced_revision.id, appended_revision.id, first_revision.id]
        );
        assert_eq!(knowledge_revisions[0].revision_kind, "replace");
        assert_eq!(knowledge_revisions[1].revision_kind, "append");
        assert_eq!(knowledge_revisions[2].revision_kind, "upload");

        let summaries = fixture
            .state
            .canonical_services
            .content
            .list_documents(&fixture.state, fixture.library_id)
            .await
            .context("failed to list canonical document summaries")?;
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].document.id, document.id);
        assert_eq!(summaries[0].document.external_key, external_key);

        Ok(())
    }
    .await;

    fixture.cleanup().await?;
    result
}
