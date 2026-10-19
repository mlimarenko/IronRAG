pub(crate) mod acronym_gloss;
mod entity_type;
#[cfg(test)]
mod extraction_bench_tests;
mod parse;
mod persistence_retry;
mod prompt;
mod recovery;
mod session;
mod type_reference;
mod types;
mod watchdog;

#[cfg(test)]
mod tests;

// Public re-exports used across the crate
pub(crate) use entity_type::{
    GraphEntityTypeClassifierIdentity, classify_graph_entity_candidates_if_bound,
};
pub(crate) use parse::normalized_graph_extraction_json;
pub(crate) use parse::repair_graph_extraction_candidate_set;
pub(crate) use parse::repair_graph_extraction_normalized_json;
pub(crate) use prompt::{GRAPH_EXTRACTION_CONTRACT_VERSION, GRAPH_EXTRACTION_VERSION};
pub(crate) use recovery::{
    extraction_lifecycle_from_record, extraction_recovery_summary_from_record,
};
pub(crate) use types::{
    GraphEntityCandidate, GraphExtractionCandidateSet, GraphExtractionExecutionError,
    GraphExtractionOutcome, GraphExtractionRequest, GraphExtractionResumeState,
    GraphExtractionStructuredChunkContext, GraphExtractionSubTypeHintEntry,
    GraphExtractionSubTypeHintGroup, GraphExtractionSubTypeHints, GraphExtractionTaskFailure,
    GraphExtractionTaskFailureCode, GraphExtractionTechnicalFact, GraphRelationCandidate,
};
pub(crate) use watchdog::{GraphExtractionActivity, GraphExtractionIdleWatchdog};

#[cfg(test)]
#[allow(unused_imports, reason = "integration tests consume this test-only prompt facade")]
pub(crate) use prompt::build_graph_extraction_prompt;

use crate::{
    agent_runtime::{
        builder::StructuredRequestBuilder,
        executor::RuntimeExecutionSession,
        persistence as runtime_persistence,
        response::{RuntimeRecoveryOutcome, RuntimeTerminalOutcome},
        tasks::graph_extract::{GraphExtractTask, GraphExtractTaskInput},
    },
    app::state::AppState,
    domains::{
        agent_runtime::{RuntimeExecutionOwner, RuntimeStageKind, RuntimeStageState},
        graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    },
    infra::{
        knowledge_plane::{IngestAttemptMutationOutcome, IngestRevisionWriteAuthority},
        repositories,
    },
    services::{
        ai_catalog_service::ResolvedRuntimeBinding, ingest::runtime::RuntimeTaskExecutionContext,
    },
};

use persistence_retry::{resolve_unique_conflict, retry_graph_persistence};
use prompt::{build_graph_extraction_prompt_plan, normalized_downgrade_level};
use recovery::{
    GraphRuntimeStageUpdate, append_graph_runtime_policy_audit, begin_graph_runtime_stage,
    graph_async_operation_status, graph_extraction_cancelled_error,
    graph_extraction_execution_error, graph_failure_code_from_outcome,
    make_graph_runtime_failure_summary, make_graph_terminal_failure_outcome,
    map_graph_runtime_execution_error, record_graph_runtime_stage,
};
use session::{GraphExtractionRawOutput, build_raw_output_json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use types::{GraphExtractionFailureOutcome, GraphExtractionPromptVariant};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphExtractionCacheFingerprint {
    pub extraction_version: String,
    /// Stable semantic cache identity (IRG-197). Independent of the rendered
    /// prompt; see [`graph_extraction_contract_hash`].
    pub extraction_contract_hash: String,
    pub request_shape_key: String,
    pub request_size_bytes: usize,
}

#[must_use]
pub(crate) fn build_graph_extraction_cache_fingerprint(
    request: &GraphExtractionRequest,
    runtime_binding: &ResolvedRuntimeBinding,
    classifier: GraphEntityTypeClassifierIdentity<'_>,
    request_size_soft_limit_bytes: usize,
) -> GraphExtractionCacheFingerprint {
    let prompt_plan = build_graph_extraction_prompt_plan(
        request,
        GraphExtractionPromptVariant::Initial,
        None,
        None,
        None,
        request_size_soft_limit_bytes,
    );
    GraphExtractionCacheFingerprint {
        extraction_version: prompt::GRAPH_EXTRACTION_VERSION.to_string(),
        extraction_contract_hash: graph_extraction_contract_hash(
            request,
            runtime_binding,
            classifier,
            request_size_soft_limit_bytes,
        ),
        request_shape_key: prompt_plan.request_shape_key,
        request_size_bytes: prompt_plan.request_size_bytes,
    }
}

/// Stable, request-shaped inputs that change the extraction prompt and so must
/// change the cache identity when they change.
///
/// Volatile guidance is deliberately absent: the chunk ordinal, the
/// `sub_type_hints` occurrence counts and their vocabulary drift on every
/// ingest while the chunk text does not. Per-revision identities are absent
/// too (chunk/document/revision ids and support block ids) because a new
/// revision reissues them regardless of content.
pub(crate) struct GraphExtractionStableContext<'a> {
    pub library_extraction_prompt: Option<&'a str>,
    pub document_label: &'a str,
    pub request_size_soft_limit_bytes: usize,
    pub chunk_kind: Option<&'a str>,
    pub section_path: &'a [String],
    pub heading_trail: &'a [String],
    pub support_block_count: usize,
    pub literal_digest: Option<&'a str>,
    pub technical_facts: &'a [GraphExtractionTechnicalFact],
}

