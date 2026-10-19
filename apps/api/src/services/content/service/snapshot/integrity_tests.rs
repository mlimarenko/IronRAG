use super::test_support::*;
use super::*;

fn complete_content_manifest(manifest: &SnapshotManifest) -> SnapshotManifest {
    let mut manifest = manifest.clone();
    for table in POSTGRES_CONTENT_TABLES {
        if !manifest.postgres_tables.iter().any(|name| name == table) {
            manifest.postgres_tables.push((*table).to_owned());
        }
    }
    manifest
}

fn empty_content_counts(manifest: &SnapshotManifest) -> BTreeMap<String, u64> {
    manifest.postgres_tables.iter().map(|table| (table.clone(), 0)).collect()
}

async fn spool_prevalidation_test_archive(
    manifest: &SnapshotManifest,
    part_path: &str,
    part: &[u8],
    postgres_rows: u64,
) -> SpoolSnapshotArchive {
    let manifest = complete_content_manifest(manifest);
    let mut counts = empty_content_counts(&manifest);
    counts.insert("catalog_library".to_owned(), postgres_rows);
    let summary = SnapshotSummary {
        inventory: SnapshotInventory {
            postgres_row_counts: counts,
            postgres_parts: vec![snapshot_entry_digest(part_path.to_string(), part)],
            blobs: Vec::new(),
        },
    };
    let mut archive = Vec::new();
    let zstd = ZstdEncoder::new(&mut archive);
    let mut builder = Builder::new(zstd);
    append_json_entry(&mut builder, "manifest.json", &manifest).await.unwrap();
    append_raw_entry(&mut builder, part_path, part).await.unwrap();
    append_json_entry(&mut builder, "summary.json", &summary).await.unwrap();
    let mut zstd = builder.into_inner().await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.unwrap();
    spool_snapshot_archive(archive.as_slice()).await.unwrap()
}

#[tokio::test]
async fn prevalidation_accepts_self_described_terminal_inventory() {
    let manifest = manifest_with_sections(vec!["catalog_library"], false);
    let part_path = "postgres/catalog_library/part-000001.ndjson";
    let part = br#"{"id":"00000000-0000-0000-0000-000000000000"}
"#;
    let spool = spool_prevalidation_test_archive(&manifest, part_path, part, 1).await;
    let (prevalidated, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(prevalidated.library_id, manifest.library_id);
    assert_eq!(verification, SnapshotBlobVerification::default());
}

#[tokio::test]
async fn prevalidation_rejects_oversized_decompressed_entry_before_payload_allocation() {
    let manifest = manifest_with_sections(vec!["catalog_library"], false);
    let part_path = "postgres/catalog_library/part-000001.ndjson";
    let part = vec![b' '; 2048];
    let spool = spool_prevalidation_test_archive(&manifest, part_path, &part, 0).await;
    let error =
        prevalidate_snapshot_archive_with_limits(&spool, 1024, 16 * 1024).await.unwrap_err();

    assert!(error.to_string().contains("maximum decompressed entry size"));
}

#[tokio::test]
async fn prevalidation_rejects_decompressed_total_over_cap() {
    let manifest = manifest_with_sections(vec!["catalog_library"], false);
    let part_path = "postgres/catalog_library/part-000001.ndjson";
    let part = br#"{"id":"00000000-0000-0000-0000-000000000000"}
"#;
    let spool = spool_prevalidation_test_archive(&manifest, part_path, part, 1).await;
    let manifest_size = serde_json::to_vec_pretty(&manifest).unwrap().len() as u64;
    let error = prevalidate_snapshot_archive_with_limits(&spool, 16 * 1024, manifest_size)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("maximum decompressed size"));
}

/// Builds an archive whose `summary.json` describes `declared_blob` while the
/// `blobs/` entry actually carries `stored_blob`. Passing the same slice twice
/// produces an honest archive; passing different slices reproduces a blob that
/// no longer matches what the archive says about it.
async fn spool_blob_test_archive(
    manifest: &SnapshotManifest,
    blob_path: &str,
    declared_blob: &[u8],
    stored_blob: &[u8],
) -> SpoolSnapshotArchive {
    let manifest = complete_content_manifest(manifest);
    let mut counts = empty_content_counts(&manifest);
    counts.insert("catalog_library".to_owned(), 1);
    let part_path = "postgres/catalog_library/part-000001.ndjson";
    let part = br#"{"id":"00000000-0000-0000-0000-000000000000"}
"#;
    let summary = SnapshotSummary {
        inventory: SnapshotInventory {
            postgres_row_counts: counts,
            postgres_parts: vec![snapshot_entry_digest(part_path.to_string(), part)],
            blobs: vec![snapshot_entry_digest(blob_path.to_string(), declared_blob)],
        },
    };
    let mut archive = Vec::new();
    let zstd = ZstdEncoder::new(&mut archive);
    let mut builder = Builder::new(zstd);
    append_json_entry(&mut builder, "manifest.json", &manifest).await.unwrap();
    append_raw_entry(&mut builder, part_path, part).await.unwrap();
    append_raw_entry(&mut builder, blob_path, stored_blob).await.unwrap();
    append_json_entry(&mut builder, "summary.json", &summary).await.unwrap();
    let mut zstd = builder.into_inner().await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.unwrap();
    spool_snapshot_archive(archive.as_slice()).await.unwrap()
}

