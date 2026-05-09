use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    infra::arangodb::document_store::{KnowledgeRevisionRow, KnowledgeStructuredRevisionRow},
    infra::repositories::ingest_repository,
};

#[derive(Debug, Clone)]
pub(super) struct InlineMutationContext {
    pub(super) mutation_id: Uuid,
    pub(super) job_id: Uuid,
    pub(super) item_id: Uuid,
    pub(super) workspace_id: Uuid,
    pub(super) library_id: Uuid,
    pub(super) document_id: Uuid,
    pub(super) revision_id: Uuid,
}

#[derive(Debug, Clone)]
pub(super) struct EditableDocumentContext {
    pub(super) title: Option<String>,
    pub(super) language_code: Option<String>,
}

pub(super) struct PrefetchedDocumentSummaryData {
    pub(super) revisions_by_id: HashMap<Uuid, KnowledgeRevisionRow>,
    pub(super) structured_revisions_by_revision_id: HashMap<Uuid, KnowledgeStructuredRevisionRow>,
    pub(super) web_pages_by_result_revision_id:
        HashMap<Uuid, ingest_repository::WebDiscoveredPageRow>,
}