#[must_use]
pub(crate) fn graph_extraction_stable_context<'a>(
    request: &'a GraphExtractionRequest,
    request_size_soft_limit_bytes: usize,
) -> GraphExtractionStableContext<'a> {
    GraphExtractionStableContext {
        library_extraction_prompt: request.library_extraction_prompt.as_deref(),
        document_label: prompt::graph_extraction_document_label(&request.document),
        request_size_soft_limit_bytes,
        chunk_kind: request.structured_chunk.chunk_kind.as_deref(),
        section_path: &request.structured_chunk.section_path,
        heading_trail: &request.structured_chunk.heading_trail,
        support_block_count: request.structured_chunk.support_block_ids.len(),
        literal_digest: request.structured_chunk.literal_digest.as_deref(),
        technical_facts: &request.technical_facts,
    }
}

/// Stable semantic cache identity of one extraction contract (IRG-197).
///
/// It hashes exactly what decides whether a stored output is still valid:
/// the static instruction template, the output JSON schema, the contract
/// version, the provider request profile, and the stable request context that
/// shapes the prompt — the library extraction prompt digest, the document
/// label, the segmentation size limit, and the rendered chunk structure
/// (`chunk_kind`, section path, heading trail, support block count, literal
/// digest, technical facts). It deliberately excludes the rendered prompt and
/// therefore the per-library `sub_type_hints` vocabulary, their occurrence
/// counts, and the chunk ordinal, which drift on every ingest even when the
/// chunk text does not. The full rendered prompt hash is still persisted, but
/// only for audit ([`graph_extraction_prompt_hash`]).
#[must_use]
pub(crate) fn graph_extraction_contract_hash(
    request: &GraphExtractionRequest,
    runtime_binding: &ResolvedRuntimeBinding,
    classifier: GraphEntityTypeClassifierIdentity<'_>,
    request_size_soft_limit_bytes: usize,
) -> String {
    graph_extraction_contract_hash_with(
        prompt::GRAPH_EXTRACTION_CONTRACT_VERSION,
        &prompt::graph_extraction_static_sections(),
        &prompt::graph_extraction_response_format(),
        &graph_extraction_stable_context(request, request_size_soft_limit_bytes),
        runtime_binding,
        classifier,
    )
}

pub(crate) fn graph_extraction_contract_hash_with(
    contract_version: &str,
    static_sections: &[(String, String)],
    response_format: &serde_json::Value,
    context: &GraphExtractionStableContext<'_>,
    runtime_binding: &ResolvedRuntimeBinding,
    classifier: GraphEntityTypeClassifierIdentity<'_>,
) -> String {
    let mut hasher = Sha256::new();
    update_hash_str(&mut hasher, "graph_extraction_contract");
    update_hash_str(&mut hasher, prompt::GRAPH_EXTRACTION_VERSION);
    update_hash_str(&mut hasher, contract_version);
    for (title, body) in static_sections {
        update_hash_str(&mut hasher, title);
        update_hash_str(&mut hasher, body);
    }
    update_hash_json(&mut hasher, response_format);
    update_hash_str(&mut hasher, &runtime_binding.provider_kind);
    update_hash_opt_str(&mut hasher, runtime_binding.provider_base_url.as_deref());
    update_hash_str(&mut hasher, &runtime_binding.provider_api_style);
    update_hash_str(&mut hasher, &runtime_binding.model_name);
    update_hash_opt_str(&mut hasher, runtime_binding.system_prompt.as_deref());
    update_hash_opt_f64(&mut hasher, runtime_binding.temperature);
    update_hash_opt_f64(&mut hasher, runtime_binding.top_p);
    update_hash_opt_i32(&mut hasher, runtime_binding.max_output_tokens_override);
    update_hash_json(&mut hasher, &runtime_binding.extra_parameters_json);
    // The optional entity-typing classifier changes the persisted node types
    // and therefore the identity keys, so its provider profile is part of the
    // extraction cache identity. `off` is hashed as an explicit marker.
    update_hash_classifier_identity(
        &mut hasher,
        classifier,
        &entity_type::entity_type_request_contract(),
    );
    // Library and structure inputs, matching exactly the sections the prompt
    // renders for them. The library prompt is trimmed because that is the form
    // the prompt carries.
    update_hash_opt_str(
        &mut hasher,
        context.library_extraction_prompt.map(str::trim).filter(|value| !value.is_empty()),
    );
    update_hash_str(&mut hasher, context.document_label);
    hasher.update(context.request_size_soft_limit_bytes.to_le_bytes());
    update_hash_opt_str(&mut hasher, context.chunk_kind);
    hash_str_sequence(&mut hasher, context.section_path);
    hash_str_sequence(&mut hasher, context.heading_trail);
    hasher.update(context.support_block_count.to_le_bytes());
    update_hash_opt_str(&mut hasher, context.literal_digest);
    hash_graph_technical_facts(&mut hasher, context.technical_facts);
    hex::encode(hasher.finalize())
}

