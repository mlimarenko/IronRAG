use super::progress::{
    ingest_attempt_stage_progress_percent, ingest_attempt_stage_unit_progress_percent,
    normalize_stage_name,
};
use super::*;

impl IngestService {
    pub async fn record_stage_event(
        &self,
        state: &AppState,
        command: RecordStageEventCommand,
    ) -> Result<IngestStageEvent, ApiError> {
        let stage_name = normalize_stage_name(&command.stage_name)?;
        let stage_state = command.stage_state.clone();
        let stage_message = command.message.clone();
        let existing_events = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let stage_details = command.details_json.clone();
        let progress = ingest_attempt_stage_progress_percent(
            &existing_events,
            &stage_name,
            &stage_state,
            &stage_details,
        )
        .unwrap_or_default();
        let failure_message = if stage_state == "failed" {
            stage_message.as_deref().map(str::trim).filter(|message| !message.is_empty())
        } else {
            None
        };
        let appended = ingest_repository::append_ingest_stage_event_and_update_attempt(
            &state.persistence.postgres,
            &NewIngestStageEvent {
                attempt_id: command.attempt_id,
                stage_name: stage_name.clone(),
                stage_state: command.stage_state,
                ordinal: i32::try_from(existing_events.len()).unwrap_or(i32::MAX) + 1,
                message: command.message,
                details_json: command.details_json,
                recorded_at: None,
                provider_kind: command.provider_kind,
                model_name: command.model_name,
                prompt_tokens: command.prompt_tokens,
                completion_tokens: command.completion_tokens,
                total_tokens: command.total_tokens,
                cached_tokens: command.cached_tokens,
                estimated_cost: command.estimated_cost,
                currency_code: command.currency_code,
                elapsed_ms: command.elapsed_ms,
            },
            &stage_name,
            Utc::now(),
            failure_message,
            progress,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some((row, _attempt)) = appended else {
            return Err(ApiError::resource_not_found("active ingest_attempt", command.attempt_id));
        };
        Ok(map_stage_event_row(row))
    }

    pub async fn record_stage_unit_progress(
        &self,
        state: &AppState,
        command: RecordStageUnitProgressCommand,
    ) -> Result<(), ApiError> {
        let stage_name = normalize_stage_name(&command.stage_name)?;
        if command.total_units == 0 {
            return Ok(());
        }
        let attempt = ingest_repository::get_ingest_attempt_by_id(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("ingest_attempt", command.attempt_id))?;
        if attempt.attempt_state != "leased" {
            return Ok(());
        }
        let existing_events = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            command.attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let Some(progress_percent) = ingest_attempt_stage_unit_progress_percent(
            &existing_events,
            &stage_name,
            command.completed_units,
            command.total_units,
            &command.details_json,
        ) else {
            return Ok(());
        };
        let _ = ingest_repository::update_leased_attempt_stage_progress(
            &state.persistence.postgres,
            command.attempt_id,
            &stage_name,
            progress_percent,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(())
    }

    pub async fn list_stage_events(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<Vec<IngestStageEvent>, ApiError> {
        let rows = ingest_repository::list_ingest_stage_events_by_attempt(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_stage_event_row).collect())
    }
}
