use super::test_support::*;
use super::workspace::*;
use super::*;

#[test]
fn workspace_manifest_serde_round_trips() {
    let workspace_id = Uuid::now_v7();
    let lib_a = Uuid::now_v7();
    let lib_b = Uuid::now_v7();
    let manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id,
        workspace_slug: "alpha-suite".to_string(),
        exported_at: chrono::Utc::now(),
        source_version: "0.0.0-test".to_string(),
        include_kinds: vec![IncludeKind::LibraryData, IncludeKind::Blobs],
        libraries: vec![
            WorkspaceManifestLibrary {
                id: lib_a,
                slug: "provider-beta".to_string(),
                display_name: "Provider Beta".to_string(),
            },
            WorkspaceManifestLibrary {
                id: lib_b,
                slug: "provider-gamma".to_string(),
                display_name: "Provider Gamma".to_string(),
            },
        ],
    };

    let bytes = serde_json::to_vec(&manifest).expect("serialize workspace manifest");
    let parsed: WorkspaceSnapshotManifest =
        serde_json::from_slice(&bytes).expect("deserialize workspace manifest");

    assert_eq!(parsed.schema_version, SNAPSHOT_SCHEMA_VERSION);
    assert_eq!(parsed.workspace_id, workspace_id);
    assert_eq!(parsed.workspace_slug, "alpha-suite");
    assert_eq!(parsed.include_kinds, vec![IncludeKind::LibraryData, IncludeKind::Blobs]);
    assert_eq!(parsed.libraries.len(), 2);
    assert_eq!(parsed.libraries[0].id, lib_a);
    assert_eq!(parsed.libraries[1].slug, "provider-gamma");
}

#[test]
fn workspace_inventory_requires_exact_unique_declared_library_archives() {
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    let manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id: Uuid::now_v7(),
        workspace_slug: "alpha-suite".to_string(),
        exported_at: chrono::Utc::now(),
        source_version: "0.0.0-test".to_string(),
        include_kinds: vec![IncludeKind::LibraryData],
        libraries: vec![
            WorkspaceManifestLibrary {
                id: first,
                slug: "provider-beta".to_string(),
                display_name: "Provider Beta".to_string(),
            },
            WorkspaceManifestLibrary {
                id: second,
                slug: "provider-gamma".to_string(),
                display_name: "Provider Gamma".to_string(),
            },
        ],
    };
    let first_path = format!("libraries/{first}.tar.zst");
    let second_path = format!("libraries/{second}.tar.zst");
    let inventory = WorkspaceSnapshotInventory {
        libraries: vec![
            snapshot_entry_digest(first_path.clone(), b"first"),
            snapshot_entry_digest(second_path.clone(), b"second"),
        ],
    };

    assert!(validate_workspace_snapshot_inventory(&manifest, &inventory, &inventory).is_ok());

    let duplicate = WorkspaceSnapshotInventory {
        libraries: vec![
            snapshot_entry_digest(first_path, b"first"),
            snapshot_entry_digest(second_path, b"second"),
            snapshot_entry_digest(format!("libraries/{first}.tar.zst"), b"first"),
        ],
    };
    assert!(validate_workspace_snapshot_inventory(&manifest, &duplicate, &duplicate).is_err());
}

#[tokio::test]
async fn workspace_prevalidation_rejects_corrupt_second_archive_before_provisioning() {
    let workspace_id = Uuid::now_v7();
    let first = Uuid::now_v7();
    let second = Uuid::now_v7();
    let workspace_manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id,
        workspace_slug: "alpha-suite".to_string(),
        exported_at: chrono::Utc::now(),
        source_version: "0.0.0-test".to_string(),
        include_kinds: vec![IncludeKind::LibraryData],
        libraries: [first, second]
            .into_iter()
            .map(|id| WorkspaceManifestLibrary {
                id,
                slug: format!("library-{id}"),
                display_name: "Synthetic Library".to_string(),
            })
            .collect(),
    };
    let library_manifest = SnapshotManifest {
        library_id: first,
        ..manifest_with_sections(
            POSTGRES_CONTENT_TABLES.iter().copied().chain(["catalog_library"]).collect(),
            false,
        )
    };
    let part_path = "postgres/catalog_library/part-000001.ndjson";
    let part = br#"{"id":"00000000-0000-0000-0000-000000000000"}
