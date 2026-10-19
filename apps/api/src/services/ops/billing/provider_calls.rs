use super::*;

/// Validates the terminal state an operator or reconciliation path may write
/// for a reservation that produced no usage.
fn reserved_terminal_call_state(call_state: &str) -> Result<(), ApiError> {
    if matches!(call_state, "failed" | "canceled") {
        return Ok(());
    }
    Err(ApiError::BadRequest(
        "reserved provider call terminal state must be failed or canceled".to_string(),
    ))
}

/// Resolves provider usage semantics from the catalog profile, falling back to
/// formal payload-shape detection when the catalog row is missing.
fn resolve_configured_usage_semantics(
    provider_call: &billing_repository::BillingProviderCallRow,
    provider_profile: Option<&ai_repository::ProviderBillingUsageProfileRow>,
) -> Result<ProviderUsageSemantics, ApiError> {
    let Some(profile) = provider_profile else {
        // The provider-call foreign key should make this impossible. Keep
        // usage durable if the catalog is nevertheless damaged, and use
        // formal payload shape instead of guessing from an identifier.
        tracing::error!(
            provider_call_id = %provider_call.id,
            provider_catalog_id = %provider_call.provider_catalog_id,
            "billing provider usage profile is missing"
        );
        return Ok(ProviderUsageSemantics::AutoDetect);
    };
    let policy =
        parse_provider_usage_policy(profile.usage_policy_json.as_ref()).map_err(|error| {
            ApiError::internal_with_log(error, "invalid typed provider usage policy in catalog")
        })?;
    Ok(configured_provider_usage_semantics(policy))
}

/// Logs when a provider reported more cached input tokens than total input and
/// the billed quantity had to be clamped.
fn warn_if_cached_input_clamped(
    provider_call: &billing_repository::BillingProviderCallRow,
    normalized_usage: &NormalizedTokenUsage,
) {
    if normalized_usage.cached_input_was_clamped {
        tracing::warn!(
            provider_call_id = %provider_call.id,
            "provider reported more cached input tokens than total input tokens; billing quantity was clamped"
        );
    }
}

impl BillingService {
    /// Creates a durable `started` provider-call row before a paid request is
    /// issued. Callers must complete or fail the returned reservation.
    ///
    /// # Errors
    /// Returns an [`ApiError`] when attribution is invalid or the reservation
    /// cannot be persisted. A caller must not issue the provider request after
    /// an error.
    pub async fn reserve_execution_provider_call(
        &self,
        state: &AppState,
        command: ReserveExecutionProviderCallCommand,
    ) -> Result<Uuid, ApiError> {
        self.reserve_execution_provider_call_with_id(state, Uuid::now_v7(), command).await
    }

