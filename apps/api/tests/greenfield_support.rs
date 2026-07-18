use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldWorkspace {
    pub(crate) id: Uuid,
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldLibrary {
    pub(crate) id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) slug: String,
    pub(crate) name: String,
    pub(crate) status: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldPrincipal {
    pub(crate) id: Uuid,
    pub(crate) kind: String,
    pub(crate) login: String,
    pub(crate) email: String,
    pub(crate) display_name: String,
    pub(crate) status: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum GreenfieldGrantTargetKind {
    Workspace,
    Library,
    Document,
    Connector,
    Credential,
    Binding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldGrant {
    pub(crate) id: Uuid,
    pub(crate) principal_id: Uuid,
    pub(crate) target_kind: GreenfieldGrantTargetKind,
    pub(crate) target_id: Uuid,
    pub(crate) permission: String,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldDocument {
    pub(crate) id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) external_key: String,
    pub(crate) title: String,
    pub(crate) mime_type: String,
    pub(crate) status: String,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldRevision {
    pub(crate) id: Uuid,
    pub(crate) document_id: Uuid,
    pub(crate) revision_no: i64,
    pub(crate) checksum: String,
    pub(crate) body_text: String,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldAttempt {
    pub(crate) id: Uuid,
    pub(crate) document_id: Uuid,
    pub(crate) operation_kind: String,
    pub(crate) status: String,
    pub(crate) stage: String,
    pub(crate) retry_count: i32,
    pub(crate) started_at: Option<DateTime<Utc>>,
    pub(crate) finished_at: Option<DateTime<Utc>>,
    pub(crate) created_at: DateTime<Utc>,
    pub(crate) updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldAuditEvent {
    pub(crate) id: Uuid,
    pub(crate) request_id: String,
    pub(crate) actor_principal_id: Option<Uuid>,
    pub(crate) action_kind: String,
    pub(crate) subject_kind: String,
    pub(crate) subject_id: Option<Uuid>,
    pub(crate) result_kind: String,
    pub(crate) details: Value,
    pub(crate) created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct GreenfieldFixtureBundle {
    pub(crate) workspace: GreenfieldWorkspace,
    pub(crate) library: GreenfieldLibrary,
    pub(crate) principal: GreenfieldPrincipal,
    pub(crate) grant: GreenfieldGrant,
    pub(crate) document: GreenfieldDocument,
    pub(crate) revision: GreenfieldRevision,
    pub(crate) attempt: GreenfieldAttempt,
    pub(crate) audit_event: GreenfieldAuditEvent,
}

#[must_use]
pub(crate) fn sample_workspace() -> GreenfieldWorkspace {
    let now = Utc::now();
    GreenfieldWorkspace {
        id: Uuid::now_v7(),
        slug: "agent-workspace".to_string(),
        name: "Agent Workspace".to_string(),
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    }
}

#[must_use]
pub(crate) fn sample_library(workspace_id: Uuid) -> GreenfieldLibrary {
    let now = Utc::now();
    GreenfieldLibrary {
        id: Uuid::now_v7(),
        workspace_id,
        slug: "agent-library".to_string(),
        name: "Agent Library".to_string(),
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    }
}

#[must_use]
pub(crate) fn sample_principal() -> GreenfieldPrincipal {
    let now = Utc::now();
    GreenfieldPrincipal {
        id: Uuid::now_v7(),
        kind: "user".to_string(),
        login: "agent".to_string(),
        email: "agent@example.local".to_string(),
        display_name: "Agent".to_string(),
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    }
}

#[must_use]
pub(crate) fn sample_grant(
    principal_id: Uuid,
    target_kind: GreenfieldGrantTargetKind,
    target_id: Uuid,
) -> GreenfieldGrant {
    GreenfieldGrant {
        id: Uuid::now_v7(),
        principal_id,
        target_kind,
        target_id,
        permission: "read".to_string(),
        created_at: Utc::now(),
    }
}

#[must_use]
pub(crate) fn sample_document(workspace_id: Uuid, library_id: Uuid) -> GreenfieldDocument {
    let now = Utc::now();
    GreenfieldDocument {
        id: Uuid::now_v7(),
        workspace_id,
        library_id,
        external_key: format!("doc-{}", Uuid::now_v7()),
        title: "Agent Memory Document".to_string(),
        mime_type: "text/plain".to_string(),
        status: "active".to_string(),
        created_at: now,
        updated_at: now,
    }
}

#[must_use]
pub(crate) fn sample_revision(document_id: Uuid) -> GreenfieldRevision {
    GreenfieldRevision {
        id: Uuid::now_v7(),
        document_id,
        revision_no: 1,
        checksum: "sha256:sample".to_string(),
        body_text: "Greenfield fixture revision body.".to_string(),
        created_at: Utc::now(),
    }
}

#[must_use]
pub(crate) fn sample_attempt(document_id: Uuid) -> GreenfieldAttempt {
    let now = Utc::now();
    GreenfieldAttempt {
        id: Uuid::now_v7(),
        document_id,
        operation_kind: "upload".to_string(),
        status: "accepted".to_string(),
        stage: "queued".to_string(),
        retry_count: 0,
        started_at: None,
        finished_at: None,
        created_at: now,
        updated_at: now,
    }
}

#[must_use]
pub(crate) fn sample_audit_event(
    subject_kind: impl Into<String>,
    subject_id: Option<Uuid>,
) -> GreenfieldAuditEvent {
    GreenfieldAuditEvent {
        id: Uuid::now_v7(),
        request_id: Uuid::now_v7().to_string(),
        actor_principal_id: None,
        action_kind: "catalog.create".to_string(),
        subject_kind: subject_kind.into(),
        subject_id,
        result_kind: "succeeded".to_string(),
        details: serde_json::json!({
            "source": "greenfield-fixture",
        }),
        created_at: Utc::now(),
    }
}

#[must_use]
pub(crate) fn sample_fixture_bundle() -> GreenfieldFixtureBundle {
    let workspace = sample_workspace();
    let library = sample_library(workspace.id);
    let principal = sample_principal();
    let grant = sample_grant(principal.id, GreenfieldGrantTargetKind::Workspace, workspace.id);
    let document = sample_document(workspace.id, library.id);
    let revision = sample_revision(document.id);
    let attempt = sample_attempt(document.id);
    let audit_event = sample_audit_event("workspace", Some(workspace.id));

    GreenfieldFixtureBundle {
        workspace,
        library,
        principal,
        grant,
        document,
        revision,
        attempt,
        audit_event,
    }
}

#[test]
fn fixture_bundle_keeps_canonical_relationships_intact() {
    let bundle = sample_fixture_bundle();

    assert_eq!(bundle.library.workspace_id, bundle.workspace.id);
    assert_eq!(bundle.grant.principal_id, bundle.principal.id);
    assert_eq!(bundle.document.library_id, bundle.library.id);
    assert_eq!(bundle.revision.document_id, bundle.document.id);
    assert_eq!(bundle.attempt.document_id, bundle.document.id);
}
