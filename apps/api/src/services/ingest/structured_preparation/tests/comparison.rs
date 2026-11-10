//! Opt-in data export through the existing extraction/preparation owners.
//! Parameter profiles are not recursive, page or semantic strategy arms.

use std::{fs, io::Write, path::PathBuf, time::Instant};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{StructuredPreparationService, prepare_extracted_content};
use crate::shared::extraction::{
    chunking::{
        StructuredChunkingProfile, build_structured_chunk_windows,
        comparison::{Treatment, prepare_treatment},
    },
    structured_document::StructuredBlockData,
};
use crate::{
    integrations::{
        extraction_protocol::{Request, Response},
        xberg::{XbergSource, project_native, validate_response},
    },
    shared::extraction::file_extract::{
        FileExtractionPlan, UploadFileKind, build_inline_text_extraction_plan,
        build_plan_from_extraction, xberg_owned_recognition_profile,
    },
};

mod assertions;
mod retrieval;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    protocol: String,
    base_source_sha: String,
    cases: Vec<Case>,
    profiles: Vec<Profile>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    id: String,
    category: String,
    source_path: PathBuf,
    source_sha256: String,
    format: String,
    native_request_path: Option<PathBuf>,
    native_request_sha256: Option<String>,
    native_response_path: Option<PathBuf>,
    native_response_sha256: Option<String>,
}