    /// Persists a reservation using a caller-known ID. Optional latency-bound
    /// stages use this form so an ambiguous timeout can always be reconciled
    /// without losing ownership of a committed `started` row.
    pub async fn reserve_execution_provider_call_with_id(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        command: ReserveExecutionProviderCallCommand,
    ) -> Result<Uuid, ApiError> {
        let owning_execution_kind = parse_execution_owner_kind(&command.owning_execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(&command.owning_execution_kind))?;
        self.validate_runtime_attribution(
            state,
            owning_execution_kind,
            command.owning_execution_id,
            command.runtime_execution_id,
            command.runtime_task_kind,
        )
        .await?;
        let execution_scope = self
            .resolve_execution_scope(state, owning_execution_kind, command.owning_execution_id)
            .await?;
        if execution_scope.workspace_id != command.workspace_id
            || execution_scope.library_id != command.library_id
        {
            return Err(ApiError::Conflict(format!(
                "provider call attribution does not match execution {} scope",
                command.owning_execution_id
            )));
        }

        let new_provider_call = billing_repository::NewBillingProviderCall {
            id: provider_call_id,
            workspace_id: command.workspace_id,
            library_id: command.library_id,
            binding_id: command.binding_id,
            owning_execution_kind: execution_owner_kind_key(owning_execution_kind),
            owning_execution_id: command.owning_execution_id,
            runtime_execution_id: command.runtime_execution_id,
            runtime_task_kind: command.runtime_task_kind.map(RuntimeTaskKind::as_str),
            provider_catalog_id: command.provider_catalog_id,
            model_catalog_id: command.model_catalog_id,
            call_kind: &command.call_kind,
            call_state: "started",
            completed_at: None,
        };
        let provider_call = match billing_repository::create_provider_call(
            &state.persistence.postgres,
            &new_provider_call,
        )
        .await
        {
            Ok(provider_call) => provider_call,
            Err(error) if database_error_is_unique_violation(&error) => {
                let existing = billing_repository::get_provider_call_by_id(
                    &state.persistence.postgres,
                    provider_call_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::Conflict(format!(
                        "provider call {provider_call_id} reservation conflict disappeared"
                    ))
                })?;
                if !provider_call_matches_reservation(&existing, &new_provider_call)
                    || existing.call_state != "started"
                {
                    return Err(ApiError::Conflict(format!(
                        "provider call {provider_call_id} already belongs to a different or terminal event"
                    )));
                }
                existing
            }
            Err(error) => return Err(ApiError::internal_with_log(error, "internal")),
        };
        Ok(provider_call.id)
    }

    /// Cancels a known reservation when it exists, and is a no-op when an
    /// interrupted INSERT never committed. This is intended for asynchronous
    /// reconciliation after a deadline or task cancellation.
    pub async fn cancel_reserved_provider_call_if_present(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
    ) -> Result<bool, ApiError> {
        let exists = billing_repository::get_provider_call_by_id(
            &state.persistence.postgres,
            provider_call_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .is_some();
        if !exists {
            return Ok(false);
        }
        self.finish_reserved_provider_call_without_usage(state, provider_call_id, "canceled")
            .await?;
        Ok(true)
    }

    /// Atomically attaches provider-reported usage and charges to a durable
    /// reservation and marks it completed. Any pre-commit error or cancellation
    /// rolls the whole transaction back to a clean, retryable `started` row.
    pub async fn complete_reserved_provider_call(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let provider_call = self
            .persist_reserved_provider_call_completion(state, provider_call_id, usage_json)
            .await?;
        match self
            .roll_up_execution_cost_with_retry(
                state,
                &provider_call.owning_execution_kind,
                provider_call.owning_execution_id,
            )
            .await
        {
            Ok(rollup) => Ok(rollup),
            Err(error) => {
                // Usage, charges, and the completed state are authoritative and
                // already committed. The derived row received bounded retries;
                // do not turn a valid provider response into a query fallback
                // after the point of commit.
                tracing::error!(
                    %provider_call_id,
                    owning_execution_kind = %provider_call.owning_execution_kind,
                    owning_execution_id = %provider_call.owning_execution_id,
                    %error,
                    "provider call completed but bounded execution-cost rollup repair failed"
                );
                Ok(None)
            }
        }
    }

    /// Completes provider-call accounting, then repairs the derived cost
    /// rollup off the latency-sensitive request path.
    ///
    /// The provider-call completion transaction also increments a durable rollup
    /// generation. Reads fail closed while it is dirty and the bounded worker
    /// repair sweep retries it after crashes. The detached task below is only
    /// a latency optimization, never the correctness mechanism, so optional
    /// query stages do not wait for derived aggregate work.
    pub async fn complete_reserved_provider_call_deferred_rollup(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<(), ApiError> {
        let provider_call = self
            .persist_reserved_provider_call_completion(state, provider_call_id, usage_json)
            .await?;
        let service = self.clone();
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(error) = service
                .roll_up_execution_cost_with_retry(
                    &state,
                    &provider_call.owning_execution_kind,
                    provider_call.owning_execution_id,
                )
                .await
            {
                tracing::error!(
                    %provider_call_id,
                    owning_execution_kind = %provider_call.owning_execution_kind,
                    owning_execution_id = %provider_call.owning_execution_id,
                    %error,
                    "deferred execution-cost rollup repair failed after active provider completion"
                );
            }
        });
        Ok(())
    }

    /// Completes one caller-stable provider event with bounded local retries.
    ///
    /// The provider-call completion transaction is idempotent by provider-call ID,
    /// so a retry after a pre-commit failure or an ambiguous commit
    /// acknowledgement cannot duplicate usage or charges. This protects
    /// in-process response handling only; it does not claim provider-side
    /// exactly-once execution or survive loss of the known usage payload in a
    /// process crash.
    ///
    /// # Errors
    ///
    /// Returns the final non-retryable error or the last retryable error after
    /// the bounded retry budget is exhausted.
    pub async fn complete_reserved_provider_call_deferred_rollup_with_retry(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<(), ApiError> {
        let mut retry_index = 0usize;
        loop {
            match self
                .complete_reserved_provider_call_deferred_rollup(
                    state,
                    provider_call_id,
                    usage_json,
                )
                .await
            {
                Ok(()) => return Ok(()),
                Err(error)
                    if provider_call_completion_error_is_retryable(&error)
                        && retry_index < PROVIDER_CALL_COMPLETION_RETRY_DELAYS_MS.len() =>
                {
                    let delay_ms = PROVIDER_CALL_COMPLETION_RETRY_DELAYS_MS[retry_index];
                    retry_index += 1;
                    tracing::warn!(
                        %provider_call_id,
                        attempt = retry_index + 1,
                        delay_ms,
                        %error,
                        "retrying tracked provider-call completion"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    pub(super) async fn persist_reserved_provider_call_completion(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        usage_json: &Value,
    ) -> Result<billing_repository::BillingProviderCallRow, ApiError> {
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // Serialize completion attempts and keep usage, charges, and the state
        // transition in one transaction. On any error or task cancellation,
        // dropping the transaction rolls every partial insert back, leaving a
        // clean `started` reservation that is safe for an operator retry.
        let provider_call = billing_repository::get_provider_call_by_id_for_update(
            &mut *transaction,
            provider_call_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("billing_provider_call", provider_call_id))?;

        // Retrying the same stable local event after an ambiguous commit must
        // never duplicate usage or charges. A completed row is the durable
        // acknowledgement for the original completion attempt.
        if provider_call.call_state == "completed" {
            transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            return Ok(provider_call);
        }
        if provider_call.call_state != "started" {
            return Err(ApiError::Conflict(format!(
                "provider call {provider_call_id} is already {}",
                provider_call.call_state
            )));
        }

        self.persist_provider_call_usage_in_transaction(
            &mut transaction,
            &provider_call,
            usage_json,
        )
        .await?;
        billing_repository::update_provider_call_state(
            &mut *transaction,
            provider_call_id,
            "completed",
            Some(Utc::now()),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| {
            ApiError::Conflict(format!(
                "provider call {provider_call_id} was finalized concurrently"
            ))
        })?;
        billing_repository::mark_execution_cost_rollup_dirty(&mut *transaction, &provider_call)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(provider_call)
    }

    /// Marks a reserved call failed or canceled when no response usage exists.
    pub async fn finish_reserved_provider_call_without_usage(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        call_state: &str,
    ) -> Result<(), ApiError> {
        reserved_terminal_call_state(call_state)?;
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let existing = billing_repository::get_provider_call_by_id_for_update(
            &mut *transaction,
            provider_call_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("billing_provider_call", provider_call_id))?;
        let (provider_call, transitioned) = if existing.call_state == "started" {
            let provider_call = billing_repository::update_provider_call_state(
                &mut *transaction,
                provider_call_id,
                call_state,
                Some(Utc::now()),
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "provider call {provider_call_id} could not be terminalized"
                ))
            })?;
            (provider_call, true)
        } else {
            // A concurrent completion/failure already won. Preserve its
            // terminal state rather than relabeling a paid call.
            (existing, false)
        };
        if transitioned {
            billing_repository::mark_execution_cost_rollup_dirty(&mut *transaction, &provider_call)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        self.repair_terminal_provider_call_rollup(state, provider_call_id, &provider_call).await;
        Ok(())
    }

    /// Repairs the derived execution-cost rollup after a terminal reservation
    /// write already committed. The row is authoritative; a bounded retry that
    /// still fails is logged and never converted into a read fallback.
    async fn repair_terminal_provider_call_rollup(
        &self,
        state: &AppState,
        provider_call_id: Uuid,
        provider_call: &billing_repository::BillingProviderCallRow,
    ) {
        if let Err(error) = self
            .roll_up_execution_cost_with_retry(
                state,
                &provider_call.owning_execution_kind,
                provider_call.owning_execution_id,
            )
            .await
        {
            tracing::error!(
                %provider_call_id,
                owning_execution_kind = %provider_call.owning_execution_kind,
                owning_execution_id = %provider_call.owning_execution_id,
                %error,
                "terminal provider call persisted but bounded execution-cost rollup repair failed"
            );
        }
    }

    /// Captures a tracked provider-call record and rolls its execution cost forward.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when attribution fails, repository writes fail, or required billing
    /// metadata cannot be resolved.
    pub async fn capture_execution_provider_call(
        &self,
        state: &AppState,
        command: CaptureExecutionBillingCommand,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let owning_execution_kind = parse_execution_owner_kind(&command.owning_execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(&command.owning_execution_kind))?;
        self.validate_runtime_attribution(
            state,
            owning_execution_kind,
            command.owning_execution_id,
            command.runtime_execution_id,
            command.runtime_task_kind,
        )
        .await?;
        let execution_scope = self
            .resolve_execution_scope(state, owning_execution_kind, command.owning_execution_id)
            .await?;
        if execution_scope.workspace_id != command.workspace_id {
            return Err(ApiError::Conflict(format!(
                "execution {} belongs to workspace {}, not {}",
                command.owning_execution_id, execution_scope.workspace_id, command.workspace_id
            )));
        }
        if execution_scope.library_id != command.library_id {
            return Err(ApiError::Conflict(format!(
                "execution {} belongs to library {}, not {}",
                command.owning_execution_id, execution_scope.library_id, command.library_id
            )));
        }
        let Some(provider_catalog) = ai_repository::get_provider_catalog_by_kind(
            &state.persistence.postgres,
            &command.provider_kind,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        let Some(model_catalog) = ai_repository::get_model_catalog_by_provider_and_name(
            &state.persistence.postgres,
            &command.provider_kind,
            &command.model_name,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };

        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let provider_call = billing_repository::create_provider_call(
            &mut *transaction,
            &billing_repository::NewBillingProviderCall {
                id: Uuid::now_v7(),
                workspace_id: command.workspace_id,
                library_id: command.library_id,
                binding_id: command.binding_id,
                owning_execution_kind: execution_owner_kind_key(owning_execution_kind),
                owning_execution_id: command.owning_execution_id,
                runtime_execution_id: command.runtime_execution_id,
                runtime_task_kind: command.runtime_task_kind.map(RuntimeTaskKind::as_str),
                provider_catalog_id: provider_catalog.id,
                model_catalog_id: model_catalog.id,
                call_kind: &command.call_kind,
                call_state: "completed",
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        self.persist_provider_call_usage_in_transaction(
            &mut transaction,
            &provider_call,
            &command.usage_json,
        )
        .await?;
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        // A captured provider call is terminal and completed. Rate-limited
        // calls never reach this point, so the retry layer counts them (IRG-204).
        crate::observability::metrics::record_provider_call(
            &command.call_kind,
            &command.model_name,
            crate::observability::metrics::ProviderCallStatus::Completed,
        );
        let rollup = self
            .roll_up_execution_cost_with_retry(
                state,
                execution_owner_kind_key(owning_execution_kind),
                command.owning_execution_id,
            )
            .await?;
        if let Some(cost) = &rollup
            && cost.currency_code == "USD"
            && let Some(usd) = rust_decimal::prelude::ToPrimitive::to_f64(&cost.total_cost)
        {
            crate::observability::metrics::record_provider_cost_usd(
                &command.call_kind,
                &command.model_name,
                usd,
            );
        }
        Ok(rollup)
    }

    pub(super) async fn persist_provider_call_usage_in_transaction(
        &self,
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        provider_call: &billing_repository::BillingProviderCallRow,
        usage_json: &Value,
    ) -> Result<(), ApiError> {
        let provider_profile = ai_repository::get_provider_billing_usage_profile(
            &mut **transaction,
            provider_call.provider_catalog_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let service_tier_multipliers = parse_service_tier_multipliers(
            provider_profile
                .as_ref()
                .and_then(|profile| profile.service_tier_multipliers_json.as_ref()),
        )
        .map_err(|error| {
            ApiError::internal_with_log(error, "invalid typed service tier multipliers in catalog")
        })?;
        let configured_semantics =
            resolve_configured_usage_semantics(provider_call, provider_profile.as_ref())?;
        let usage_semantics =
            provider_usage_semantics_for_payload(usage_json, configured_semantics);
        let normalized_usage = normalize_token_usage(usage_json, usage_semantics);
        warn_if_cached_input_clamped(provider_call, &normalized_usage);
        let request_input_tokens = normalized_usage.context_input_tokens.and_then(decimal_to_i32);
        // Pricing is part of the model row, so every usage row prices in the
        // same transaction. A missing model row would mean the provider-call
        // foreign key was violated — fail loudly instead of recording an
        // unpriced charge gap.
        let pricing =
            ai_repository::get_model_pricing(&mut **transaction, provider_call.model_catalog_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::internal_with_log(
                        anyhow::anyhow!(
                            "model {} vanished while pricing provider call {}",
                            provider_call.model_catalog_id,
                            provider_call.id
                        ),
                        "internal",
                    )
                })?;
        let usages = normalized_usage.rows(provider_call.id);
        let tier_multiplier = service_tier_multiplier(usage_json, &service_tier_multipliers);
        for usage in usages {
            let usage_row = billing_repository::create_usage(&mut **transaction, &usage)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let unit_price =
                pricing.unit_price(&usage_row.billing_unit, request_input_tokens) * tier_multiplier;
            let total_price = unit_price * usage_row.quantity / Decimal::from(1_000_000u64);
            let _ = billing_repository::create_charge(
                &mut **transaction,
                &billing_repository::NewBillingCharge {
                    usage_id: usage_row.id,
                    currency_code: pricing.currency_code.clone(),
                    unit_price,
                    total_price,
                    priced_at: Some(Utc::now()),
                },
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        }
        Ok(())
    }

    /// Recomputes the rolled-up billing cost for an execution after provider usage changes.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the execution kind is invalid or repository writes fail.
    pub async fn roll_up_execution_cost(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let execution_kind = parse_execution_owner_kind(execution_kind)
            .ok_or_else(|| invalid_execution_owner_kind(execution_kind))?;
        // Resolve owning execution scope (library + document) so the
        // rollup row carries its own attribution columns. Both billing
        // read endpoints (/library-cost-summary and /library-document-costs)
        // read those columns directly without re-joining provider_call. Do
        // this before opening the single-connection transaction below.
        let scope = self.resolve_execution_scope(state, execution_kind, execution_id).await?;
        let execution_kind_key = execution_owner_kind_key(execution_kind);
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        billing_repository::lock_execution_cost_rollup(
            &mut *transaction,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let rollup_state = billing_repository::ensure_execution_cost_rollup_state(
            &mut *transaction,
            execution_kind_key,
            execution_id,
            scope.workspace_id,
            scope.library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let provider_call_count = billing_repository::count_provider_calls_by_execution(
            &mut *transaction,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let unpriced_provider_call_count =
            billing_repository::count_unpriced_provider_calls_by_execution(
                &mut *transaction,
                execution_kind_key,
                execution_id,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let rollups = billing_repository::list_execution_cost_rollups(
            &mut *transaction,
            execution_kind_key,
            execution_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let (total_cost, currency_code) = match rollups.as_slice() {
            [] => (Decimal::ZERO, "USD".to_string()),
            [rollup] => (rollup.total_cost, rollup.currency_code.clone()),
            _ => {
                let terminal_acknowledged =
                    billing_repository::acknowledge_execution_cost_rollup_terminal_error(
                        &mut *transaction,
                        execution_kind_key,
                        execution_id,
                        rollup_state.dirty_generation,
                        MIXED_CURRENCY_TERMINAL_ERROR_CODE,
                    )
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
                if !terminal_acknowledged {
                    return Err(ApiError::service_unavailable(
                        "billing execution changed while its cost was being reconciled",
                        "billing_rollup_raced",
                    ));
                }
                transaction
                    .commit()
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
                return Err(mixed_currency_billing_scope("execution", execution_id));
            }
        };
        let provider_call_count = i32::try_from(provider_call_count).unwrap_or(i32::MAX);
        let unpriced_provider_call_count =
            i32::try_from(unpriced_provider_call_count).unwrap_or(i32::MAX);
        let row = billing_repository::upsert_execution_cost(
            &mut *transaction,
            &billing_repository::UpsertBillingExecutionCost {
                owning_execution_kind: execution_kind_key,
                owning_execution_id: execution_id,
                workspace_id: scope.workspace_id,
                library_id: scope.library_id,
                knowledge_document_id: scope.knowledge_document_id,
                total_cost,
                currency_code: &currency_code,
                provider_call_count,
                unpriced_provider_call_count,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let generation_acknowledged =
            billing_repository::acknowledge_execution_cost_rollup_generation(
                &mut *transaction,
                execution_kind_key,
                execution_id,
                rollup_state.dirty_generation,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !generation_acknowledged {
            return Err(ApiError::service_unavailable(
                "billing execution changed while its cost was being reconciled",
                "billing_rollup_raced",
            ));
        }
        transaction.commit().await.map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(Some(map_execution_cost_row(row).map_err(ApiError::BadRequest)?))
    }

    pub(super) async fn roll_up_execution_cost_with_retry(
        &self,
        state: &AppState,
        execution_kind: &str,
        execution_id: Uuid,
    ) -> Result<Option<BillingExecutionCost>, ApiError> {
        let mut retry_index = 0_usize;
        loop {
            match self.roll_up_execution_cost(state, execution_kind, execution_id).await {
                Ok(rollup) => return Ok(rollup),
                Err(error)
                    if execution_cost_rollup_error_is_retryable(&error)
                        && retry_index < EXECUTION_COST_ROLLUP_RETRY_DELAYS_MS.len() =>
                {
                    let delay_ms = EXECUTION_COST_ROLLUP_RETRY_DELAYS_MS[retry_index];
                    retry_index += 1;
                    tracing::warn!(
                        execution_kind,
                        %execution_id,
                        attempt = retry_index,
                        delay_ms,
                        %error,
                        "retrying derived execution-cost rollup"
                    );
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ApiError, reserved_terminal_call_state};

    #[test]
    fn zero_usage_reservation_accepts_only_failed_or_canceled() {
        for state in ["failed", "canceled"] {
            assert!(reserved_terminal_call_state(state).is_ok(), "{state}");
        }
        for state in ["started", "completed", "", "Failed", "cancelled"] {
            assert!(
                matches!(reserved_terminal_call_state(state), Err(ApiError::BadRequest(_))),
                "{state}",
            );
        }
    }
}
