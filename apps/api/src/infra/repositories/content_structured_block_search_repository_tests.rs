use super::*;

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn structured_block_search_uses_indexed_language_neutral_fts() {
    assert!(
        STRUCTURED_BLOCK_SEARCH_SQL
            .contains("plainto_tsquery('simple'::regconfig, ironrag_unaccent($2))")
    );
    assert!(STRUCTURED_BLOCK_SEARCH_SQL.contains("block.search_tsv @@ search_query.value"));
    assert!(!STRUCTURED_BLOCK_SEARCH_SQL.contains("to_tsvector("));
    assert!(!STRUCTURED_BLOCK_SEARCH_SQL.contains("websearch_to_tsquery"));

    let baseline = include_str!("../../../migrations/0001_init.sql");
    assert!(baseline.contains(
        "CREATE INDEX knowledge_structured_block_search_tsv_gin ON public.knowledge_structured_block USING gin (search_tsv) WITH (fastupdate=off);"
    ));
}

#[test]
fn structured_block_search_scopes_exact_readable_identity_before_limit() {
    for identity_filter in [
        "document.document_id = block.document_id",
        "document.workspace_id = block.workspace_id",
        "document.library_id = block.library_id",
        "current_revision.revision_id = block.revision_id",
        "document.document_state = 'active'",
        "document.deleted_at is null",
        "structured_revision.revision_id = block.revision_id",
        "structured_revision.document_id = block.document_id",
        "structured_revision.workspace_id = block.workspace_id",
        "structured_revision.library_id = block.library_id",
        "structured_revision.preparation_state = 'prepared'",
        "block.library_id = $1",
    ] {
        let scope_offset = STRUCTURED_BLOCK_SEARCH_SQL.find(identity_filter).unwrap_or(usize::MAX);
        let limit_offset = STRUCTURED_BLOCK_SEARCH_SQL.rfind("limit $3").unwrap_or(0);
        assert!(
            scope_offset < limit_offset,
            "{identity_filter} must scope candidates before the top-k bound",
        );
    }
}

#[test]
fn structured_block_search_limits_documents_before_hydrating_original_text() {
    let dedupe_offset =
        STRUCTURED_BLOCK_SEARCH_SQL.find("select distinct on (document_id)").unwrap_or(usize::MAX);
    let limit_offset = STRUCTURED_BLOCK_SEARCH_SQL.find("limit $3").unwrap_or(0);
    let source_text_offset =
        STRUCTURED_BLOCK_SEARCH_SQL.find("block.text as source_text").unwrap_or(0);

    assert!(
        STRUCTURED_BLOCK_SEARCH_SQL
            .contains("order by document_id asc, text_score desc, ordinal asc, block_id asc")
    );
    assert!(
        STRUCTURED_BLOCK_SEARCH_SQL
            .contains("order by text_score desc, document_id asc\n      limit $3")
    );
    assert!(dedupe_offset < limit_offset);
    assert!(limit_offset < source_text_offset);
    assert_eq!(STRUCTURED_BLOCK_SEARCH_SQL.matches("block.text").count(), 1);
    assert!(!STRUCTURED_BLOCK_SEARCH_SQL.contains("ts_headline("));
    assert!(!STRUCTURED_BLOCK_SEARCH_SQL.contains("ironrag_unaccent(block.text)"));
}

#[test]
fn structured_block_lane_hands_off_only_to_a_ready_chunk() {
    let normalized_sql = normalize_sql(STRUCTURED_BLOCK_SEARCH_SQL);
    assert!(
        normalized_sql.contains(
            "not exists ( select 1 from knowledge_chunk as chunk where \
             chunk.revision_id = block.revision_id and \
             chunk.document_id = block.document_id and \
             chunk.workspace_id = block.workspace_id and \
             chunk.library_id = block.library_id and \
             chunk.chunk_state = 'ready' )"
        ),
        "the structured-block lane must hand the exact readable revision off only after a ready chunk exists",
    );

    let baseline = include_str!("../../../migrations/0001_init.sql");
    assert!(baseline.contains(
        "CREATE INDEX knowledge_chunk_revision_index ON public.knowledge_chunk USING btree (revision_id, chunk_index, chunk_id);"
    ));
}

#[test]
fn structured_block_search_excludes_preparing_artifacts_before_top_k() {
    let prepared_offset = STRUCTURED_BLOCK_SEARCH_SQL
        .find("structured_revision.preparation_state = 'prepared'")
        .unwrap_or(usize::MAX);
    let limit_offset = STRUCTURED_BLOCK_SEARCH_SQL.rfind("limit $3").unwrap_or(0);

    assert!(STRUCTURED_BLOCK_SEARCH_SQL.contains("join knowledge_structured_revision"));
    assert!(prepared_offset < limit_offset);
    assert!(!STRUCTURED_BLOCK_SEARCH_SQL.contains("preparation_state = 'preparing'"));
}
