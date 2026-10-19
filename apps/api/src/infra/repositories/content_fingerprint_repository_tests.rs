use super::LIBRARY_READABLE_STATUS_SQL;

/// Fragments the readable-status query must carry: it reads the maintained
/// projection version and gates chunks through the prepared structured
/// revision of the document's readable head.
const READABLE_STATUS_REQUIRED_FRAGMENTS: &[&str] = &[
    "readable_projection_version",
    "join knowledge_structured_revision as structured",
    "structured.revision_id = chunk.revision_id",
    "structured.workspace_id = chunk.workspace_id",
    "structured.library_id = chunk.library_id",
    "structured.document_id = chunk.document_id",
    "structured.preparation_state = 'prepared'",
    "chunk.document_id = document.document_id",
    "chunk.workspace_id = document.workspace_id",
    "chunk.library_id = document.library_id",
    "chunk.revision_id = current_revision.revision_id",
    "join content_current_revision current_revision",
];

/// Projection-scan shapes the maintained-version query must not regress to.
const READABLE_STATUS_FORBIDDEN_FRAGMENTS: &[&str] = &[
    "content_document",
    "content_revision",
    "full join",
    "string_agg",
    "preparation_state = 'preparing'",
];

fn assert_readable_status_query_shape(sql: &str) {
    for fragment in READABLE_STATUS_REQUIRED_FRAGMENTS {
        assert!(sql.contains(*fragment), "readable-status query is missing {fragment:?}");
    }
    for fragment in READABLE_STATUS_FORBIDDEN_FRAGMENTS {
        assert!(!sql.contains(*fragment), "readable-status query must not contain {fragment:?}");
    }
}

#[test]
fn readable_status_query_uses_the_maintained_version_without_projection_scans() {
    assert_readable_status_query_shape(LIBRARY_READABLE_STATUS_SQL);
}