/// Hashes the typing classifier into an extraction cache identity: the
/// explicit off marker, or the binding's secret-free provider profile plus the
/// classifier request contract, so either a different model or a different
/// request invalidates node types cached under the old ones.
fn update_hash_classifier_identity(
    hasher: &mut Sha256,
    classifier: GraphEntityTypeClassifierIdentity<'_>,
    request_contract: &[String],
) {
    match classifier {
        GraphEntityTypeClassifierIdentity::Off => {
            update_hash_str(hasher, "graph_entity_type=off");
        }
        GraphEntityTypeClassifierIdentity::On(binding) => {
            update_hash_str(hasher, "graph_entity_type=on");
            update_hash_str(hasher, &binding.provider_kind);
            update_hash_opt_str(hasher, binding.provider_base_url.as_deref());
            update_hash_str(hasher, &binding.provider_api_style);
            update_hash_str(hasher, &binding.model_name);
            hash_str_sequence(hasher, request_contract);
        }
    }
}

fn hash_str_sequence(hasher: &mut Sha256, values: &[String]) {
    hasher.update(values.len().to_le_bytes());
    for value in values {
        update_hash_str(hasher, value);
    }
}

/// The classifier cache identity of one extraction runtime context: the
/// resolved `graph_entity_type` binding when the task resolved one, otherwise
/// the explicit off marker. Resolving it once (in
/// [`crate::services::ingest::runtime::resolve_runtime_task_execution_context`])
/// and reusing the value keeps the fingerprint and the paid call on one state.
#[must_use]
pub(crate) fn graph_entity_type_classifier_identity(
    runtime_context: &RuntimeTaskExecutionContext,
) -> GraphEntityTypeClassifierIdentity<'_> {
    match runtime_context.entity_type_binding.as_ref() {
        Some(binding) => GraphEntityTypeClassifierIdentity::On(binding),
        None => GraphEntityTypeClassifierIdentity::Off,
    }
}

/// Hashes technical facts in a canonical order (fact, then qualifier key and
/// value), so a different SQL row order for the same facts cannot move the
/// cache identity.
fn hash_graph_technical_facts(hasher: &mut Sha256, facts: &[GraphExtractionTechnicalFact]) {
    let mut rows = facts
        .iter()
        .map(|fact| {
            let mut qualifiers = fact
                .qualifiers
                .iter()
                .map(|qualifier| (qualifier.key.clone(), qualifier.value.clone()))
                .collect::<Vec<_>>();
            qualifiers.sort();
            (
                fact.fact_kind.clone(),
                fact.normalized_value.clone(),
                fact.display_value.clone(),
                qualifiers,
            )
        })
        .collect::<Vec<_>>();
    rows.sort();
    hasher.update(rows.len().to_le_bytes());
    for (fact_kind, normalized_value, display_value, qualifiers) in rows {
        update_hash_str(hasher, &fact_kind);
        update_hash_str(hasher, &normalized_value);
        update_hash_str(hasher, &display_value);
        hasher.update(qualifiers.len().to_le_bytes());
        for (key, value) in qualifiers {
            update_hash_str(hasher, &key);
            update_hash_str(hasher, &value);
        }
    }
}

/// Hash of the full rendered prompt plus the provider request profile.
///
/// Persisted as `runtime_graph_extraction.prompt_hash` for audit only; it is no
/// longer a cache key, so prompt wording, hint counts and chunk ordinals are
/// free to change without invalidating a semantically valid stored output.
#[must_use]
pub(crate) fn graph_extraction_prompt_hash(
    prompt: &str,
    runtime_binding: &ResolvedRuntimeBinding,
) -> String {
    let mut hasher = Sha256::new();
    update_hash_str(&mut hasher, "graph_extraction_prompt");
    update_hash_str(&mut hasher, prompt::GRAPH_EXTRACTION_VERSION);
    update_hash_str(&mut hasher, prompt);
    update_hash_str(&mut hasher, &runtime_binding.provider_kind);
    update_hash_opt_str(&mut hasher, runtime_binding.provider_base_url.as_deref());
    update_hash_str(&mut hasher, &runtime_binding.provider_api_style);
    update_hash_str(&mut hasher, &runtime_binding.model_name);
    update_hash_opt_str(&mut hasher, runtime_binding.system_prompt.as_deref());
    update_hash_opt_f64(&mut hasher, runtime_binding.temperature);
    update_hash_opt_f64(&mut hasher, runtime_binding.top_p);
    update_hash_opt_i32(&mut hasher, runtime_binding.max_output_tokens_override);
    update_hash_json(&mut hasher, &runtime_binding.extra_parameters_json);
    hex::encode(hasher.finalize())
}

/// The configured provider request size ceiling that drives prompt
/// segmentation. It is a stable cache input (IRG-197): the same chunk under a
/// different ceiling can be segmented differently, so the stored output is no
/// longer the same contract. Resolved from the same service the materialization
/// binding reads, so the lookup and the write path agree on the value.
pub(crate) fn provider_request_size_soft_limit_bytes(state: &AppState) -> usize {
    state
        .resolve_settle_blockers_services
        .provider_failure_classification
        .request_size_soft_limit_bytes()
}

fn update_hash_str(hasher: &mut Sha256, value: &str) {
    hasher.update(value.len().to_le_bytes());
    hasher.update(value.as_bytes());
}

fn update_hash_opt_str(hasher: &mut Sha256, value: Option<&str>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            update_hash_str(hasher, value);
        }
        None => hasher.update([0]),
    }
}

