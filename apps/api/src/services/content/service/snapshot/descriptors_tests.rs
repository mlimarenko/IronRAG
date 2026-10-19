use super::test_support::*;
use super::*;

#[test]
fn per_dim_vector_collection_name_parser_round_trips_chunk_and_entity_shards() {
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024"), Some(1024));
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_entity_vector_d3072"), Some(3072));
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1"), Some(1));
    assert!(is_per_dim_chunk_vector_collection_name("knowledge_chunk_vector_d1024"));
    assert!(!is_per_dim_chunk_vector_collection_name("knowledge_entity_vector_d1024"));
    assert!(is_runtime_vector_relation_name("knowledge_chunk_vector_d3072"));
    // Negative cases — legacy names, missing digits, alpha/library suffixes,
    // and wrong prefixes.
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector"), None);
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d"), None);
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024x"), None);
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024_l"), None);
    assert_eq!(
        parse_per_dim_vector_collection_dim(
            "knowledge_chunk_vector_d3072_l019ded0008207ad29224ca3d0c82d57c"
        ),
        None
    );
    assert_eq!(parse_per_dim_vector_collection_dim("knowledge_chunk_vector_d1024_lABC"), None);
    assert_eq!(parse_per_dim_vector_collection_dim("other_collection_d1024"), None);
}

#[test]
fn snapshot_manifest_sections_accept_per_dim_vector_shards() {
    let mut manifest =
        manifest_with_sections(vec!["catalog_library", "knowledge_chunk_vector_d1024"], false);
    manifest.vector_shards =
        vec![VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 1024 }];
    let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
    assert_eq!(
        sections.require_postgres_table("knowledge_chunk_vector_d1024").unwrap(),
        "knowledge_chunk_vector_d1024"
    );
}

#[test]
fn snapshot_manifest_rejects_missing_vector_shard_declaration() {
    let manifest = manifest_with_sections(
        vec!["catalog_library", "knowledge_chunk_vector_d1024", "knowledge_entity_vector_d3072"],
        false,
    );
    let error = SnapshotManifestSections::from_manifest(&manifest).unwrap_err();
    assert!(error.to_string().contains("must exactly match"));
}

#[test]
fn snapshot_manifest_requires_exact_vector_shard_declarations() {
    let missing =
        manifest_with_sections(vec!["catalog_library", "knowledge_chunk_vector_d1024"], false);
    let missing_error = SnapshotManifestSections::from_manifest(&missing).unwrap_err();
    assert!(missing_error.to_string().contains("must exactly match"));

    let mut extra = manifest_with_sections(vec!["catalog_library"], false);
    extra.vector_shards =
        vec![VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 1024 }];
    let extra_error = SnapshotManifestSections::from_manifest(&extra).unwrap_err();
    assert!(extra_error.to_string().contains("has no matching postgres table"));
}

#[test]
fn snapshot_manifest_rejects_vector_shard_dimension_or_identity_mismatch() {
    let mut wrong_dimension =
        manifest_with_sections(vec!["catalog_library", "knowledge_chunk_vector_d1024"], false);
    wrong_dimension.vector_shards =
        vec![VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 3072 }];
    let wrong_dimension_error =
        SnapshotManifestSections::from_manifest(&wrong_dimension).unwrap_err();
    assert!(wrong_dimension_error.to_string().contains("declares dimension 3072"));

    let mut duplicate =
        manifest_with_sections(vec!["catalog_library", "knowledge_chunk_vector_d1024"], false);
    duplicate.vector_shards = vec![
        VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 1024 },
        VectorShardEntry { name: "knowledge_chunk_vector_d1024".to_string(), dim: 1024 },
    ];
    let duplicate_error = SnapshotManifestSections::from_manifest(&duplicate).unwrap_err();
    assert!(duplicate_error.to_string().contains("more than once"));

    assert!(!is_expected_per_dim_vector_collection_name("knowledge_chunk_vector_d01024"));

    let mut zero_dimension =
        manifest_with_sections(vec!["catalog_library", "knowledge_chunk_vector_d0"], false);
    zero_dimension.vector_shards =
        vec![VectorShardEntry { name: "knowledge_chunk_vector_d0".to_string(), dim: 0 }];
    let zero_dimension_error =
        SnapshotManifestSections::from_manifest(&zero_dimension).unwrap_err();
    assert!(zero_dimension_error.to_string().contains("invalid dimension"));
}

#[test]
fn ai_config_snapshot_orders_model_purposes_before_bindings() {
    assert_eq!(
        POSTGRES_AI_CONFIG_TABLES,
        [
            "ai_provider_catalog",
            "ai_model_catalog",
            "ai_model_binding_purpose",
            "ai_account",
            "ai_binding",
        ]
    );
}