"#;
    let summary = SnapshotSummary {
        inventory: SnapshotInventory {
            postgres_row_counts: library_manifest
                .postgres_tables
                .iter()
                .map(|table| (table.clone(), u64::from(table == "catalog_library")))
                .collect(),
            postgres_parts: vec![snapshot_entry_digest(part_path.to_string(), part)],
            blobs: Vec::new(),
        },
    };
    let mut first_archive = Vec::new();
    let zstd = ZstdEncoder::new(&mut first_archive);
    let mut inner = Builder::new(zstd);
    append_json_entry(&mut inner, "manifest.json", &library_manifest).await.unwrap();
    append_raw_entry(&mut inner, part_path, part).await.unwrap();
    append_json_entry(&mut inner, "summary.json", &summary).await.unwrap();
    let mut zstd = inner.into_inner().await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.unwrap();

    let first_path = format!("libraries/{first}.tar.zst");
    let second_path = format!("libraries/{second}.tar.zst");
    let corrupt_second = b"not-a-zstd-archive";
    let outer_summary = WorkspaceSnapshotSummary {
        inventory: WorkspaceSnapshotInventory {
            libraries: vec![
                snapshot_entry_digest(first_path.clone(), &first_archive),
                snapshot_entry_digest(second_path.clone(), corrupt_second),
            ],
        },
    };
    let mut outer_archive = Vec::new();
    let mut outer = Builder::new(&mut outer_archive);
    append_json_entry(&mut outer, "workspace-manifest.json", &workspace_manifest).await.unwrap();
    append_raw_entry(&mut outer, &first_path, &first_archive).await.unwrap();
    append_raw_entry(&mut outer, &second_path, corrupt_second).await.unwrap();
    append_json_entry(&mut outer, "workspace-summary.json", &outer_summary).await.unwrap();
    outer.into_inner().await.unwrap();

    let result = prevalidate_workspace_archive(outer_archive.as_slice()).await;
    assert!(result.is_err(), "corrupt second archive must prevent any provisioning phase");
}

/// IRG-32/IRG-23: an embedded library blob that contradicts its
/// `content_revision.checksum` is a refusal, not a crash. The workspace door
/// must classify it as an invalid archive — `snapshot_archive_invalid` — for
/// the same reason the library door does, instead of collapsing into
/// `snapshot_import_failed`.
#[tokio::test]
async fn workspace_prevalidation_classifies_an_embedded_blob_mismatch_as_an_invalid_archive() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let workspace_manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id,
        workspace_slug: "alpha-suite".to_string(),
        exported_at: chrono::Utc::now(),
        source_version: "0.0.0-test".to_string(),
        include_kinds: vec![IncludeKind::LibraryData, IncludeKind::Blobs],
        libraries: vec![WorkspaceManifestLibrary {
            id: library_id,
            slug: "embedded-library".to_string(),
            display_name: "Embedded Library".to_string(),
        }],
    };

    // Embedded library archive whose `content_revision` row proves the digest
    // of the honest bytes while the archive streams tampered bytes.
    let library_manifest =
        SnapshotManifest { library_id, ..manifest_with_sections(vec!["content_revision"], true) };
    let honest = b"the export service listens on port 8081";
    let tampered = b"the export service listens on port 8082";
    let digest = hex::encode(Sha256::digest(honest));
    let storage_key = format!("content/workspace/library/{digest}-alpha.bin");
    let row = serde_json::json!({
        "id": Uuid::now_v7().to_string(),
        "storage_key": storage_key,
        "checksum": format!("sha256:{digest}"),
    });
    let mut part = serde_json::to_vec(&row).unwrap();
    part.push(b'\n');
    let part_path = "postgres/content_revision/part-000001.ndjson";
    let blob_path = format!("blobs/{}", encode_blob_path(&storage_key));
    let library_summary = SnapshotSummary {
        inventory: SnapshotInventory {
            postgres_row_counts: BTreeMap::from([("content_revision".to_string(), 1)]),
            postgres_parts: vec![snapshot_entry_digest(part_path.to_string(), &part)],
            blobs: vec![snapshot_entry_digest(blob_path.clone(), tampered)],
        },
    };
    let mut library_archive = Vec::new();
    let zstd = ZstdEncoder::new(&mut library_archive);
    let mut inner = Builder::new(zstd);
    append_json_entry(&mut inner, "manifest.json", &library_manifest).await.unwrap();
    append_raw_entry(&mut inner, part_path, &part).await.unwrap();
    append_raw_entry(&mut inner, &blob_path, tampered).await.unwrap();
    append_json_entry(&mut inner, "summary.json", &library_summary).await.unwrap();
    let mut zstd = inner.into_inner().await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.unwrap();

    let library_path = format!("libraries/{library_id}.tar.zst");
    let outer_summary = WorkspaceSnapshotSummary {
        inventory: WorkspaceSnapshotInventory {
            libraries: vec![snapshot_entry_digest(library_path.clone(), &library_archive)],
        },
    };
    let mut outer_archive = Vec::new();
    let mut outer = Builder::new(&mut outer_archive);
    append_json_entry(&mut outer, "workspace-manifest.json", &workspace_manifest).await.unwrap();
    append_raw_entry(&mut outer, &library_path, &library_archive).await.unwrap();
    append_json_entry(&mut outer, "workspace-summary.json", &outer_summary).await.unwrap();
    outer.into_inner().await.unwrap();

    let error = prevalidate_workspace_archive(outer_archive.as_slice()).await.unwrap_err();
    assert_eq!(
        snapshot_import_failure_code(&error),
        "snapshot_archive_invalid",
        "an embedded blob that contradicts its checksum must classify as an invalid archive, not a crash: {error:#}"
    );
}

#[test]
fn workspace_library_entry_path_parses_and_rejects() {
    let id = Uuid::now_v7();
    assert_eq!(
        parse_workspace_library_entry_path(&format!("libraries/{id}.tar.zst")).unwrap(),
        Some(id)
    );
    assert_eq!(parse_workspace_library_entry_path("workspace-manifest.json").unwrap(), None);
    assert_eq!(parse_workspace_library_entry_path("libraries/nested/x.tar.zst").unwrap(), None);
    assert!(parse_workspace_library_entry_path("libraries/not-a-uuid.tar.zst").is_err());
}
