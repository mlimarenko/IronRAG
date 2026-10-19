use super::*;

/// Context shared by both catalog deletion workers. A workspace deletion
/// carries no library id, so the library worker takes it as a separate
/// argument rather than a second near-identical context type.
pub(crate) struct CatalogDeletionWorker {
    pub(crate) operation_id: Uuid,
    pub(crate) workspace_id: Uuid,
    pub(crate) display_name: String,
    pub(crate) principal_id: Uuid,
    pub(crate) request_id: Option<String>,
}

pub(crate) fn spawn_workspace_deletion_worker(state: AppState, worker: CatalogDeletionWorker) {
    let CatalogDeletionWorker {
        operation_id,
        workspace_id,
        display_name,
        principal_id,
        request_id,
    } = worker;
    tokio::spawn(
        async move {
            match state
                .application_services
                .catalog
                .execute_workspace_deletion(&state, operation_id, workspace_id)
                .await
            {
                Ok(workspace) => {
                    record_rest_audit_event(
                        &state,
                        principal_id,
                        RestAuditEvent {
                            request_id,
                            action_kind: "catalog.workspace.delete".to_string(),
                            result_kind: "succeeded".to_string(),
                            redacted_message: Some(format!(
                                "workspace {} deleted",
                                workspace.display_name
                            )),
                            internal_message: Some(format!(
                                "principal {principal_id} deleted workspace {} via async operation {operation_id}",
                                workspace.id
                            )),
                            subjects: vec![AppendAuditEventSubjectCommand {
                                subject_kind: "workspace".to_string(),
                                subject_id: workspace.id,
                                workspace_id: Some(workspace.id),
                                library_id: None,
                                document_id: None,
                            }],
                        },
                    )
                    .await;
                }
                Err(error) => {
                    tracing::error!(
                        %operation_id,
                        %workspace_id,
                        error = ?error,
                        "workspace deletion worker failed"
                    );
                    record_rest_audit_event(
                        &state,
                        principal_id,
                        RestAuditEvent {
                            request_id,
                            action_kind: "catalog.workspace.delete".to_string(),
                            result_kind: "failed".to_string(),
                            redacted_message: Some(format!(
                                "workspace {display_name} deletion failed"
                            )),
                            internal_message: Some(format!(
                                "principal {principal_id} failed to delete workspace {workspace_id} via async operation {operation_id}: {error:?}"
                            )),
                            subjects: vec![AppendAuditEventSubjectCommand {
                                subject_kind: "workspace".to_string(),
                                subject_id: workspace_id,
                                workspace_id: Some(workspace_id),
                                library_id: None,
                                document_id: None,
                            }],
                        },
                    )
                    .await;
                }
            }
        }
        .instrument(tracing::info_span!(
            "catalog.workspace_delete.worker",
            %operation_id,
            %workspace_id
        )),
    );
}

pub(crate) fn spawn_library_deletion_worker(
    state: AppState,
    worker: CatalogDeletionWorker,
    library_id: Uuid,
) {
    let CatalogDeletionWorker {
        operation_id,
        workspace_id,
        display_name,
        principal_id,
        request_id,
    } = worker;
    tokio::spawn(
        async move {
            match state
                .application_services
                .catalog
                .execute_library_deletion(&state, operation_id, library_id)
                .await
            {
                Ok(library) => {
                    record_rest_audit_event(
                        &state,
                        principal_id,
                        RestAuditEvent {
                            request_id,
                            action_kind: "catalog.library.delete".to_string(),
                            result_kind: "succeeded".to_string(),
                            redacted_message: Some(format!("library {} deleted", library.display_name)),
                            internal_message: Some(format!(
                                "principal {principal_id} deleted library {} in workspace {} via async operation {operation_id}",
                                library.id, library.workspace_id
                            )),
                            subjects: vec![AppendAuditEventSubjectCommand {
                                subject_kind: "library".to_string(),
                                subject_id: library.id,
                                workspace_id: Some(library.workspace_id),
                                library_id: Some(library.id),
                                document_id: None,
                            }],
                        },
                    )
                    .await;
                }
                Err(error) => {
                    tracing::error!(
                        %operation_id,
                        %workspace_id,
                        %library_id,
                        error = ?error,
                        "library deletion worker failed"
                    );
                    record_rest_audit_event(
                        &state,
                        principal_id,
                        RestAuditEvent {
                            request_id,
                            action_kind: "catalog.library.delete".to_string(),
                            result_kind: "failed".to_string(),
                            redacted_message: Some(format!(
                                "library {display_name} deletion failed"
                            )),
                            internal_message: Some(format!(
                                "principal {principal_id} failed to delete library {library_id} in workspace {workspace_id} via async operation {operation_id}: {error:?}"
                            )),
                            subjects: vec![AppendAuditEventSubjectCommand {
                                subject_kind: "library".to_string(),
                                subject_id: library_id,
                                workspace_id: Some(workspace_id),
                                library_id: Some(library_id),
                                document_id: None,
                            }],
                        },
                    )
                    .await;
                }
            }
        }
        .instrument(tracing::info_span!(
            "catalog.library_delete.worker",
            %operation_id,
            %workspace_id,
            %library_id
        )),
    );
}

pub(super) fn map_deletion_admission(
    admission: CatalogDeletionAdmission,
) -> CatalogDeletionAcceptedResponse {
    CatalogDeletionAcceptedResponse {
        operation_id: admission.operation_id,
        workspace_id: admission.workspace_id,
        library_id: admission.library_id,
    }
}
