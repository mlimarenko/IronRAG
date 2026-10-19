use super::LIST_PREPARED_CHUNKS_BY_REVISION_SQL;

fn normalize_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[test]
fn prepared_content_chunk_read_keeps_readiness_and_rows_in_one_statement() {
    let normalized = normalize_sql(LIST_PREPARED_CHUNKS_BY_REVISION_SQL);

    assert!(!normalized.contains(';'), "the prepared chunk read must remain one statement");
    assert!(
        normalized.contains(
            "from content_chunk as chunk where chunk.revision_id = $1 and exists ( select 1 from knowledge_structured_revision as structured where structured.revision_id = chunk.revision_id and structured.preparation_state = 'prepared' ) order by chunk.chunk_index asc",
        ),
        "the exact prepared-state predicate must be evaluated in the same snapshot as chunk rows",
    );
}
