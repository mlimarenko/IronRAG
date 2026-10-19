use super::*;

pub(super) fn merge_stages(
    rows: &[&ingest_repository::IngestStageEventRow],
) -> Vec<DocumentStageEvent> {
    let mut out: Vec<DocumentStageEvent> = Vec::new();
    for row in rows {
        if let Some(ex) = out.iter_mut().find(|s| s.stage == row.stage_name) {
            merge_stage_details(&mut ex.details, &row.details_json);
            if row.stage_state == "completed" || row.stage_state == "failed" {
                if let Some(started_at) = row.started_at
                    && started_at < ex.started_at
                {
                    ex.started_at = started_at;
                }
                ex.status = row.stage_state.clone();
                ex.finished_at = Some(row.recorded_at);
                ex.elapsed_ms = row
                    .elapsed_ms
                    .or_else(|| Some((row.recorded_at - ex.started_at).num_milliseconds()));
                ex.provider_kind = row
                    .provider_kind
                    .clone()
                    .or_else(|| {
                        row.details_json
                            .get("providerKind")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .or_else(|| ex.provider_kind.take());
                ex.model_name = row
                    .model_name
                    .clone()
                    .or_else(|| {
                        row.details_json.get("modelName").and_then(|v| v.as_str()).map(String::from)
                    })
                    .or_else(|| ex.model_name.take());
                ex.prompt_tokens = row.prompt_tokens.or(ex.prompt_tokens);
                ex.completion_tokens = row.completion_tokens.or(ex.completion_tokens);
                ex.total_tokens = row.total_tokens.or(ex.total_tokens);
                ex.estimated_cost = row.estimated_cost.or(ex.estimated_cost);
                ex.currency_code = row.currency_code.clone().or_else(|| ex.currency_code.take());
                ex.reused_chunks = row
                    .details_json
                    .get("reusedChunks")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.reused_chunks);
                ex.reused_entities = row
                    .details_json
                    .get("reusedEntities")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.reused_entities);
                ex.reused_relations = row
                    .details_json
                    .get("reusedRelations")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.reused_relations);
                ex.chunks_processed = row
                    .details_json
                    .get("chunksProcessed")
                    .and_then(serde_json::Value::as_i64)
                    .or(ex.chunks_processed);
            }
        } else {
            let started_at = row.started_at.unwrap_or(row.recorded_at);
            let is_terminal = row.stage_state == "completed" || row.stage_state == "failed";
            out.push(DocumentStageEvent {
                stage: row.stage_name.clone(),
                status: row.stage_state.clone(),
                started_at,
                finished_at: is_terminal.then_some(row.recorded_at),
                elapsed_ms: row.elapsed_ms.or_else(|| {
                    is_terminal.then_some((row.recorded_at - started_at).num_milliseconds())
                }),
                provider_kind: row.provider_kind.clone().or_else(|| {
                    row.details_json.get("providerKind").and_then(|v| v.as_str()).map(String::from)
                }),
                model_name: row.model_name.clone().or_else(|| {
                    row.details_json.get("modelName").and_then(|v| v.as_str()).map(String::from)
                }),
                prompt_tokens: row.prompt_tokens,
                completion_tokens: row.completion_tokens,
                total_tokens: row.total_tokens,
                estimated_cost: row.estimated_cost,
                currency_code: row.currency_code.clone(),
                reused_chunks: row
                    .details_json
                    .get("reusedChunks")
                    .and_then(serde_json::Value::as_i64),
                reused_entities: row
                    .details_json
                    .get("reusedEntities")
                    .and_then(serde_json::Value::as_i64),
                reused_relations: row
                    .details_json
                    .get("reusedRelations")
                    .and_then(serde_json::Value::as_i64),
                chunks_processed: row
                    .details_json
                    .get("chunksProcessed")
                    .and_then(serde_json::Value::as_i64),
                provider_call_count: None,
                details: stage_details_or_none(&row.details_json),
            });
        }
    }
    out
}

fn stage_details_or_none(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        _ => Some(value.clone()),
    }
}

fn merge_stage_details(target: &mut Option<Value>, incoming: &Value) {
    let Some(incoming) = stage_details_or_none(incoming) else {
        return;
    };

    match (target.as_mut(), incoming) {
        (Some(Value::Object(existing)), Value::Object(incoming)) => {
            for (key, value) in incoming {
                if !value.is_null() {
                    existing.insert(key, value);
                }
            }
        }
        (None, value) => {
            *target = Some(value);
        }
        (Some(_), value) => {
            *target = Some(value);
        }
    }
}

#[cfg(test)]
pub(super) fn sort_attempts_latest_first(attempts: &mut [DocumentAttempt]) {
    attempts.sort_by_key(latest_attempt_sort_key);
}

pub(super) fn sort_attempt_entries_latest_first(attempts: &mut [(Uuid, DocumentAttempt)]) {
    attempts.sort_by_key(|(_, attempt)| latest_attempt_sort_key(attempt));
}

/// Newest-attempt-first ordering key: started time, attempt number, then queue
/// start time, each reversed so `sort_by_key` yields the latest attempt first.
type LatestAttemptSortKey =
    (std::cmp::Reverse<DateTime<Utc>>, std::cmp::Reverse<i32>, std::cmp::Reverse<DateTime<Utc>>);

fn latest_attempt_sort_key(attempt: &DocumentAttempt) -> LatestAttemptSortKey {
    (
        std::cmp::Reverse(attempt.started_at.unwrap_or(attempt.queue_started_at)),
        std::cmp::Reverse(attempt.attempt_no),
        std::cmp::Reverse(attempt.queue_started_at),
    )
}