#[tokio::test]
async fn prevalidation_accepts_a_blob_that_matches_what_the_archive_declares() {
    // The companion of the rejection test below: without it, a prevalidation
    // that rejected every archive with a blob would look like a working gate.
    let manifest = manifest_with_sections(vec!["catalog_library"], true);
    let blob_path = "blobs/content/workspace/library/alpha.bin";
    let blob = b"the export service listens on port 8081";
    let spool = spool_blob_test_archive(&manifest, blob_path, blob, blob).await;

    let (prevalidated, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(prevalidated.library_id, manifest.library_id);
    // No `content_revision` row names this path, so the byte contract is not
    // proven: the blob is accepted as unverifiable rather than refused.
    assert_eq!(verification, SnapshotBlobVerification { verified: 0, unverifiable: 1 });
}

#[tokio::test]
async fn prevalidation_rejects_a_blob_whose_bytes_no_longer_match_the_declared_digest() {
    let manifest = manifest_with_sections(vec!["catalog_library"], true);
    let blob_path = "blobs/content/workspace/library/alpha.bin";
    let declared = b"the export service listens on port 8081";
    // One byte, same length: a size check alone would not see this.
    let stored = b"the export service listens on port 8082";
    let spool = spool_blob_test_archive(&manifest, blob_path, declared, stored).await;

    let error = prevalidate_snapshot_archive(&spool).await.unwrap_err();

    assert!(
        error.to_string().contains("does not match observed archive inventory"),
        "a blob that diverged from its declared digest must be refused by name: {error}"
    );
}

/// Builds an archive carrying BOTH a `content_revision` NDJSON part (rows with
/// `storage_key` and `checksum`) and a `blobs/` entry, so prevalidation can
/// compare the two — the IRG-32 shape. The summary digests the bytes the
/// archive actually streams, keeping the archive self-consistent at that layer.
async fn spool_content_revision_blob_archive(
    manifest: &SnapshotManifest,
    revision_rows: &[(&str, &str)],
    blob_storage_key: &str,
    blob_bytes: &[u8],
) -> SpoolSnapshotArchive {
    let manifest = complete_content_manifest(manifest);
    let workspace_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let document = serde_json::json!({"id": document_id, "workspace_id": workspace_id,
        "library_id": manifest.library_id, "revision_sequence": revision_rows.len()});
    let head = serde_json::json!({"document_id": document_id, "active_revision_id": null,
        "readable_revision_id": null, "visibility_intent_id": null});
    let part_path = "postgres/content_revision/part-000001.ndjson";
    let mut part = Vec::new();
    for (index, (storage_key, checksum)) in revision_rows.iter().enumerate() {
        let row = serde_json::json!({
            "id": Uuid::now_v7().to_string(),
            "storage_key": storage_key,
            "checksum": checksum,
            "document_id": document_id,
            "workspace_id": workspace_id,
            "library_id": manifest.library_id,
            "revision_number": index + 1,
            "admission_identity": null,
        });
        part.extend_from_slice(&serde_json::to_vec(&row).unwrap());
        part.push(b'\n');
    }
    let blob_path = format!("blobs/{}", encode_blob_path(blob_storage_key));
    let mut counts = empty_content_counts(&manifest);
    counts.insert("content_revision".to_owned(), revision_rows.len() as u64);
    counts.insert("content_document".to_owned(), 1);
    counts.insert("content_document_head".to_owned(), 1);
    let document_path = "postgres/content_document/part-000001.ndjson";
    let head_path = "postgres/content_document_head/part-000001.ndjson";
    let mut document_bytes = serde_json::to_vec(&document).unwrap();
    document_bytes.push(b'\n');
    let mut head_bytes = serde_json::to_vec(&head).unwrap();
    head_bytes.push(b'\n');
    let mut parts = vec![
        snapshot_entry_digest(document_path.to_owned(), &document_bytes),
        snapshot_entry_digest(head_path.to_owned(), &head_bytes),
    ];
    if !part.is_empty() {
        parts.push(snapshot_entry_digest(part_path.to_owned(), &part));
    }
    let summary = SnapshotSummary {
        inventory: SnapshotInventory {
            postgres_row_counts: counts,
            postgres_parts: parts,
            blobs: vec![snapshot_entry_digest(blob_path.clone(), blob_bytes)],
        },
    };
    let mut archive = Vec::new();
    let zstd = ZstdEncoder::new(&mut archive);
    let mut builder = Builder::new(zstd);
    append_json_entry(&mut builder, "manifest.json", &manifest).await.unwrap();
    append_raw_entry(&mut builder, document_path, &document_bytes).await.unwrap();
    append_raw_entry(&mut builder, head_path, &head_bytes).await.unwrap();
    if !part.is_empty() {
        append_raw_entry(&mut builder, part_path, &part).await.unwrap();
    }
    append_raw_entry(&mut builder, &blob_path, blob_bytes).await.unwrap();
    append_json_entry(&mut builder, "summary.json", &summary).await.unwrap();
    let mut zstd = builder.into_inner().await.unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut zstd).await.unwrap();
    spool_snapshot_archive(archive.as_slice()).await.unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn storage_key_for(digest: &str) -> String {
    format!("content/workspace/library/{digest}-alpha.bin")
}

#[tokio::test]
async fn prevalidation_verifies_a_blob_against_its_content_revision_checksum() {
    let manifest = manifest_with_sections(vec!["content_revision"], true);
    let blob_bytes = b"the export service listens on port 8081";
    let digest = sha256_hex(blob_bytes);
    let storage_key = storage_key_for(&digest);
    let checksum = format!("sha256:{digest}");
    let spool = spool_content_revision_blob_archive(
        &manifest,
        &[(storage_key.as_str(), checksum.as_str())],
        &storage_key,
        blob_bytes,
    )
    .await;

    let (manifest_out, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(manifest_out.library_id, manifest.library_id);
    assert_eq!(verification, SnapshotBlobVerification { verified: 1, unverifiable: 0 });
}

#[tokio::test]
async fn prevalidation_rejects_a_blob_that_contradicts_the_checksum_it_was_uploaded_with() {
    let manifest = manifest_with_sections(vec!["content_revision"], true);
    let original = b"the export service listens on port 8081";
    let tampered = b"the export service listens on port 8082";
    let original_digest = sha256_hex(original);
    let storage_key = storage_key_for(&original_digest);
    let checksum = format!("sha256:{original_digest}");
    // `summary.json` is recomputed over the tampered bytes, so the archive is
    // self-consistent at the summary layer — this is exactly IRG-32.
    let spool = spool_content_revision_blob_archive(
        &manifest,
        &[(storage_key.as_str(), checksum.as_str())],
        &storage_key,
        tampered,
    )
    .await;

    let error = prevalidate_snapshot_archive(&spool).await.unwrap_err();
    let message = format!("{error:#}");

    assert!(message.contains(&format!("blobs/{storage_key}")), "must name the path: {message}");
    assert!(message.contains(&original_digest), "must name the expected digest: {message}");
    assert!(message.contains(&sha256_hex(tampered)), "must name the observed digest: {message}");
}

#[tokio::test]
async fn prevalidation_leaves_a_blob_unverifiable_when_the_row_checksum_and_key_disagree() {
    let manifest = manifest_with_sections(vec!["content_revision"], true);
    let blob_bytes = b"the export service listens on port 8081";
    let key_digest = sha256_hex(blob_bytes);
    let other_digest = sha256_hex(b"a different payload");
    let storage_key = storage_key_for(&key_digest);
    let checksum = format!("sha256:{other_digest}");
    let spool = spool_content_revision_blob_archive(
        &manifest,
        &[(storage_key.as_str(), checksum.as_str())],
        &storage_key,
        blob_bytes,
    )
    .await;

    let (_, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(verification, SnapshotBlobVerification { verified: 0, unverifiable: 1 });
}

#[tokio::test]
async fn prevalidation_leaves_a_blob_unverifiable_when_the_checksum_is_not_a_real_digest() {
    let manifest = manifest_with_sections(vec!["content_revision"], true);
    let blob_bytes = b"the export service listens on port 8081";
    let digest = sha256_hex(blob_bytes);
    let storage_key = storage_key_for(&digest);
    let spool = spool_content_revision_blob_archive(
        &manifest,
        &[(storage_key.as_str(), "sha256:source")],
        &storage_key,
        blob_bytes,
    )
    .await;

    let (_, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(verification, SnapshotBlobVerification { verified: 0, unverifiable: 1 });
}

#[tokio::test]
async fn prevalidation_leaves_a_blob_unverifiable_when_two_rows_disagree_about_its_checksum() {
    let manifest = manifest_with_sections(vec!["content_revision"], true);
    let blob_bytes = b"the export service listens on port 8081";
    let digest = sha256_hex(blob_bytes);
    let other_digest = sha256_hex(b"a different payload");
    let storage_key = storage_key_for(&digest);
    let spool = spool_content_revision_blob_archive(
        &manifest,
        &[
            (storage_key.as_str(), format!("sha256:{digest}").as_str()),
            (storage_key.as_str(), format!("sha256:{other_digest}").as_str()),
        ],
        &storage_key,
        blob_bytes,
    )
    .await;

    let (_, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(verification, SnapshotBlobVerification { verified: 0, unverifiable: 1 });
}

#[tokio::test]
async fn prevalidation_leaves_a_blob_with_no_content_revision_row_unverifiable() {
    let manifest = manifest_with_sections(vec!["content_revision"], true);
    let blob_bytes = b"the export service listens on port 8081";
    let digest = sha256_hex(blob_bytes);
    let storage_key = storage_key_for(&digest);
    let spool = spool_content_revision_blob_archive(&manifest, &[], &storage_key, blob_bytes).await;

    let (_, verification) = prevalidate_snapshot_archive(&spool).await.unwrap();

    assert_eq!(verification, SnapshotBlobVerification { verified: 0, unverifiable: 1 });
}

#[test]
fn blob_expectation_fold_proves_only_an_unambiguous_byte_contract() {
    let digest = sha256_hex(b"payload");
    let other = sha256_hex(b"other payload");
    let storage_key = storage_key_for(&digest);
    let checksum = format!("sha256:{digest}");
    let encoded = encode_blob_path(&storage_key);

    let mut proven = BTreeMap::new();
    fold_content_revision_blob_expectations(
        &mut proven,
        [(storage_key.as_str(), checksum.as_str())],
    );
    assert_eq!(
        proven.get(&encoded).expect("an entry for the encoded path").proven_digest(),
        Some(digest.as_str())
    );

    // A second comparable checksum for the same encoded path withdraws the proof.
    let mut conflicting = BTreeMap::new();
    fold_content_revision_blob_expectations(
        &mut conflicting,
        [
            (storage_key.as_str(), checksum.as_str()),
            (storage_key.as_str(), format!("sha256:{other}").as_str()),
        ],
    );
    assert_eq!(conflicting.get(&encoded).expect("an entry").proven_digest(), None);

    // A non-hex key segment is never a proven contract, and neither is a
    // checksum that is not a real digest.
    let mut derived = BTreeMap::new();
    fold_content_revision_blob_expectations(
        &mut derived,
        [("content/workspace/library/not-a-digest-alpha.bin", "sha256:source")],
    );
    assert_eq!(
        derived
            .get("content/workspace/library/not-a-digest-alpha.bin")
            .expect("an entry")
            .proven_digest(),
        None
    );
}

#[test]
fn blob_expectation_fold_never_proves_a_digest_across_an_encoded_path_collision() {
    // `encode_blob_path` rewrites every `..` to `__`, so two distinct storage
    // keys can collapse onto one archive path. Their differing key segments do
    // not name any digest, so the collision yields no proof at all: the blob
    // must be counted unverifiable, never proven or refused by the wrong key.
    let blob_bytes = b"the export service listens on port 8081";
    let digest = sha256_hex(blob_bytes);
    let dotted = format!("content/workspace/library/prefix..{digest}-alpha.bin");
    let underscored = format!("content/workspace/library/prefix__{digest}-alpha.bin");
    assert_eq!(encode_blob_path(&dotted), encode_blob_path(&underscored));
    let checksum = format!("sha256:{digest}");

    let mut expectations = BTreeMap::new();
    fold_content_revision_blob_expectations(
        &mut expectations,
        [(dotted.as_str(), checksum.as_str()), (underscored.as_str(), checksum.as_str())],
    );
    let encoded = encode_blob_path(&dotted);
    assert_eq!(
        expectations.get(&encoded).expect("an entry for the collided path").proven_digest(),
        None,
        "a collision must not let either key prove a digest"
    );

    let observed = SnapshotInventory {
        blobs: vec![snapshot_entry_digest(format!("blobs/{encoded}"), blob_bytes)],
        ..SnapshotInventory::default()
    };
    let verification = verify_snapshot_blobs(&observed, &expectations).unwrap();
    assert_eq!(
        verification,
        SnapshotBlobVerification { verified: 0, unverifiable: 1 },
        "the collided blob is counted unverifiable, never mis-verified or aborted"
    );
}