fn update_hash_opt_i32(hasher: &mut Sha256, value: Option<i32>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn update_hash_opt_f64(hasher: &mut Sha256, value: Option<f64>) {
    match value {
        Some(value) => {
            hasher.update([1]);
            hasher.update(value.to_bits().to_le_bytes());
        }
        None => hasher.update([0]),
    }
}

fn update_hash_json(hasher: &mut Sha256, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null => hasher.update([0]),
        serde_json::Value::Bool(value) => hasher.update([1, u8::from(*value)]),
        serde_json::Value::Number(value) => {
            hasher.update([2]);
            update_hash_str(hasher, &value.to_string());
        }
        serde_json::Value::String(value) => {
            hasher.update([3]);
            update_hash_str(hasher, value);
        }
        serde_json::Value::Array(values) => {
            hasher.update([4]);
            hasher.update(values.len().to_le_bytes());
            for item in values {
                update_hash_json(hasher, item);
            }
        }
        serde_json::Value::Object(values) => {
            hasher.update([5]);
            hasher.update(values.len().to_le_bytes());
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            for key in keys {
                update_hash_str(hasher, key);
                if let Some(item) = values.get(key) {
                    update_hash_json(hasher, item);
                }
            }
        }
    }
}

pub(crate) async fn extract_chunk_graph_candidates(
    state: &AppState,
    runtime_context: &RuntimeTaskExecutionContext,
    authority: IngestRevisionWriteAuthority,
    request: &GraphExtractionRequest,
    cancellation_token: &CancellationToken,
) -> std::result::Result<
    IngestAttemptMutationOutcome<GraphExtractionOutcome>,
    GraphExtractionExecutionError,
> {
    if cancellation_token.is_cancelled() {
        return Err(graph_extraction_cancelled_error(
            request,
            format!("{}:cancelled", prompt::GRAPH_EXTRACTION_VERSION),
            request.chunk.content.len(),
        ));
    }
    let extraction_record_id = uuid::Uuid::now_v7();
    let mut runtime_session =
        seed_graph_extract_runtime_session(state, extraction_record_id, request, runtime_context)
            .await
            .map_err(|error| map_graph_runtime_execution_error(request, None, error))?;

    let runtime_binding = &runtime_context.runtime_binding;
    let extraction_contract_hash = graph_extraction_contract_hash(
        request,
        runtime_binding,
        graph_entity_type_classifier_identity(runtime_context),
        provider_request_size_soft_limit_bytes(state),
    );
    let create_input = repositories::CreateRuntimeGraphExtractionRecordInput {
        id: extraction_record_id,
        runtime_execution_id: runtime_session.execution.id,
        library_id: request.library_id,
        document_id: request.document.id,
        chunk_id: request.chunk.id,
        provider_kind: runtime_binding.provider_kind.clone(),
        model_name: runtime_binding.model_name.clone(),
        extraction_version: prompt::GRAPH_EXTRACTION_VERSION.to_string(),
        prompt_hash: "pending".to_string(),
        status: "processing".to_string(),
        raw_output_json: serde_json::json!({
            "lifecycle": {
                "revision_id": authority.revision_id,
                "activated_by_attempt_id": authority.attempt_id,
            },
            "extraction_contract_hash": extraction_contract_hash,
        }),
        normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
        glean_pass_count: 0,
        error_message: None,
    };
    let create_result = retry_graph_persistence(|| {
        repositories::runtime_graph_repository::create_runtime_graph_extraction_record_fenced(
            &state.persistence.postgres,
            authority,
            &create_input,
        )
    })
    .await;
    // The insert carries a fixed extraction_record_id. A retry after a commit
    // whose acknowledgement was lost fails with a unique violation even though
    // the owner record now exists, so adopt the stored row instead of failing
    // the whole revision's graph extraction. The adoption read is retried too:
    // a transient read failure must surface as itself, not be masked as the
    // permanent conflict it was meant to resolve.
    let create_result = match create_result {
        Ok(outcome) => Ok(outcome),
        Err(conflict) if crate::shared::database::error_is_unique_violation(&conflict) => {
            let existing = retry_graph_persistence(|| {
                repositories::runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
                    &state.persistence.postgres,
                    extraction_record_id,
                )
            })
            .await;
            resolve_unique_conflict(existing, conflict).map(IngestAttemptMutationOutcome::Applied)
        }
        Err(error) => Err(error),
    };
    let processing_record = create_result.map_err(|error| {
        graph_extraction_execution_error(
            request,
            format!("failed to create graph extraction owner record: {error:#}"),
            None,
            ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            },
            Vec::new(),
        )
    })?;
    if matches!(processing_record, IngestAttemptMutationOutcome::AuthorityLost) {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }

    let execution_result = run_graph_extraction_runtime(
        state,
        runtime_context,
        request,
        extraction_record_id,
        &mut runtime_session,
        cancellation_token,
    )
    .await;

    match execution_result {
        Ok((runtime_outcome, mut extraction_outcome)) => {
            let runtime_result = state
                .agent_runtime
                .executor()
                .finalize_session::<GraphExtractTask>(runtime_session, runtime_outcome)
                .await;
            runtime_persistence::persist_runtime_result(
                &state.persistence.postgres,
                &runtime_result.execution,
                &runtime_result.trace,
            )
            .await
            .map_err(|error| {
                graph_extraction_execution_error(
                    request,
                    format!("failed to persist graph extraction runtime trace: {error:#}"),
                    extraction_outcome.provider_failure.clone(),
                    extraction_outcome.recovery_summary.clone(),
                    extraction_outcome.recovery_attempts.clone(),
                )
            })?;
            extraction_outcome.normalized =
                parse::repair_graph_extraction_candidate_set(extraction_outcome.normalized);
            if parse::graph_extraction_candidate_set_contains_encoding_damage(
                &extraction_outcome.normalized,
            ) {
                return fail_graph_extraction_encoding_damage(
                    state,
                    authority,
                    request,
                    extraction_record_id,
                    extraction_outcome,
                )
                .await;
            }
            complete_graph_extraction_success(
                state,
                authority,
                request,
                extraction_record_id,
                runtime_result.execution.id,
                extraction_outcome,
            )
            .await
        }
        Err((runtime_outcome, error)) => {
            persist_graph_extraction_failure(
                state,
                authority,
                request,
                extraction_record_id,
                FailedGraphExtractionAttempt { runtime_session, runtime_outcome, error },
            )
            .await
        }
    }
}

