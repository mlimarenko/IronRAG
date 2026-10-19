use super::test_support::*;
use super::*;

#[test]
fn strict_snapshot_inventory_requires_exact_parts_blobs_and_terminal_summary() {
    let inventory = SnapshotInventory {
        postgres_row_counts: BTreeMap::from([
            ("catalog_library".to_string(), 1),
            ("content_document".to_string(), 2),
        ]),
        postgres_parts: vec![SnapshotEntryDigest {
            path: "postgres/content_document/part-000001.ndjson".to_string(),
            size: 12,
            sha256: "a".repeat(64),
        }],
        blobs: vec![SnapshotEntryDigest {
            path: "blobs/content/example".to_string(),
            size: 3,
            sha256: "b".repeat(64),
        }],
    };
    let manifest = manifest_with_sections(vec!["catalog_library", "content_document"], true);

    assert!(validate_strict_snapshot_inventory(&manifest, &inventory).is_ok());

    let mut missing_table = inventory.clone();
    missing_table.postgres_row_counts.clear();
    assert!(validate_strict_snapshot_inventory(&manifest, &missing_table).is_err());

    let mut malformed_digest = inventory.clone();
    malformed_digest.blobs[0].sha256 = "bad".to_string();
    assert!(validate_strict_snapshot_inventory(&manifest, &malformed_digest).is_err());
}

#[test]
fn strict_snapshot_summary_must_be_terminal_and_match_observed_inventory() {
    let expected = SnapshotInventory {
        postgres_row_counts: BTreeMap::from([("catalog_library".to_string(), 1)]),
        postgres_parts: vec![SnapshotEntryDigest {
            path: "postgres/catalog_library/part-000001.ndjson".to_string(),
            size: 4,
            sha256: "c".repeat(64),
        }],
        blobs: Vec::new(),
    };
    let observed = expected.clone();

    assert!(validate_terminal_snapshot_summary(&expected, &observed, true).is_ok());
    assert!(validate_terminal_snapshot_summary(&expected, &observed, false).is_err());

    let mut changed = observed;
    changed.postgres_parts[0].size = 5;
    assert!(validate_terminal_snapshot_summary(&expected, &changed, true).is_err());
}
