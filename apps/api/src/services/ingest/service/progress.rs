use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum IngestProgressProfile {
    Balanced,
    InlineText,
    ExtractionHeavy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct IngestStageProgressWeight {
    stage_name: &'static str,
    percent_weight: i32,
}

const BALANCED_PROGRESS_WEIGHTS: [IngestStageProgressWeight; 7] = [
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EXTRACT_CONTENT, percent_weight: 12 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_PREPARE_STRUCTURE, percent_weight: 6 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_CHUNK_CONTENT, percent_weight: 5 },
    IngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
        percent_weight: 5,
    },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EMBED_CHUNK, percent_weight: 30 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EXTRACT_GRAPH, percent_weight: 40 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_FINALIZING, percent_weight: 2 },
];

const INLINE_TEXT_PROGRESS_WEIGHTS: [IngestStageProgressWeight; 7] = [
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EXTRACT_CONTENT, percent_weight: 4 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_PREPARE_STRUCTURE, percent_weight: 6 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_CHUNK_CONTENT, percent_weight: 5 },
    IngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
        percent_weight: 5,
    },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EMBED_CHUNK, percent_weight: 35 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EXTRACT_GRAPH, percent_weight: 43 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_FINALIZING, percent_weight: 2 },
];

const EXTRACTION_HEAVY_PROGRESS_WEIGHTS: [IngestStageProgressWeight; 7] = [
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EXTRACT_CONTENT, percent_weight: 30 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_PREPARE_STRUCTURE, percent_weight: 6 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_CHUNK_CONTENT, percent_weight: 4 },
    IngestStageProgressWeight {
        stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
        percent_weight: 4,
    },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EMBED_CHUNK, percent_weight: 24 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_EXTRACT_GRAPH, percent_weight: 30 },
    IngestStageProgressWeight { stage_name: INGEST_STAGE_FINALIZING, percent_weight: 2 },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngestStageMetadata {
    pub stage_name: &'static str,
    pub stage_rank: i32,
    pub lifecycle_kind: &'static str,
}

const fn progress_weights_for_profile(
    profile: IngestProgressProfile,
) -> &'static [IngestStageProgressWeight; 7] {
    match profile {
        IngestProgressProfile::Balanced => &BALANCED_PROGRESS_WEIGHTS,
        IngestProgressProfile::InlineText => &INLINE_TEXT_PROGRESS_WEIGHTS,
        IngestProgressProfile::ExtractionHeavy => &EXTRACTION_HEAVY_PROGRESS_WEIGHTS,
    }
}

pub(super) fn ingest_stage_progress_percent_for_profile(
    profile: IngestProgressProfile,
    stage_name: &str,
    stage_state: &str,
) -> Option<i32> {
    let weights = progress_weights_for_profile(profile);
    let stage_index = weights.iter().position(|candidate| candidate.stage_name == stage_name)?;
    let progress_before_stage: i32 =
        weights.iter().take(stage_index).map(|weight| weight.percent_weight).sum();
    let stage_weight = weights[stage_index].percent_weight;

    match stage_state {
        "started" | "failed" => {
            let visible_started_bump = (stage_weight / 5).clamp(1, 5);
            Some((progress_before_stage + visible_started_bump).min(99))
        }
        "completed" => Some((progress_before_stage + stage_weight).min(100)),
        _ => None,
    }
}

#[must_use]
pub(super) fn ingest_stage_unit_progress_percent_for_profile(
    profile: IngestProgressProfile,
    stage_name: &str,
    completed_units: u32,
    total_units: u32,
) -> Option<i32> {
    if total_units == 0 {
        return None;
    }

    let stage_started = ingest_stage_progress_percent_for_profile(profile, stage_name, "started")?;
    let stage_completed =
        ingest_stage_progress_percent_for_profile(profile, stage_name, "completed")?;
    let stage_span = (stage_completed - stage_started).max(1);
    let completed_units = completed_units.min(total_units);
    let stage_offset =
        ((i64::from(completed_units) * i64::from(stage_span)) / i64::from(total_units)) as i32;

    Some((stage_started + stage_offset).clamp(stage_started, stage_completed.min(99)))
}

pub(super) fn ingest_attempt_stage_progress_percent(
    existing_events: &[IngestStageEventRow],
    stage_name: &str,
    stage_state: &str,
    current_details: &Value,
) -> Option<i32> {
    let profile = ingest_progress_profile(existing_events, current_details);
    ingest_stage_progress_percent_for_profile(profile, stage_name, stage_state)
}

