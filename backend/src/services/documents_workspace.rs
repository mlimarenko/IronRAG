use crate::domains::ui_documents::{
    DocumentCollectionWarning, DocumentsWorkspaceDiagnosticChip, DocumentsWorkspaceModel,
    DocumentsWorkspaceNotice, DocumentsWorkspacePrimarySummary,
};

#[derive(Debug, Clone, Default)]
pub struct DocumentsWorkspaceService;

impl DocumentsWorkspaceService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    #[must_use]
    pub fn split_notices(
        &self,
        warnings: &[DocumentCollectionWarning],
    ) -> (Vec<DocumentsWorkspaceNotice>, Vec<DocumentsWorkspaceNotice>) {
        let mut degraded = Vec::new();
        let mut informational = Vec::new();
        for warning in warnings {
            let notice = DocumentsWorkspaceNotice {
                kind: warning.warning_kind.clone(),
                title: warning.warning_kind.replace('_', " "),
                message: warning.warning_message.clone(),
            };
            if warning.is_degraded {
                degraded.push(notice);
            } else {
                informational.push(notice);
            }
        }
        (degraded, informational)
    }

    #[must_use]
    pub fn diagnostic_chip(
        &self,
        kind: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> DocumentsWorkspaceDiagnosticChip {
        DocumentsWorkspaceDiagnosticChip {
            kind: kind.into(),
            label: label.into(),
            value: value.into(),
        }
    }

    #[must_use]
    pub fn build(
        &self,
        primary_summary: DocumentsWorkspacePrimarySummary,
        secondary_diagnostics: Vec<DocumentsWorkspaceDiagnosticChip>,
        warnings: &[DocumentCollectionWarning],
        table_document_count: usize,
        active_filter_count: usize,
        highlighted_status: Option<String>,
    ) -> DocumentsWorkspaceModel {
        let (degraded_notices, informational_notices) = self.split_notices(warnings);
        DocumentsWorkspaceModel {
            primary_summary,
            secondary_diagnostics,
            degraded_notices,
            informational_notices,
            table_document_count,
            active_filter_count,
            highlighted_status,
        }
    }
}
