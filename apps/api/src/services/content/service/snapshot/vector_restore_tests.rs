use super::*;

#[test]
fn raw_restore_logical_indexes_match_the_runtime_identity_contract() {
    assert_eq!(PG_VECTOR_RESTORE_KEY_REPLAY_CONFLICT, "ON CONFLICT (key) DO NOTHING");

    let chunk_sql = pg_vector_logical_identity_index_sql(
        "knowledge_chunk_vector_d1024",
        "chunk_id",
        Some("revision_id"),
    )
    .unwrap();
    assert_eq!(
        chunk_sql,
        "create unique index if not exists \"knowledge_chunk_vector_d1024_logical_key\" on \"knowledge_chunk_vector_d1024\" (library_id, \"chunk_id\", \"revision_id\", embedding_model_key, vector_kind, freshness_generation)"
    );

    let entity_sql =
        pg_vector_logical_identity_index_sql("knowledge_entity_vector_d3072", "entity_id", None)
            .unwrap();
    assert_eq!(
        entity_sql,
        "create unique index if not exists \"knowledge_entity_vector_d3072_logical_key\" on \"knowledge_entity_vector_d3072\" (library_id, \"entity_id\", embedding_model_key, vector_kind, freshness_generation)"
    );
}

#[test]
fn logical_vector_duplicates_are_not_retryable_restore_contention() {
    assert!(!pg_restore_sqlstate_is_retryable(
        "23505",
        Some("knowledge_chunk_vector_d1024_logical_key"),
    ));
    assert!(pg_restore_sqlstate_is_retryable("23505", Some("pg_class_relname_nsp_index")));
    assert!(pg_restore_sqlstate_is_retryable(PG_SQLSTATE_DEADLOCK_DETECTED, None));
}
