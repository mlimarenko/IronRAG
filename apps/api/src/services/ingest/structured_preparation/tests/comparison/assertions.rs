//! Native technical-fact baseline over the same immutable canonical source blocks.
//! Proposed assertions remain an experimental derived representation, never source.

use std::time::Instant;

use anyhow::{Result, ensure};
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::{CanonicalInput, hash, pinned_environment, write_report};
use crate::services::ingest::technical_facts::{
    ExtractTechnicalFactsCommand, TechnicalFactService,
};

#[test]
#[ignore = "explicit pinned canonical input and create-new fact baseline; no models or database"]
fn native_atomic_assertion_input_export() -> Result<()> {
    let (bytes, input_sha) = pinned_environment("ASSERTION_CANONICAL_INPUT")?;
    let canonical: CanonicalInput = serde_json::from_slice(&bytes)?;
    ensure!(canonical.protocol == "irg256-native-treatment-input/1", "wrong canonical input");
    ensure!(canonical.cases.len() == 4, "requires the frozen four-category corpus");
    let source_sha = std::env::var("ASSERTION_SOURCE_SHA")?;
    ensure!(
        source_sha.len() == 40 && source_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "comparison source identity missing"
    );
    let mut cases = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for case in canonical.cases {
        ensure!(ids.insert(case.case_id.clone()), "duplicate canonical case");
        ensure!(
            hash(case.normalized_text.as_bytes()) == case.normalized_sha256,
            "normalized source identity mismatch"
        );
        let document_id = Uuid::new_v5(&Uuid::NAMESPACE_URL, case.case_id.as_bytes());
        let revision_id = Uuid::new_v5(&document_id, case.normalized_sha256.as_bytes());
        let started = Instant::now();
        let extracted = TechnicalFactService::new().extract_from_blocks(
            ExtractTechnicalFactsCommand {
                revision_id,
                document_id,
                workspace_id: Uuid::from_u128(0x25700000_0000_4000_8000_000000000001),
                library_id: Uuid::from_u128(0x25700000_0000_4000_8000_000000000002),
                blocks: case.blocks.clone(),
            },
            &CancellationToken::new(),
        )?;
        let extraction_ms = started.elapsed().as_secs_f64() * 1000.0;
        let block_ids = case
            .blocks
            .iter()
            .map(|block| block.block_id)
            .collect::<std::collections::HashSet<_>>();
        ensure!(
            extracted.facts.iter().all(|fact| fact
                .support_block_ids
                .iter()
                .all(|identity| block_ids.contains(identity))),
            "unresolved native fact source support"
        );
        cases.push(json!({"caseId":case.case_id,"category":case.category,
            "sourceSha256":case.source_sha256,"normalizedSha256":case.normalized_sha256,
            "normalizedText":case.normalized_text,"blocks":case.blocks,
            "technicalFacts":extracted.facts,"technicalConflicts":extracted.conflicts,
            "technicalExtractionMs":extraction_ms,
            "technicalFactBytes":serde_json::to_vec(&extracted.facts)?.len()}));
    }
    write_report(
        "ASSERTION_NATIVE_EXPORT",
        &json!({
            "protocol":"irg257-native-assertion-input/1","comparisonSourceSha":source_sha,
            "preparationSourceSha":canonical.source_sha,"canonicalInputSha256":input_sha,
            "inputManifestSha256":canonical.input_manifest_sha256,"cases":cases,
            "technicalFactOwnerSha256":hash(include_bytes!("../../../technical_facts.rs")),
            "exporterSha256":hash(include_bytes!("assertions.rs")),
            "scope":"Actual native technical-fact extraction plus original source blocks; not proposition quality or retrieval",
            "providerCalls":0
        }),
    )
}
