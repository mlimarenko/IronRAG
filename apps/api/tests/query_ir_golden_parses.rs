//! Gate: every line in `tests/query_ir_golden.jsonl` must deserialize into a
//! valid `QueryIR`. If this fails, either the schema drifted from the
//! labelling guide or the golden set has bad data — both block merges.

use ironrag_backend::domains::query_ir::QueryIR;
use std::path::PathBuf;

#[test]
fn all_golden_expected_ir_entries_deserialize() {
    let path: PathBuf =
        [env!("CARGO_MANIFEST_DIR"), "tests", "query_ir_golden.jsonl"].iter().collect();
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));

    let mut parsed_count = 0usize;
    let mut failures = Vec::<(usize, String, String)>::new();

    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|error| panic!("line {}: invalid JSON: {error}", index + 1));
        let mut expected_ir = row
            .get("expected_ir")
            .cloned()
            .unwrap_or_else(|| panic!("line {}: missing `expected_ir` field", index + 1));
        // The golden set stores the per-row language on the top-level row so
        // annotators can edit it without drilling into expected_ir. The IR
        // schema puts it inside the IR object — so copy it down before we
        // feed the row into the struct.
        if let (Some(row_language), Some(object)) =
            (row.get("language"), expected_ir.as_object_mut())
        {
            object.entry("language".to_string()).or_insert(row_language.clone());
        }
        match serde_json::from_value::<QueryIR>(expected_ir) {
            Ok(_) => parsed_count += 1,
            Err(error) => {
                let question = row
                    .get("question")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("<missing>")
                    .to_string();
                failures.push((index + 1, question, error.to_string()));
            }
        }
    }

    if !failures.is_empty() {
        let mut message =
            format!("{} / {} golden entries failed to parse:\n", failures.len(), parsed_count);
        for (line, question, error) in failures.iter().take(10) {
            message.push_str(&format!("  line {line}: `{question}` — {error}\n"));
        }
        if failures.len() > 10 {
            message.push_str(&format!("  ... and {} more\n", failures.len() - 10));
        }
        panic!("{message}");
    }

    assert!(parsed_count >= 200, "expected at least 200 golden rows, got {parsed_count}");
}