#[test]
fn snapshot_manifest_sections_accept_declared_table_names() {
    let manifest = manifest_with_sections(
        vec![
            "catalog_library",
            "content_document",
            "runtime_graph_node",
            "ai_provider_catalog",
            "ai_model_catalog",
            "ai_model_binding_purpose",
            "ai_account",
            "ai_binding",
        ],
        true,
    );

    let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();

    assert_eq!(sections.require_postgres_table("catalog_library").unwrap(), "catalog_library");
    assert_eq!(sections.require_postgres_table("content_document").unwrap(), "content_document");
    assert_eq!(
        sections.require_postgres_table("runtime_graph_node").unwrap(),
        "runtime_graph_node"
    );
    for table in POSTGRES_AI_CONFIG_TABLES {
        assert_eq!(sections.require_postgres_table(table).unwrap(), *table);
    }
}

#[test]
fn snapshot_manifest_sections_reject_unknown_or_undeclared_names() {
    let manifest = manifest_with_sections(vec!["catalog_library", "pg_catalog_authid"], false);
    assert!(SnapshotManifestSections::from_manifest(&manifest).is_err());

    let manifest = manifest_with_sections(vec!["catalog_library"], false);
    let sections = SnapshotManifestSections::from_manifest(&manifest).unwrap();
    assert!(sections.require_postgres_table("content_document").is_err());
    assert!(sections.require_postgres_table("ai_provider_credential").is_err());
}

#[test]
fn snapshot_manifest_sections_reject_non_library_config_table_names() {
    for table in ["ai_provider_credential", "ai_model_preset", "ai_binding_assignment"] {
        let manifest = manifest_with_sections(vec!["catalog_library", table], false);
        assert!(
            SnapshotManifestSections::from_manifest(&manifest).is_err(),
            "non-library snapshot table `{table}` must be rejected"
        );
    }
}

#[test]
fn snapshot_section_path_requires_declared_part_files() {
    assert_eq!(
        split_section_path("content_document/part-000001.ndjson").unwrap(),
        ("content_document", "part-000001.ndjson")
    );
    assert!(split_section_path("content_document/raw.json").is_err());
    assert!(split_section_path("content_document/part-000001.ndjson/extra").is_err());
}

#[test]
fn snapshot_manifest_rejects_inconsistent_blob_declaration() {
    let mut manifest = manifest_with_sections(vec!["catalog_library"], true);
    manifest.include_kinds = vec![IncludeKind::LibraryData];

    assert!(SnapshotManifestSections::from_manifest(&manifest).is_err());
}

#[test]
fn catalog_library_import_does_not_carry_parallel_update_column_list() {
    assert_eq!(pg_insert_conflict_clause("catalog_workspace"), " ON CONFLICT DO NOTHING");
    assert_eq!(pg_insert_conflict_clause("catalog_library"), "");
}