/// Fails the owner record when normalization repair left encoding damage in the
/// candidate set. Writes the failed record fenced, honours a lost authority, and
/// returns the typed execution error otherwise.
async fn fail_graph_extraction_encoding_damage(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    request: &GraphExtractionRequest,
    extraction_record_id: uuid::Uuid,
    extraction_outcome: GraphExtractionOutcome,
) -> std::result::Result<
    IngestAttemptMutationOutcome<GraphExtractionOutcome>,
    GraphExtractionExecutionError,
> {
    let message =
        "graph extraction retained encoding damage after normalization repair".to_string();
    tracing::error!(extraction_record_id = %extraction_record_id, "{}", message);
    let encoding_failure_update = repositories::UpdateRuntimeGraphExtractionRecordInput {
        provider_kind: extraction_outcome.provider_kind.clone(),
        model_name: extraction_outcome.model_name.clone(),
        prompt_hash: extraction_outcome.prompt_hash.clone(),
        status: "failed".to_string(),
        raw_output_json: extraction_outcome.raw_output_json.clone(),
        normalized_output_json: serde_json::json!({
            "entities": [],
            "relations": []
        }),
        glean_pass_count: i32::try_from(extraction_outcome.usage_calls.len()).unwrap_or(i32::MAX),
        error_message: Some(message.clone()),
    };
    let update = retry_graph_persistence(|| {
        repositories::runtime_graph_repository::update_runtime_graph_extraction_record_safe_fenced(
            &state.persistence.postgres,
            authority,
            extraction_record_id,
            &encoding_failure_update,
        )
    })
    .await
    .map_err(|error| {
        graph_extraction_execution_error(
            request,
            format!("failed to persist graph extraction encoding failure: {error:#}"),
            extraction_outcome.provider_failure.clone(),
            extraction_outcome.recovery_summary.clone(),
            extraction_outcome.recovery_attempts.clone(),
        )
    })?;
    if matches!(update, IngestAttemptMutationOutcome::AuthorityLost) {
        return Ok(IngestAttemptMutationOutcome::AuthorityLost);
    }
    Err(graph_extraction_execution_error(
        request,
        message,
        extraction_outcome.provider_failure.clone(),
        extraction_outcome.recovery_summary.clone(),
        extraction_outcome.recovery_attempts.clone(),
    ))
}

/// Marks the owner record ready with the normalized candidate set and links the
/// runtime execution. Honours a lost authority and a missing owner record.
async fn complete_graph_extraction_success(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    request: &GraphExtractionRequest,
    extraction_record_id: uuid::Uuid,
    runtime_execution_id: uuid::Uuid,
    extraction_outcome: GraphExtractionOutcome,
) -> std::result::Result<
    IngestAttemptMutationOutcome<GraphExtractionOutcome>,
    GraphExtractionExecutionError,
