use super::*;

pub(super) fn manifest_with_sections(
    postgres_tables: Vec<&str>,
    has_blobs: bool,
) -> SnapshotManifest {
    SnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        library_id: Uuid::now_v7(),
        library_slug: "sample-library".to_string(),
        exported_at: chrono::Utc::now(),
        source_version: "0.0.0-test".to_string(),
        include_kinds: if has_blobs {
            vec![IncludeKind::LibraryData, IncludeKind::Blobs]
        } else {
            vec![IncludeKind::LibraryData]
        },
        postgres_tables: postgres_tables.into_iter().map(str::to_string).collect(),
        has_blobs,
        vector_shards: Vec::new(),
    }
}