pub(super) fn ingest_attempt_stage_unit_progress_percent(
    existing_events: &[IngestStageEventRow],
    stage_name: &str,
    completed_units: u32,
    total_units: u32,
    current_details: &Value,
) -> Option<i32> {
    let profile = ingest_progress_profile(existing_events, current_details);
    ingest_stage_unit_progress_percent_for_profile(
        profile,
        stage_name,
        completed_units,
        total_units,
    )
}

pub(super) fn ingest_progress_profile(
    existing_events: &[IngestStageEventRow],
    current_details: &Value,
) -> IngestProgressProfile {
    progress_profile_from_stage_details(current_details)
        .or_else(|| {
            existing_events
                .iter()
                .rev()
                .find_map(|event| progress_profile_from_stage_details(&event.details_json))
        })
        .unwrap_or(IngestProgressProfile::Balanced)
}

pub(super) fn progress_profile_from_stage_details(
    details: &Value,
) -> Option<IngestProgressProfile> {
    let source = details.get("source").and_then(Value::as_str);
    if source == Some("knowledge_revision") {
        return Some(IngestProgressProfile::InlineText);
    }

    let file_kind = details.get("fileKind").and_then(Value::as_str);
    match file_kind {
        Some("text_like") => Some(IngestProgressProfile::InlineText),
        Some("pdf" | "image" | "docx" | "spreadsheet" | "pptx") => {
            Some(IngestProgressProfile::ExtractionHeavy)
        }
        Some(_) => Some(IngestProgressProfile::Balanced),
        None => {
            let has_pages =
                details.get("pageCount").and_then(Value::as_i64).unwrap_or_default() > 0;
            let has_extract_units =
                details.get("extractUnitCount").and_then(Value::as_i64).unwrap_or_default() > 0;
            if has_pages || has_extract_units {
                Some(IngestProgressProfile::ExtractionHeavy)
            } else {
                None
            }
        }
    }
}

#[must_use]
pub fn ingest_stage_metadata(stage_name: &str) -> Option<IngestStageMetadata> {
    match stage_name {
        INGEST_STAGE_EXTRACT_CONTENT => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_CONTENT,
            stage_rank: 10,
            lifecycle_kind: "preparation",
        }),
        INGEST_STAGE_PREPARE_STRUCTURE => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_PREPARE_STRUCTURE,
            stage_rank: 20,
            lifecycle_kind: "preparation",
        }),
        INGEST_STAGE_CHUNK_CONTENT => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_CHUNK_CONTENT,
            stage_rank: 30,
            lifecycle_kind: "preparation",
        }),
        INGEST_STAGE_EMBED_CHUNK => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_EMBED_CHUNK,
            stage_rank: 50,
            lifecycle_kind: "embedding",
        }),
        INGEST_STAGE_EXTRACT_TECHNICAL_FACTS => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_TECHNICAL_FACTS,
            stage_rank: 40,
            lifecycle_kind: "grounding",
        }),
        INGEST_STAGE_EXTRACT_GRAPH => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_EXTRACT_GRAPH,
            stage_rank: 60,
            lifecycle_kind: "graph",
        }),
        INGEST_STAGE_VERIFY_QUERY_ANSWER => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_VERIFY_QUERY_ANSWER,
            stage_rank: 70,
            lifecycle_kind: "query",
        }),
        INGEST_STAGE_FINALIZING => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_FINALIZING,
            stage_rank: 80,
            lifecycle_kind: "finalization",
        }),
        INGEST_STAGE_WEB_DISCOVERY => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_WEB_DISCOVERY,
            stage_rank: 15,
            lifecycle_kind: "web_discovery",
        }),
        INGEST_STAGE_WEB_MATERIALIZE_PAGE => Some(IngestStageMetadata {
            stage_name: INGEST_STAGE_WEB_MATERIALIZE_PAGE,
            stage_rank: 25,
            lifecycle_kind: "web_materialization",
        }),
        _ => None,
    }
}

pub(super) fn normalize_optional_stage(
    stage_name: Option<String>,
) -> Result<Option<String>, ApiError> {
    stage_name.map(|value| normalize_stage_name(&value)).transpose()
}

pub(super) fn normalize_stage_name(stage_name: &str) -> Result<String, ApiError> {
    let normalized = stage_name.trim().to_ascii_lowercase();
    ingest_stage_metadata(&normalized)
        .map(|metadata| metadata.stage_name.to_string())
        .ok_or_else(|| ApiError::BadRequest(format!("unsupported ingest stage: {stage_name}")))
}