> {
    let normalized_output_json =
        parse::normalized_graph_extraction_json(extraction_outcome.normalized.clone());
    let ready_update = repositories::UpdateRuntimeGraphExtractionRecordInput {
        provider_kind: extraction_outcome.provider_kind.clone(),
        model_name: extraction_outcome.model_name.clone(),
        prompt_hash: extraction_outcome.prompt_hash.clone(),
        status: "ready".to_string(),
        raw_output_json: extraction_outcome.raw_output_json.clone(),
        normalized_output_json,
        glean_pass_count: i32::try_from(extraction_outcome.usage_calls.len()).unwrap_or(i32::MAX),
        error_message: None,
    };
    let update = retry_graph_persistence(|| {
        repositories::runtime_graph_repository::update_runtime_graph_extraction_record_safe_fenced(
            &state.persistence.postgres,
            authority,
            extraction_record_id,
            &ready_update,
        )
    })
    .await
    .map_err(|error| {
        graph_extraction_execution_error(
            request,
            format!("failed to update graph extraction owner record: {error:#}"),
            extraction_outcome.provider_failure.clone(),
            extraction_outcome.recovery_summary.clone(),
            extraction_outcome.recovery_attempts.clone(),
        )
    })?;
    let updated = match update {
        IngestAttemptMutationOutcome::Applied(updated) => updated,
        IngestAttemptMutationOutcome::AuthorityLost => {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
    };
    updated.ok_or_else(|| {
        graph_extraction_execution_error(
            request,
            format!(
                "graph extraction owner record {extraction_record_id} was not found during update"
            ),
            extraction_outcome.provider_failure.clone(),
            extraction_outcome.recovery_summary.clone(),
            extraction_outcome.recovery_attempts.clone(),
        )
    })?;
    Ok(IngestAttemptMutationOutcome::Applied(GraphExtractionOutcome {
        graph_extraction_id: Some(extraction_record_id),
        runtime_execution_id: Some(runtime_execution_id),
        ..extraction_outcome
    }))
}

struct FailedGraphExtractionAttempt {
    runtime_session: RuntimeExecutionSession,
    runtime_outcome: RuntimeTerminalOutcome<
        types::GraphExtractionCandidateSet,
        types::GraphExtractionTaskFailure,
    >,
    error: Box<GraphExtractionExecutionError>,
}

/// Persists the terminal failure of one graph extraction attempt: the runtime
/// trace, the failed owner record, and the policy audit when the runtime
/// canceled the task. The typed execution error is returned unchanged.
async fn persist_graph_extraction_failure(
    state: &AppState,
    authority: IngestRevisionWriteAuthority,
    request: &GraphExtractionRequest,
    extraction_record_id: uuid::Uuid,
    failure: FailedGraphExtractionAttempt,
) -> std::result::Result<
    IngestAttemptMutationOutcome<GraphExtractionOutcome>,
    GraphExtractionExecutionError,
> {
    let FailedGraphExtractionAttempt { runtime_session, runtime_outcome, error } = failure;
    let runtime_result = state
        .agent_runtime
        .executor()
        .finalize_session::<GraphExtractTask>(runtime_session, runtime_outcome)
        .await;
    runtime_persistence::persist_runtime_result(
        &state.persistence.postgres,
        &runtime_result.execution,
        &runtime_result.trace,
    )
    .await
    .map_err(|persist_error| GraphExtractionExecutionError {
        message: format!("failed to persist graph extraction runtime trace: {persist_error:#}"),
        request_shape_key: error.request_shape_key.clone(),
        request_size_bytes: error.request_size_bytes,
        provider_failure: error.provider_failure.clone(),
        provider_admission_error: error.provider_admission_error.clone(),
        recovery_summary: error.recovery_summary.clone(),
        recovery_attempts: error.recovery_attempts.clone(),
        resume_state: error.resume_state.clone(),
        cancelled: error.cancelled,
    })?;
    let failure_update = repositories::UpdateRuntimeGraphExtractionRecordInput {
        provider_kind: error
            .provider_failure
            .as_ref()
            .and_then(|failure| failure.provider_kind.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        model_name: error
            .provider_failure
            .as_ref()
            .and_then(|failure| failure.model_name.clone())
            .unwrap_or_else(|| "unknown".to_string()),
        prompt_hash: "unknown".to_string(),
        status: graph_async_operation_status(&runtime_result.outcome).to_string(),
        raw_output_json: serde_json::json!({}),
        normalized_output_json: serde_json::json!({ "entities": [], "relations": [] }),
        glean_pass_count: i32::try_from(error.resume_state.replay_count).unwrap_or(i32::MAX),
        error_message: Some(error.message.clone()),
    };
    let update = retry_graph_persistence(|| {
        repositories::runtime_graph_repository::update_runtime_graph_extraction_record_safe_fenced(
            &state.persistence.postgres,
            authority,
            extraction_record_id,
            &failure_update,
        )
    })
    .await
    .map_err(|persist_error| GraphExtractionExecutionError {
        message: format!("failed to update graph extraction failure record: {persist_error:#}"),
        request_shape_key: error.request_shape_key.clone(),
        request_size_bytes: error.request_size_bytes,
        provider_failure: error.provider_failure.clone(),
        provider_admission_error: error.provider_admission_error.clone(),
        recovery_summary: error.recovery_summary.clone(),
        recovery_attempts: error.recovery_attempts.clone(),
        resume_state: error.resume_state.clone(),
        cancelled: error.cancelled,
    })?;
    let updated = match update {
        IngestAttemptMutationOutcome::Applied(updated) => updated,
        IngestAttemptMutationOutcome::AuthorityLost => {
            return Ok(IngestAttemptMutationOutcome::AuthorityLost);
        }
    };
    updated.ok_or_else(|| GraphExtractionExecutionError {
        message: format!(
            "graph extraction owner record {extraction_record_id} was not found during failure update"
        ),
        request_shape_key: error.request_shape_key.clone(),
        request_size_bytes: error.request_size_bytes,
        provider_failure: error.provider_failure.clone(),
        provider_admission_error: error.provider_admission_error.clone(),
        recovery_summary: error.recovery_summary.clone(),
        recovery_attempts: error.recovery_attempts.clone(),
        resume_state: error.resume_state.clone(),
        cancelled: error.cancelled,
    })?;
    append_graph_runtime_policy_audit(state, request, extraction_record_id, &runtime_result).await;
    Err(*error)
}

async fn seed_graph_extract_runtime_session(
    state: &AppState,
    graph_extraction_id: uuid::Uuid,
    request: &GraphExtractionRequest,
    runtime_context: &RuntimeTaskExecutionContext,
) -> std::result::Result<
    RuntimeExecutionSession,
    crate::agent_runtime::executor::RuntimeExecutionError,
> {
    let runtime_request = StructuredRequestBuilder::<GraphExtractTask>::new(
        GraphExtractTaskInput {
            library_id: request.library_id,
            document_id: request.document.id,
            chunk_id: request.chunk.id,
            revision_id: request.revision_id,
            normalized_text: request.chunk.content.clone(),
            technical_facts: request.technical_facts.clone(),
        },
        RuntimeExecutionOwner::graph_extraction_attempt(graph_extraction_id),
    )
    .with_budget_limits(
        runtime_context.runtime_overrides.max_turns,
        runtime_context.runtime_overrides.max_parallel_actions,
    )
    .build();

    state
        .agent_runtime
        .seed_and_persist_session(&state.persistence.postgres, &runtime_request)
        .await
}

async fn run_graph_extraction_runtime(
    state: &AppState,
    runtime_context: &RuntimeTaskExecutionContext,
    request: &GraphExtractionRequest,
    graph_extraction_id: uuid::Uuid,
    runtime_session: &mut RuntimeExecutionSession,
    cancellation_token: &CancellationToken,
) -> std::result::Result<
    (
        RuntimeTerminalOutcome<
            types::GraphExtractionCandidateSet,
            types::GraphExtractionTaskFailure,
        >,
        GraphExtractionOutcome,
    ),
    (
        RuntimeTerminalOutcome<
            types::GraphExtractionCandidateSet,
            types::GraphExtractionTaskFailure,
        >,
        Box<GraphExtractionExecutionError>,
    ),
> {
    let runtime_binding = &runtime_context.runtime_binding;

    if let Err(failure) = begin_graph_runtime_stage(
        state.agent_runtime.executor(),
        runtime_session,
        RuntimeStageKind::ExtractGraph,
    )
    .await
    {
        record_graph_runtime_stage(
            state.agent_runtime.executor(),
            runtime_session,
            GraphRuntimeStageUpdate {
                stage_kind: RuntimeStageKind::ExtractGraph,
                stage_state: RuntimeStageState::Failed,
                deterministic: false,
                failure: Some(&failure),
                started_at: None,
            },
        );
        let error = graph_extraction_execution_error(
            request,
            failure.summary.clone(),
            None,
            ExtractionRecoverySummary {
                status: ExtractionOutcomeStatus::Failed,
                second_pass_applied: false,
                warning: None,
            },
            Vec::new(),
        );
        return Err((make_graph_terminal_failure_outcome(failure), Box::new(error)));
    }

    if cancellation_token.is_cancelled() {
        let error = graph_extraction_cancelled_error(
            request,
            format!("{}:cancelled", prompt::GRAPH_EXTRACTION_VERSION),
            request.chunk.content.len(),
        );
        let failure = GraphExtractionTaskFailure {
            code: "ingest_stage_cancelled".to_string(),
            summary: error.message.clone(),
        };
        record_graph_runtime_stage(
            state.agent_runtime.executor(),
            runtime_session,
            GraphRuntimeStageUpdate {
                stage_kind: RuntimeStageKind::ExtractGraph,
                stage_state: RuntimeStageState::Canceled,
                deterministic: false,
                failure: Some(&failure),
                started_at: None,
            },
        );
        return Err((make_graph_terminal_failure_outcome(failure), Box::new(error)));
    }

    match resolve_graph_extraction(state, runtime_binding, request, cancellation_token).await {
        Ok(mut resolved) => {
            // Optional entity typing: rewrite each candidate's node type before
            // it is recorded, persisted or turned into an identity/edge key.
            classify_graph_entity_candidates_if_bound(
                state,
                runtime_context.entity_type_binding.as_ref(),
                &mut resolved.normalized,
                graph_extraction_id,
                runtime_session.execution.id,
                cancellation_token,
            )
            .await;
            record_graph_runtime_stage(
                state.agent_runtime.executor(),
                runtime_session,
                GraphRuntimeStageUpdate {
                    stage_kind: RuntimeStageKind::ExtractGraph,
                    stage_state: RuntimeStageState::Completed,
                    deterministic: false,
                    failure: None,
                    started_at: None,
                },
            );

            let runtime_execution_id = runtime_session.execution.id;
            let recovery_status = resolved.recovery_summary.status.clone();
            if matches!(
                recovery_status,
                ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial
            ) {
                if let Err(failure) = begin_graph_runtime_stage(
                    state.agent_runtime.executor(),
                    runtime_session,
                    RuntimeStageKind::Recovery,
                )
                .await
                {
                    record_graph_runtime_stage(
                        state.agent_runtime.executor(),
                        runtime_session,
                        GraphRuntimeStageUpdate {
                            stage_kind: RuntimeStageKind::Recovery,
                            stage_state: RuntimeStageState::Failed,
                            deterministic: false,
                            failure: Some(&failure),
                            started_at: None,
                        },
                    );
                    let error = graph_extraction_execution_error(
                        request,
                        failure.summary.clone(),
                        resolved.provider_failure.clone(),
                        resolved.recovery_summary.clone(),
                        resolved.recovery_attempts.clone(),
                    );
                    return Err((make_graph_terminal_failure_outcome(failure), Box::new(error)));
                }
                record_graph_runtime_stage(
                    state.agent_runtime.executor(),
                    runtime_session,
                    GraphRuntimeStageUpdate {
                        stage_kind: RuntimeStageKind::Recovery,
                        stage_state: RuntimeStageState::Recovered,
                        deterministic: false,
                        failure: None,
                        started_at: None,
                    },
                );
            }

            let normalized = resolved.normalized.clone();
            let outcome = GraphExtractionOutcome {
                graph_extraction_id: Some(graph_extraction_id),
                runtime_execution_id: Some(runtime_execution_id),
                provider_kind: resolved.provider_kind.clone(),
                model_name: resolved.model_name.clone(),
                prompt_hash: resolved.prompt_hash.clone(),
                raw_output_json: build_raw_output_json(GraphExtractionRawOutput {
                    output_text: &resolved.output_text,
                    usage_json: resolved.usage_json.clone(),
                    lifecycle: &resolved.lifecycle,
                    recovery: &resolved.recovery,
                    recovery_summary: &resolved.recovery_summary,
                    usage_calls: &resolved.usage_calls,
                    extraction_contract_hash: &graph_extraction_contract_hash(
                        request,
                        runtime_binding,
                        graph_entity_type_classifier_identity(runtime_context),
                        provider_request_size_soft_limit_bytes(state),
                    ),
                }),
                usage_json: resolved.usage_json.clone(),
                usage_calls: resolved.usage_calls.clone(),
                normalized: resolved.normalized,
                provider_failure: resolved.provider_failure.clone(),
                recovery_summary: resolved.recovery_summary.clone(),
                recovery_attempts: resolved.recovery_attempts.clone(),
            };

            let runtime_outcome = match recovery_status {
                ExtractionOutcomeStatus::Clean => {
                    RuntimeTerminalOutcome::Completed { success: normalized }
                }
                ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial => {
                    RuntimeTerminalOutcome::Recovered {
                        success: normalized,
                        recovery: RuntimeRecoveryOutcome {
                            attempts: u8::try_from(outcome.recovery_attempts.len())
                                .unwrap_or(u8::MAX),
                            summary_redacted: outcome.recovery_summary.warning.clone(),
                        },
                    }
                }
                ExtractionOutcomeStatus::Failed => RuntimeTerminalOutcome::Failed {
                    failure: GraphExtractionTaskFailure {
                        code: GraphExtractionTaskFailureCode::MalformedOutput.as_str().to_string(),
                        summary: "graph extraction resolved with failed recovery status"
                            .to_string(),
                    },
                    summary: make_graph_runtime_failure_summary(
                        GraphExtractionTaskFailureCode::MalformedOutput.as_str(),
                    ),
                },
            };
            match runtime_outcome {
                RuntimeTerminalOutcome::Failed { failure, summary } => Err((
                    RuntimeTerminalOutcome::Failed { failure, summary },
                    Box::new(graph_extraction_execution_error(
                        request,
                        "graph extraction resolved with failed recovery status",
                        outcome.provider_failure.clone(),
                        outcome.recovery_summary.clone(),
                        outcome.recovery_attempts,
                    )),
                )),
                _ => Ok((runtime_outcome, outcome)),
            }
        }
        Err(failure) => {
            let stage_state = if failure.cancelled {
                RuntimeStageState::Canceled
            } else {
                RuntimeStageState::Failed
            };
            record_graph_runtime_stage(
                state.agent_runtime.executor(),
                runtime_session,
                GraphRuntimeStageUpdate {
                    stage_kind: RuntimeStageKind::ExtractGraph,
                    stage_state,
                    deterministic: false,
                    failure: Some(&GraphExtractionTaskFailure {
                        code: graph_failure_code_from_outcome(&failure).to_string(),
                        summary: failure.error_message.clone(),
                    }),
                    started_at: None,
                },
            );
            let task_failure = GraphExtractionTaskFailure {
                code: graph_failure_code_from_outcome(&failure).to_string(),
                summary: failure.error_message.clone(),
            };
            Err((
                make_graph_terminal_failure_outcome(task_failure),
                Box::new(GraphExtractionExecutionError {
                    message: failure.error_message,
                    request_shape_key: failure.request_shape_key,
                    request_size_bytes: failure.request_size_bytes,
                    provider_failure: failure.provider_failure,
                    provider_admission_error: failure.provider_admission_error,
                    recovery_summary: failure.recovery_summary,
                    recovery_attempts: failure.recovery_attempts,
                    resume_state: Box::new(GraphExtractionResumeState {
                        resumed_from_checkpoint: false,
                        replay_count: request
                            .resume_hint
                            .as_ref()
                            .map_or(1, |hint| hint.replay_count.saturating_add(1)),
                        downgrade_level: normalized_downgrade_level(request),
                    }),
                    cancelled: failure.cancelled,
                }),
            ))
        }
    }
}

async fn resolve_graph_extraction(
    state: &AppState,
    runtime_binding: &ResolvedRuntimeBinding,
    request: &GraphExtractionRequest,
    cancellation_token: &CancellationToken,
) -> std::result::Result<types::ResolvedGraphExtraction, GraphExtractionFailureOutcome> {
    session::resolve_graph_extraction_with_gateway(
        state.llm_gateway.as_ref(),
        session::GraphExtractionLoopRequest {
            extraction_recovery: &state.retrieval_intelligence_services.extraction_recovery,
            provider_failure_classification: &state
                .resolve_settle_blockers_services
                .provider_failure_classification,
            runtime_binding,
            request,
            recovery_enabled: state.retrieval_intelligence.extraction_recovery_enabled,
            max_provider_attempts: state
                .retrieval_intelligence
                .extraction_recovery_max_attempts
                .clamp(1, prompt::GRAPH_EXTRACTION_MAX_PROVIDER_ATTEMPTS),
            provider_timeout_retry_limit: state
                .resolve_settle_blockers
                .provider_timeout_retry_limit
                .max(1),
        },
        cancellation_token,
    )
    .await
}
