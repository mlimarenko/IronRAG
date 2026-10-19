//! Contract gate: query-path reads of a revision are bounded, never
//! fetch-everything-then-truncate.
//!
//! The full-revision load followed by an in-Rust truncate was a real
//! whole-library restore outage (R4). There is no unit-test seam for this
//! invariant: the bounded helpers require a live PostgreSQL connection, and the
//! failure mode is a *call site* choosing the unbounded `list_chunks_by_revision`
//! over the bounded head/range builders. This file therefore scans the query
//! source tree for that unbounded call as the only executable enforcement at
//! this layer — a cross-cutting architectural contract, not a per-line
//! implementation mirror. Exemptions carry a reason and are checked for
//! staleness, so a removed exemption fails instead of silently drifting.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const QUERY_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/services/query");

const EXEMPTIONS: &[(&str, &str)] = &[
    (
        "execution/table_retrieval.rs",
        "predicate is table_row / table-summary metadata_block, not a reading-order head",
    ),
    ("search/reuse.rs", "ingest vector reuse needs every parent chunk"),
    ("search/embedding.rs", "ingest embed needs every chunk"),
];

fn unlimited_revision_list_call_count(source: &str) -> usize {
    source.matches("list_chunks_by_revision(").count()
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).unwrap_or_else(|error| panic!("read {dir:?}: {error}"));
        for entry in entries {
            let entry = entry.unwrap_or_else(|error| panic!("entry {dir:?}: {error}"));
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
                let relative = relative_query_path(&path);
                if name.ends_with("_tests.rs")
                    || name == "tests.rs"
                    || relative.contains("/tests/")
                    || relative == "revision_hydration_tests.rs"
                {
                    continue;
                }
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

fn relative_query_path(path: &Path) -> String {
    path.strip_prefix(QUERY_ROOT).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

#[test]
fn planted_fetch_all_then_truncate_fails_the_query_path_gate() {
    let planted = r#"
        let rows = match state.document_store.list_chunks_by_revision(revision_id).await {
            Ok(rows) => rows,
            Err(_) => return None,
        };
        rows.sort_by_key(|row| row.chunk_index);
        rows.truncate(400);
    "#;
    assert_eq!(
        unlimited_revision_list_call_count(planted),
        1,
        "the deleted declaration-inventory full-revision load must stay detectable"
    );
    assert_eq!(unlimited_revision_list_call_count("list_chunks_by_revision_matching_terms("), 0);
    assert_eq!(unlimited_revision_list_call_count("list_chunks_by_revision_range("), 0);
    assert_eq!(unlimited_revision_list_call_count("list_head_chunks_by_revision("), 0);
}

#[test]
fn query_path_unlimited_revision_hydration_is_closed() {
    let root = Path::new(QUERY_ROOT);
    let exemption_index: BTreeMap<&str, &str> = EXEMPTIONS.iter().copied().collect();
    let mut unused_exemptions: BTreeMap<&str, &str> = exemption_index.clone();
    let mut violations = Vec::new();

    for path in rust_sources(root) {
        let relative = relative_query_path(&path);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let hits = unlimited_revision_list_call_count(&source);
        if hits == 0 {
            continue;
        }
        if let Some(_reason) = unused_exemptions.remove(relative.as_str()) {
            continue;
        }
        violations.push(format!("{relative}: {hits} list_chunks_by_revision( call(s)"));
    }

    assert!(
        violations.is_empty(),
        "query-path full-revision hydration must be exempted with a reason; planted preflight fetch-all-then-truncate is this failure:\n{}",
        violations.join("\n")
    );
    assert!(
        unused_exemptions.is_empty(),
        "stale exemptions (call removed, reason must go): {:?}",
        unused_exemptions.keys().collect::<Vec<_>>()
    );
}

/// The declaration-inventory path must use the bounded head helper with a SQL
/// `LIMIT`, not a full-revision load then a Rust `truncate`. Its "no unbounded
/// `list_chunks_by_revision(`" half is already covered by
/// [`query_path_unlimited_revision_hydration_is_closed`]; this test pins the
/// remaining contract (bounded head helper + limit constant, no Rust truncate)
/// against the owning source because the function itself has no DB-free seam.
#[test]
fn declaration_inventory_uses_revision_head_helper() {
    let source = include_str!("execution/answer_pipeline/preflight/mod.rs");
    assert!(source.contains("list_head_chunks_by_revision("));
    assert!(source.contains("DECLARATION_INVENTORY_DOCUMENT_CHUNK_LIMIT"));
    assert_eq!(
        unlimited_revision_list_call_count(source),
        0,
        "declaration inventory must not hydrate a full revision then truncate in Rust"
    );
    assert!(!source.contains("rows.truncate("), "SQL LIMIT replaced the Rust truncate");
}
