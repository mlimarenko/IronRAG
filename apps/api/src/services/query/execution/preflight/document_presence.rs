use std::collections::HashMap;

use anyhow::Context as _;
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::query_ir::QueryLanguage,
    infra::knowledge_rows::KnowledgeDocumentRow, services::query::i18n::document_presence_messages,
};

use super::super::document_target::{
    explicit_document_reference_literal_is_present, explicit_document_reference_literals,
};
use super::{AnswerKind, DeterministicAnswerOverride};

/// The three stored identity fields every document contributes to matching.
/// One derivation keeps the explicit-reference presence check and
/// `scope_resolution` from disagreeing about what identifies a document.
pub(crate) fn document_identity_values(document: &KnowledgeDocumentRow) -> [Option<&str>; 3] {
    [document.file_name.as_deref(), document.title.as_deref(), Some(document.external_key.as_str())]
}

/// Whether a library row the searchable index omitted could be searched.
///
/// The searchable verdict is delegated to `document_index.contains_key`, not
/// re-derived from lifecycle columns: the index additionally requires a
/// `prepared` structured revision, so an active row with a readable revision
/// can still be absent from it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LibraryPresence {
    Searchable,
    NotSearchable { document_state: String, has_readable_revision: bool },
    Absent,
}

/// Resolve the explicit-document statement once, after the searchable index is
/// loaded. The store is read only when a file-like literal is absent from that
/// index, so the common case stays a pure in-memory check.
pub(super) async fn resolve_explicit_document_presence_override(
    state: &AppState,
    workspace_id: Uuid,
    library_id: Uuid,
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    language: QueryLanguage,
) -> anyhow::Result<Option<DeterministicAnswerOverride>> {
    if !absent_explicit_document_literal_exists(question, document_index) {
        return Ok(None);
    }
    let library_documents = state
        .document_store
        .list_documents_by_library(workspace_id, library_id, false)
        .await
        .context("failed to load library documents for explicit reference presence")?;
    Ok(classify_document_presence_override(question, document_index, &library_documents, language))
}

/// Pure classifier driven by unit tests: no store, no `AppState`.
///
/// The first literal missing from the searchable index decides the statement.
/// A store row the index contains leaves the grounded answer untouched; a store
/// row the index omitted is reported as not searchable; a literal the library
/// does not know stays absent.
pub(crate) fn classify_document_presence_override(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    library_documents: &[KnowledgeDocumentRow],
    language: QueryLanguage,
) -> Option<DeterministicAnswerOverride> {
    let literals = explicit_document_reference_literals(question);
    let missing = first_missing_explicit_document_literal(&literals, document_index)?;
    let presence = classify_library_presence(missing, document_index, library_documents);
    presence_message(language, missing, presence).map(|answer| DeterministicAnswerOverride {
        answer,
        answer_kind: AnswerKind::MissingExplicitDocument,
    })
}

/// Whether resolving this question needs the library store at all.
///
/// True only when the question names an explicit document that the searchable
/// index does not already contain. An empty literal set and a literal set the
/// index fully covers both stay in memory. Pure so the resolver's store-read
/// gate has a direct test.
pub(crate) fn absent_explicit_document_literal_exists(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> bool {
    let literals = explicit_document_reference_literals(question);
    first_missing_explicit_document_literal(&literals, document_index).is_some()
}

fn first_missing_explicit_document_literal<'a>(
    literals: &'a [String],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<&'a str> {
    let searchable_identity_values =
        document_index.values().flat_map(document_identity_values).flatten().collect::<Vec<_>>();
    literals.iter().map(String::as_str).find(|literal| {
        !explicit_document_reference_literal_is_present(
            literal,
            searchable_identity_values.iter().copied(),
        )
    })
}

fn classify_library_presence(
    literal: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    library_documents: &[KnowledgeDocumentRow],
) -> LibraryPresence {
    let mut not_searchable = None;
    for document in library_documents {
        if !explicit_document_reference_literal_is_present(
            literal,
            document_identity_values(document).into_iter().flatten(),
        ) {
            continue;
        }
        if document_index.contains_key(&document.document_id) {
            return LibraryPresence::Searchable;
        }
        not_searchable.get_or_insert_with(|| LibraryPresence::NotSearchable {
            document_state: document.document_state.clone(),
            has_readable_revision: document.readable_revision_id.is_some(),
        });
    }
    not_searchable.unwrap_or(LibraryPresence::Absent)
}

fn presence_message(
    language: QueryLanguage,
    literal: &str,
    presence: LibraryPresence,
) -> Option<String> {
    let messages = document_presence_messages(language);
    let template = match presence {
        LibraryPresence::Searchable => return None,
        LibraryPresence::Absent => messages.absent,
        LibraryPresence::NotSearchable { document_state, has_readable_revision } => {
            match (document_state == "active", has_readable_revision) {
                (false, false) => messages.not_active_and_not_readable,
                (false, true) => messages.not_active,
                (true, false) => messages.not_readable_revision,
                (true, true) => messages.not_prepared,
            }
        }
    };
    Some(render_presence_message(template, literal))
}

fn render_presence_message(template: &str, literal: &str) -> String {
    template.replace("{document}", literal)
}

#[cfg(test)]
#[path = "document_presence_tests.rs"]
mod tests;
