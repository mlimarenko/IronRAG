use super::*;

pub(super) fn provider_call_attribution(
    binding: &ResolvedRuntimeBinding,
    call_kind: QueryProviderCallKind,
) -> Result<QueryProviderCallAttribution, QueryServiceError> {
    QueryProviderCallAttribution::try_new(
        binding.binding_id,
        binding.binding_purpose,
        ProviderModelSelection {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
        },
        call_kind,
    )
    .map_err(|error| {
        tracing::error!(
            binding_id = %binding.binding_id,
            actual_purpose = binding.binding_purpose.as_str(),
            expected_purpose = call_kind.binding_purpose().as_str(),
            call_kind = call_kind.as_str(),
            "query provider-call attribution purpose mismatch"
        );
        QueryServiceError::Internal(anyhow::Error::new(error))
    })
}

pub(super) async fn reserve_attributed_provider_call(
    state: &AppState,
    execution_context: QueryProviderExecutionContext,
    binding: &ResolvedRuntimeBinding,
    call_kind: QueryProviderCallKind,
) -> Result<(QueryProviderCallAttribution, QueryProviderCallReservation), QueryServiceError> {
    // Purpose validation must happen before reservation and before provider
    // I/O; a misrouted binding is never represented as a billable call.
    let attribution = provider_call_attribution(binding, call_kind)?;
    let reservation = QueryProviderCallReservation::reserve(
        state,
        execution_context,
        binding,
        call_kind.binding_purpose(),
        call_kind.as_str(),
    )
    .await
    .map_err(|error| {
        QueryServiceError::Internal(anyhow::anyhow!(
            "failed to reserve {} provider call: {error}",
            call_kind.as_str()
        ))
    })?;
    Ok((attribution, reservation))
}

pub(super) async fn complete_attributed_provider_call(
    attribution: &QueryProviderCallAttribution,
    reservation: &mut QueryProviderCallReservation,
    usage_json: serde_json::Value,
) -> Result<QueryProviderCall, QueryServiceError> {
    let provider_call_id = reservation.provider_call_id();
    reservation.complete(&usage_json).await.map_err(|error| {
        QueryServiceError::Internal(anyhow::anyhow!(
            "failed to persist provider-call usage {provider_call_id}: {error}"
        ))
    })?;
    Ok(attribution.record(provider_call_id, usage_json))
}

pub(super) async fn fail_provider_call(reservation: &mut QueryProviderCallReservation) {
    if let Err(error) = reservation.fail().await {
        tracing::error!(
            provider_call_id = %reservation.provider_call_id(),
            %error,
            "failed to terminalize query provider-call reservation"
        );
    }
}