#[test]
fn snapshot_row_scope_rewrites_existing_target_identity_and_blob_prefix() {
    let source_workspace_id = Uuid::now_v7();
    let source_library_id = Uuid::now_v7();
    let target_workspace_id = Uuid::now_v7();
    let target_library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mutation_id = Uuid::now_v7();
    let source_storage_key =
        format!("content/{source_workspace_id}/{source_library_id}/source.bin");
    let target_storage_key =
        format!("content/{target_workspace_id}/{target_library_id}/source.bin");
    let mut scope = SnapshotRowScope::new(
        source_library_id,
        target_library_id,
        target_workspace_id,
        Some("alpha-restored".to_string()),
    );

    let mut library = serde_json::json!({
        "id": source_library_id,
        "workspace_id": source_workspace_id,
        "slug": "alpha",
        "display_name": "Alpha",
    });
    scope.normalize_postgres_row("catalog_library", &mut library).unwrap();
    assert_eq!(required_uuid_field("catalog_library", &library, "id").unwrap(), target_library_id);
    assert_eq!(
        required_uuid_field("catalog_library", &library, "workspace_id").unwrap(),
        target_workspace_id
    );
    // The archive's slug `alpha` is rewritten to the target library's
    // own slug so a restore cannot collide with a sibling library.
    assert_eq!(string_field(&library, "slug"), Some("alpha-restored"));

    let mut document = serde_json::json!({
        "id": document_id,
        "library_id": source_library_id,
        "workspace_id": source_workspace_id,
    });
    scope.normalize_postgres_row("content_document", &mut document).unwrap();
    assert_eq!(
        required_uuid_field("content_document", &document, "library_id").unwrap(),
        target_library_id
    );
    assert_eq!(
        required_uuid_field("content_document", &document, "workspace_id").unwrap(),
        target_workspace_id
    );

    let mut revision = serde_json::json!({
        "id": revision_id,
        "document_id": document_id,
        "library_id": source_library_id,
        "workspace_id": source_workspace_id,
        "storage_key": source_storage_key,
    });
    scope.normalize_postgres_row("content_revision", &mut revision).unwrap();
    assert_eq!(string_field(&revision, "storage_key"), Some(target_storage_key.as_str()));
    assert_eq!(scope.normalize_blob_key(&source_storage_key).unwrap(), target_storage_key);
    let mut knowledge = serde_json::json!({"revision_id": revision_id, "document_id": document_id,
        "library_id": source_library_id, "workspace_id": source_workspace_id, "storage_ref": source_storage_key});
    scope.normalize_postgres_row("knowledge_revision", &mut knowledge).unwrap();
    assert_eq!(string_field(&knowledge, "storage_ref"), Some(target_storage_key.as_str()));
    knowledge["document_id"] = serde_json::json!(Uuid::now_v7());
    knowledge["library_id"] = serde_json::json!(source_library_id);
    knowledge["workspace_id"] = serde_json::json!(source_workspace_id);
    assert!(scope.normalize_postgres_row("knowledge_revision", &mut knowledge).is_err());

    let mut mutation = serde_json::json!({
        "id": mutation_id,
        "library_id": source_library_id,
        "workspace_id": source_workspace_id,
    });
    let mut reference = mutation.clone();
    let original_actor = Uuid::now_v7();
    reference["reservation_principal_id"] = serde_json::json!(original_actor);
    scope.normalize_postgres_row("content_mutation_reference", &mut reference).unwrap();
    assert_eq!(
        required_uuid_field("registry", &reference, "reservation_principal_id").unwrap(),
        original_actor
    );
    scope.normalize_postgres_row("content_mutation", &mut mutation).unwrap();

    let mut head = serde_json::json!({
        "document_id": document_id,
        "active_revision_id": revision_id,
        "readable_revision_id": revision_id,
        "latest_mutation_id": mutation_id,
    });
    scope.normalize_postgres_row("content_document_head", &mut head).unwrap();
}

#[test]
fn snapshot_row_scope_normalizes_ai_config_rows() {
    let source_workspace_id = Uuid::now_v7();
    let source_library_id = Uuid::now_v7();
    let target_workspace_id = Uuid::now_v7();
    let target_library_id = Uuid::now_v7();
    let mut scope =
        SnapshotRowScope::new(source_library_id, target_library_id, target_workspace_id, None);

    // System provider catalog: stable id, no scope columns — untouched.
    let provider_id = Uuid::now_v7();
    let mut provider = serde_json::json!({
        "id": provider_id,
        "provider_kind": "openai",
        "display_name": "OpenAI",
    });
    scope.normalize_postgres_row("ai_provider_catalog", &mut provider).unwrap();
    assert_eq!(required_uuid_field("ai_provider_catalog", &provider, "id").unwrap(), provider_id);

    // Workspace-scoped account: workspace_id remapped, library_id stays
    // null, principal author reference dropped.
    let mut account = serde_json::json!({
        "id": Uuid::now_v7(),
        "workspace_id": source_workspace_id,
        "library_id": serde_json::Value::Null,
        "scope_kind": "workspace",
        "api_key": serde_json::Value::Null,
        "created_by_principal_id": Uuid::now_v7(),
    });
    scope.normalize_postgres_row("ai_account", &mut account).unwrap();
    assert_eq!(
        required_uuid_field("ai_account", &account, "workspace_id").unwrap(),
        target_workspace_id
    );
    assert!(account.get("library_id").unwrap().is_null());
    assert!(account.get("created_by_principal_id").unwrap().is_null());

    // Library-scoped binding: both scope ids remapped and principal author
    // references dropped.
    let mut binding = serde_json::json!({
        "id": Uuid::now_v7(),
        "workspace_id": source_workspace_id,
        "library_id": source_library_id,
        "scope_kind": "library",
        "updated_by_principal_id": Uuid::now_v7(),
    });
    scope.normalize_postgres_row("ai_binding", &mut binding).unwrap();
    assert_eq!(
        required_uuid_field("ai_binding", &binding, "workspace_id").unwrap(),
        target_workspace_id
    );
    assert_eq!(
        required_uuid_field("ai_binding", &binding, "library_id").unwrap(),
        target_library_id
    );
    assert!(binding.get("updated_by_principal_id").unwrap().is_null());

    // A library-scoped binding referencing a different library is
    // rejected.
    let mut foreign = serde_json::json!({
        "id": Uuid::now_v7(),
        "workspace_id": source_workspace_id,
        "library_id": Uuid::now_v7(),
        "scope_kind": "library",
    });
    assert!(scope.normalize_postgres_row("ai_binding", &mut foreign).is_err());
}