#[derive(Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Profile {
    id: String,
    max_chars: usize,
    overlap_chars: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalInput {
    protocol: String,
    source_sha: String,
    input_manifest_sha256: String,
    profiles: Vec<Profile>,
    cases: Vec<CanonicalCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CanonicalCase {
    case_id: String,
    category: String,
    format: String,
    source_sha256: String,
    normalized_sha256: String,
    normalized_text: String,
    blocks: Vec<StructuredBlockData>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SemanticBoundaries {
    protocol: String,
    canonical_input_sha256: String,
    source_sha: String,
    cases: Vec<SemanticCase>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SemanticCase {
    case_id: String,
    normalized_sha256: String,
    break_before_ordinals: Vec<usize>,
}

fn pinned_environment(variable: &str) -> Result<(Vec<u8>, String)> {
    let path = std::env::var(variable)?;
    let identity = std::env::var(format!("{variable}_SHA256"))?;
    Ok((pinned(std::path::Path::new(&path), &identity)?, identity))
}

#[test]
#[ignore = "explicit canonical blocks and query-blind semantic boundaries; no model calls"]
fn native_category_treatment_export() -> Result<()> {
    let (input_bytes, input_sha) = pinned_environment("CHUNKING_CANONICAL_INPUT")?;
    let input: CanonicalInput = serde_json::from_slice(&input_bytes)?;
    ensure!(input.protocol == "irg256-native-treatment-input/1", "wrong canonical protocol");
    ensure!(!input.cases.is_empty() && input.cases.len() <= 16, "invalid canonical case count");
    ensure!(!input.profiles.is_empty() && input.profiles.len() <= 16, "invalid profile count");
    let (semantic_bytes, semantic_sha) = pinned_environment("CHUNKING_SEMANTIC_BOUNDARIES")?;
    let semantic: SemanticBoundaries = serde_json::from_slice(&semantic_bytes)?;
    ensure!(
        semantic.protocol == "irg256-semantic-boundaries/1"
            && semantic.canonical_input_sha256 == input_sha
            && semantic.source_sha == input.source_sha,
        "semantic preparation identity mismatch"
    );
    ensure!(semantic.cases.len() == input.cases.len(), "semantic case coverage mismatch");
    let mut results = Vec::new();
    for case in input.cases {
        ensure!(
            hash(case.normalized_text.as_bytes()) == case.normalized_sha256,
            "canonical normalized text identity mismatch"
        );
        let matching =
            semantic.cases.iter().filter(|row| row.case_id == case.case_id).collect::<Vec<_>>();
        ensure!(
            matching.len() == 1 && matching[0].normalized_sha256 == case.normalized_sha256,
            "semantic case identity mismatch"
        );
        let mut treatments = Vec::new();
        for profile in &input.profiles {
            let settings = StructuredChunkingProfile {
                max_chars: profile.max_chars,
                overlap_chars: profile.overlap_chars,
                max_input_bytes: None,
            };
            ensure!(
                settings.max_chars > 0 && settings.overlap_chars < settings.max_chars,
                "invalid treatment profile"
            );
            for (id, treatment) in [
                ("structural", None),
                ("recursive", Some(Treatment::Recursive)),
                ("semantic", Some(Treatment::Semantic)),
                ("page", Some(Treatment::Page)),
            ] {
                if id == "page" && case.format != "pdf" {
                    continue;
                }
                let started = Instant::now();
                let (blocks, chunks, boundaries, whitespace_spans) =
                    if let Some(treatment) = treatment {
                        let breaks = if treatment == Treatment::Semantic {
                            matching[0].break_before_ordinals.as_slice()
                        } else {
                            &[]
                        };
                        let prepared = prepare_treatment(&case.blocks, settings, treatment, breaks)
                            .map_err(anyhow::Error::msg)?;
                        (
                            prepared.blocks,
                            prepared.chunks,
                            prepared.boundary_count,
                            prepared.whitespace_spans,
                        )
                    } else {
                        (
                            case.blocks.clone(),
                            build_structured_chunk_windows(&case.blocks, settings, None)?,
                            0,
                            Vec::new(),
                        )
                    };
                ensure!(!chunks.is_empty(), "empty treatment result");
                treatments.push(json!({"treatmentId":id,"profileId":profile.id,
                    "maxChars":settings.max_chars,"overlapChars":settings.overlap_chars,
                    "preparationMs":started.elapsed().as_secs_f64()*1000.0,
                    "boundaryCount":boundaries,"blocks":blocks,"chunks":chunks,
                    "whitespaceSpans":whitespace_spans,
                    "chunksOverNominalProfile":chunks.iter().filter(|chunk|
                        chunk.normalized_text.chars().count() > settings.max_chars).count(),
                    "normalizedChars":chunks.iter().map(|chunk| chunk.normalized_text.chars().count()).sum::<usize>(),
                    "contextChars":chunks.iter().map(|chunk| chunk.window_text.as_deref()
                        .unwrap_or(&chunk.content_text).chars().count()).sum::<usize>()}));
            }
        }
        results.push(json!({"caseId":case.case_id,"category":case.category,
            "sourceSha256":case.source_sha256,"normalizedSha256":case.normalized_sha256,
            "normalizedText":case.normalized_text,"blocks":case.blocks,"treatments":treatments}));
    }
    write_report(
        "CHUNKING_TREATMENT_EXPORT",
        &json!({
            "protocol":"irg256-native-treatment-export/1","sourceSha":input.source_sha,
            "inputManifestSha256":input.input_manifest_sha256,"canonicalInputSha256":input_sha,
            "semanticBoundariesSha256":semantic_sha,"exporterSha256":hash(include_bytes!("comparison.rs")),
            "treatmentOwnerSha256":hash(include_bytes!("../../../../shared/extraction/chunking/comparison.rs")),
            "cases":results,"providerCalls":0,"retrievalOrAnswerQualityMeasured":false,
            "scope":"One small pilot; page arm PDF-only; semantic boundaries are model-derived, all arms share native rendering"
        }),
    )
}

fn load_manifest() -> Result<(Manifest, String)> {
    let input = std::env::var("CHUNKING_COMPARISON_INPUT")?;
    let input_sha = std::env::var("CHUNKING_COMPARISON_INPUT_SHA256")?;
    let manifest: Manifest =
        serde_json::from_slice(&pinned(std::path::Path::new(&input), &input_sha)?)?;
    ensure!(manifest.protocol == "irg256-category-pilot-input/1", "wrong pilot protocol");
    ensure!(
        manifest.base_source_sha.len() == 40
            && manifest.base_source_sha.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "base source identity missing"
    );
    ensure!(
        !manifest.cases.is_empty()
            && manifest.cases.len() <= 16
            && !manifest.profiles.is_empty()
            && manifest.profiles.len() <= 16,
        "empty or oversized pilot"
    );
    let mut ids = std::collections::HashSet::new();
    for case in &manifest.cases {
        ensure!(ids.insert(&case.id), "duplicate case identity");
    }
    ids.clear();
    for profile in &manifest.profiles {
        ensure!(ids.insert(&profile.id), "duplicate profile identity");
        ensure!(
            profile.max_chars > 0 && profile.overlap_chars < profile.max_chars,
            "invalid character profile"
        );
    }
    Ok((manifest, input_sha))
}

fn write_report(variable: &str, report: &Value) -> Result<()> {
    let path = std::env::var(variable)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(&serde_json::to_vec_pretty(report)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}

#[test]
#[ignore = "explicit immutable pilot input and create-new canonical export; no model calls"]
fn native_category_treatment_input() -> Result<()> {
    let (manifest, input_sha) = load_manifest()?;
    let mut cases = Vec::new();
    for case in &manifest.cases {
        let plan = extraction_plan(case)?;
        let source = plan.source_text.as_ref().context("canonical source missing")?;
        let normalized = plan.normalized_text.as_ref().context("normalized text missing")?;
        let started = Instant::now();
        let prepared = prepare_extracted_content(
            plan.source_format_metadata.source_format.clone(),
            plan.normalization_profile.clone(),
            source.clone(),
            normalized.clone(),
            plan.structure_hints.clone(),
            StructuredPreparationService::default(),
        );
        cases.push(json!({"caseId":case.id,"category":case.category,"format":case.format,
            "sourceSha256":case.source_sha256,"normalizedSha256":hash(normalized.as_bytes()),
            "normalizedText":normalized,"sourceText":source,
            "canonicalPreparationMs":started.elapsed().as_secs_f64()*1000.0,
            "blocks":prepared.ordered_blocks,"sourceMap":plan.source_map}));
    }
    write_report(
        "CHUNKING_CANONICAL_EXPORT",
        &json!({
            "protocol":"irg256-native-treatment-input/1","sourceSha":manifest.base_source_sha,
            "inputManifestSha256":input_sha,"profiles":manifest.profiles,"cases":cases,
            "canonicalBlockProfile":{"maxChars":2800,"overlapChars":280},
            "proofScope":"Frozen canonical blocks for all treatments; no retrieval scoring or model calls",
            "providerCalls":0
        }),
    )
}

fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn pinned(path: &std::path::Path, expected: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(path)?;
    ensure!(bytes.len() <= 16 * 1024 * 1024, "pilot input exceeds bounded file size");
    ensure!(hash(&bytes) == expected, "pilot input identity mismatch");
    Ok(bytes)
}

fn extraction_plan(case: &Case) -> Result<FileExtractionPlan> {
    let bytes = pinned(&case.source_path, &case.source_sha256)?;
    if case.format == "txt" {
        return Ok(build_inline_text_extraction_plan(std::str::from_utf8(&bytes)?));
    }
    let kind = match case.format.as_str() {
        "pdf" => UploadFileKind::Pdf,
        "docx" => UploadFileKind::Docx,
        _ => anyhow::bail!("pilot format is not admitted"),
    };
    let request: Request = serde_json::from_slice(&pinned(
        case.native_request_path.as_deref().context("native request missing")?,
        case.native_request_sha256.as_deref().context("native request pin missing")?,
    )?)?;
    ensure!(request.source_sha256 == case.source_sha256, "request/source identity mismatch");
    let response: Response = serde_json::from_slice(&pinned(
        case.native_response_path.as_deref().context("native response missing")?,
        case.native_response_sha256.as_deref().context("native response pin missing")?,
    )?)?;
    let native = validate_response(response, &request)?;
    ensure!(native.visual_inputs.is_empty(), "pilot does not admit paid visual recognition");
    let source = XbergSource {
        file_name: case.source_path.file_name().and_then(|name| name.to_str()),
        mime_type: Some(&request.mime),
        source_format: &case.format,
    };
    let extracted = project_native(native, source)?;
    let profile = xberg_owned_recognition_profile(kind, &case.format)?;
    Ok(build_plan_from_extraction(kind, extracted, profile))
}

#[test]
#[ignore = "explicit pinned category manifest and create-new export path; no model calls"]
fn native_category_profile_export() -> Result<()> {
    let (manifest, input_sha) = load_manifest()?;
    let mut results = Vec::new();
    for case in &manifest.cases {
        let plan = extraction_plan(case)?;
        let source = plan.source_text.as_ref().context("canonical source text missing")?;
        let normalized =
            plan.normalized_text.as_ref().context("canonical normalized text missing")?;
        for profile in &manifest.profiles {
            ensure!(
                profile.max_chars > 0 && profile.overlap_chars < profile.max_chars,
                "invalid character profile"
            );
            let started = Instant::now();
            let prepared = prepare_extracted_content(
                plan.source_format_metadata.source_format.clone(),
                plan.normalization_profile.clone(),
                source.clone(),
                normalized.clone(),
                plan.structure_hints.clone(),
                StructuredPreparationService::with_chunking(
                    profile.max_chars,
                    profile.overlap_chars,
                ),
            );
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let chunks = &prepared.chunk_windows;
            results.push(json!({"caseId":case.id,"category":case.category,"profileId":profile.id,
                "sourceSha256":case.source_sha256,"normalizedSha256":hash(normalized.as_bytes()),
                "maxChars":profile.max_chars,"overlapChars":profile.overlap_chars,
                "preparationMs":elapsed_ms,"chunks":chunks.len(),
                "totalNormalizedChars":chunks.iter().map(|c| c.normalized_text.chars().count()).sum::<usize>(),
                "totalWindowChars":chunks.iter().filter_map(|c| c.window_text.as_deref()).map(|s| s.chars().count()).sum::<usize>(),
                "chunksOverNominalProfile":chunks.iter().filter(|c| c.normalized_text.chars().count()>profile.max_chars).count(),
                "maxObservedChunkChars":chunks.iter().map(|c| c.normalized_text.chars().count()).max(),
                "sourceMap":plan.source_map,"prepared":prepared}));
        }
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let source_hashes: Value = json!({
        "structured_preparation.rs":hash(&fs::read(root.join("src/services/ingest/structured_preparation.rs"))?),
        "blocks.rs":hash(&fs::read(root.join("src/services/ingest/structured_preparation/blocks.rs"))?),
        "chunking.rs":hash(&fs::read(root.join("src/shared/extraction/chunking.rs"))?),
        "comparison.rs":hash(include_bytes!("comparison.rs"))});
    let report = json!({"protocol":"irg256-native-profile-export/1","manifestSha256":input_sha,
        "baseSourceSha":manifest.base_source_sha,"sourceFileHashes":source_hashes,"results":results,
        "proofScope":"native extraction projection and character-profile structure/preparation only",
        "recursivePageSemanticTreatmentsMeasured":false,"retrievalOrAnswerQualityMeasured":false,
        "actualIndexBytesMeasured":false,"providerCalls":0});
    write_report("CHUNKING_COMPARISON_EXPORT", &report)
}
