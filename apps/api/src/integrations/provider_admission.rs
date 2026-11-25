//! The one durable owner that admits a physical provider request.
//!
//! It is injected into the gateway and the TypeSafe client, and required by
//! [`crate::shared::provider_http::send_provider_http_request`]: a caller that
//! does not hold it cannot dispatch at all. Every billable send — one HTTP
//! attempt, including each retry — owns exactly one `billing_provider_call` row
//! created in the same transaction that debits the financial counter under the
//! provider row lock. With no required scope it admits only while the database
//! proves no persisted incident scope exists, so removing the configuration
//! cannot silently restore unguarded sending.
//!
//! The only dispatch that owns no row is the narrow account-creation credential
//! validation, which runs before an account or a binding exists, has no owning
//! execution to attribute, and is already refused under any required scope.
//! That exclusion is typed here rather than implied by a missing reservation.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::{
    infra::repositories::{
        ai_repository::{FinancialAdmissionError, RequiredFinancialScope},
        billing_repository::{self, ProviderSendAdmissionError},
    },
    shared::{
        provider_http::{
            DispatchRequestEvidence, ProviderDispatchAdmission, ProviderDispatchAuthority,
            ProviderDispatchContext, ProviderDispatchPermit, ProviderDispatchRefusal,
            ProviderSendPermit, ProviderSendTerminator,
        },
        provider_send::{self, ProviderSendClaimError},
    },
};

/// Durable admission over the single authoritative provider row.
pub struct FinancialDispatchAdmission {
    postgres: PgPool,
    required_scope: Option<RequiredFinancialScope>,
    /// Evaluated once during state construction. `true` only when startup
    /// proved no persisted incident scope exists, so an ordinary runtime may
    /// send; a failed or unresolved evaluation stays `false` (fail closed).
    ordinary_sends_allowed: bool,
}

impl FinancialDispatchAdmission {
    #[must_use]
    pub fn new(
        postgres: PgPool,
        required_scope: Option<RequiredFinancialScope>,
        ordinary_sends_allowed: bool,
    ) -> Self {
        Self { postgres, required_scope, ordinary_sends_allowed }
    }

    /// Terminalizes a send row from a dropped transport future.
    ///
    /// The task owns a pool clone only; it never fabricates usage and never
    /// sends again. A row that a concurrent settlement already completed keeps
    /// its terminal state.
    fn send_terminator(&self) -> ProviderSendTerminator {
        let postgres = self.postgres.clone();
        Arc::new(move |provider_call_id, call_state| {
            let postgres = postgres.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(async move {
                        if let Err(error) = billing_repository::finish_provider_call_without_usage(
                            &postgres,
                            provider_call_id,
                            call_state,
                        )
                        .await
                        {
                            tracing::error!(
                                %provider_call_id,
                                %error,
                                "provider send row could not be terminalized"
                            );
                        }
                    });
                }
                Err(error) => {
                    tracing::error!(
                        %provider_call_id,
                        %error,
                        "provider send row terminalization has no runtime"
                    );
                }
            }
        })
    }
}

#[async_trait]
impl ProviderDispatchAdmission for FinancialDispatchAdmission {
    async fn admit(
        &self,
        context: &ProviderDispatchContext,
        evidence: &DispatchRequestEvidence<'_>,
    ) -> Result<ProviderDispatchPermit, ProviderDispatchRefusal> {
        let authority = context.authority();

        // Account creation may validate a proposed credential with a chat round
        // trip. That is a control-plane probe, not a product execution: it runs
        // before the account and any binding exist, so it has no owning
        // execution to attribute. Its cost is not represented by the product
        // ledger, whose rows require a workspace; that coverage limit is
        // documented in `docs/en/BILLING-COSTS.md` rather than hidden.
        //
        // The narrow authority keeps the existing shape: outside an incident
        // scope the probe runs under its own control-plane authorization and
        // writes neither a row nor a counter; an active incident scope always
        // refuses it. A `RuntimeBinding` or `OrdinaryVision` send can never take
        // this branch — the authority is set only by the crate-visible
        // `DispatchAuthority` constructors, never by request input.
        if authority == ProviderDispatchAuthority::AccountValidation {
            return if self.required_scope.is_none() && self.ordinary_sends_allowed {
                Ok(ProviderDispatchPermit::unqualified())
            } else {
                Err(ProviderDispatchRefusal::Authority)
            };
        }

        // The narrow account-validation authority never covers incident
        // traffic, and only the runtime binding authority may spend under one.
        if let Some(scope) = &self.required_scope {
            if authority != ProviderDispatchAuthority::RuntimeBinding
                || context.provider_catalog_id() != scope.provider_id
            {
                return Err(ProviderDispatchRefusal::Authority);
            }
        } else if !self.ordinary_sends_allowed {
            return Err(ProviderDispatchRefusal::Authority);
        }

        let attribution = provider_send::current_send_attribution()
            .ok_or(ProviderDispatchRefusal::Attribution)?;
        if attribution.provider_catalog_id != context.provider_catalog_id() {
            return Err(ProviderDispatchRefusal::Attribution);
        }
        let provider_call_id =
            provider_send::claim_current_send_row().map_err(|error| match error {
                ProviderSendClaimError::NoAttribution => ProviderDispatchRefusal::Attribution,
                ProviderSendClaimError::Busy => ProviderDispatchRefusal::Busy,
            })?;

        let admitted = billing_repository::admit_provider_send(
            &self.postgres,
            billing_repository::ProviderSendRequest {
                required_scope: self.required_scope.as_ref(),
                caller: &context.caller(),
                model: context.model_name(),
                endpoint: evidence.url.as_str(),
                body: evidence.body,
                attribution: &attribution,
                provider_call_id,
            },
        )
        .await;

        match admitted {
            Ok(admitted) => {
                // The row and the counter are committed; publish the row without
                // awaiting anything, so no cancellation point exists between the
                // durable commit and the installed send guard.
                provider_send::publish_current_send_row(admitted.provider_call_id);
                let terminator = self.send_terminator();
                Ok(ProviderDispatchPermit::with_send(
                    admitted.scope_id,
                    ProviderSendPermit::new(admitted.provider_call_id, Some(terminator)),
                ))
            }
            Err(error) => {
                let uncertain = matches!(
                    error,
                    ProviderSendAdmissionError::Persistence(_)
                        | ProviderSendAdmissionError::Financial(
                            FinancialAdmissionError::Persistence(_)
                        )
                );
                provider_send::release_current_send_row(uncertain);
                Err(map_provider_send_admission_error(error))
            }
        }
    }
}

fn classify_financial_admission_error(error: FinancialAdmissionError) -> ProviderDispatchRefusal {
    match error {
        FinancialAdmissionError::InvalidScope | FinancialAdmissionError::NotActive => {
            ProviderDispatchRefusal::Authority
        }
        FinancialAdmissionError::Caller => ProviderDispatchRefusal::Caller,
        FinancialAdmissionError::Exhausted => ProviderDispatchRefusal::Exhausted,
        FinancialAdmissionError::Envelope => ProviderDispatchRefusal::Envelope,
        FinancialAdmissionError::Persistence(_) => ProviderDispatchRefusal::Admission,
    }
}

fn map_provider_send_admission_error(error: ProviderSendAdmissionError) -> ProviderDispatchRefusal {
    match error {
        ProviderSendAdmissionError::Attribution => ProviderDispatchRefusal::Attribution,
        ProviderSendAdmissionError::Financial(error) => classify_financial_admission_error(error),
        ProviderSendAdmissionError::Persistence(error) => {
            tracing::error!(%error, "provider send admission transaction failed");
            ProviderDispatchRefusal::Admission
        }
    }
}

/// Test-only admission for loopback provider fixtures. It is never compiled
/// into a production build and therefore cannot become a runtime fallback.
#[cfg(any(test, feature = "test-support"))]
pub struct UnqualifiedTestAdmission;

#[cfg(any(test, feature = "test-support"))]
#[async_trait]
impl ProviderDispatchAdmission for UnqualifiedTestAdmission {
    async fn admit(
        &self,
        _context: &ProviderDispatchContext,
        _evidence: &DispatchRequestEvidence<'_>,
    ) -> Result<ProviderDispatchPermit, ProviderDispatchRefusal> {
        Ok(ProviderDispatchPermit::unqualified())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::provider_http::{
        DispatchAuthority, ProviderDispatchContext, ProviderSurface,
    };
    use uuid::Uuid;

    #[test]
    fn financial_admission_error_preserves_refusal_kind() {
        for (error, expected) in [
            (FinancialAdmissionError::InvalidScope, ProviderDispatchRefusal::Authority),
            (FinancialAdmissionError::NotActive, ProviderDispatchRefusal::Authority),
            (FinancialAdmissionError::Caller, ProviderDispatchRefusal::Caller),
            (FinancialAdmissionError::Exhausted, ProviderDispatchRefusal::Exhausted),
            (FinancialAdmissionError::Envelope, ProviderDispatchRefusal::Envelope),
        ] {
            assert_eq!(classify_financial_admission_error(error), expected);
        }
    }

    fn ctx(authority: DispatchAuthority) -> ProviderDispatchContext {
        authority.attest(ProviderSurface::Chat, None)
    }

    fn evidence() -> (serde_json::Value, reqwest::Url) {
        (
            serde_json::json!({"model": "probe", "messages": []}),
            reqwest::Url::parse("https://provider.example/v1/chat/completions").expect("test URL"),
        )
    }

    fn admission(
        scope: Option<RequiredFinancialScope>,
        ordinary: bool,
    ) -> FinancialDispatchAdmission {
        let pool = sqlx::PgPool::connect_lazy("postgres://unused/unused")
            .expect("lazy pool never connects in these decisions");
        FinancialDispatchAdmission::new(pool, scope, ordinary)
    }

    fn scope() -> RequiredFinancialScope {
        RequiredFinancialScope {
            provider_id: Uuid::now_v7(),
            scope_id: Uuid::now_v7(),
            policy_fingerprint: "sha256:test".to_string(),
        }
    }

    #[tokio::test]
    async fn the_account_validation_probe_keeps_its_narrow_control_plane_shape() {
        let authority = DispatchAuthority::account_validation(
            "probe",
            Uuid::now_v7(),
            None,
            "probe-model",
            "https://provider.example",
            None,
        );
        let (body, url) = evidence();
        let evidence =
            DispatchRequestEvidence { method: &reqwest::Method::POST, url: &url, body: &body };

        // Outside an incident scope the probe keeps its existing behavior: it is
        // admitted under its own control-plane authority and owns no product
        // ledger row and no counter debit.
        let permit = admission(None, true)
            .admit(&ctx(authority.clone()), &evidence)
            .await
            .expect("control-plane probe admitted outside an incident scope");
        assert_eq!(permit.scope_id(), None);
        assert_eq!(permit.send_id(), None);

        // An active incident scope always refuses it, before any I/O.
        let error = admission(Some(scope()), true)
            .admit(&ctx(authority), &evidence)
            .await
            .expect_err("an incident scope refuses the control-plane probe");
        assert_eq!(error, ProviderDispatchRefusal::Authority);
    }

    #[tokio::test]
    async fn a_runtime_binding_send_without_attribution_never_takes_the_probe_branch() {
        let authority = DispatchAuthority::runtime_binding(
            "probe",
            Uuid::now_v7(),
            None,
            "probe-model",
            "https://provider.example",
            None,
        );
        let (body, url) = evidence();
        let evidence =
            DispatchRequestEvidence { method: &reqwest::Method::POST, url: &url, body: &body };
        let error = admission(None, true)
            .admit(&ctx(authority), &evidence)
            .await
            .expect_err("product dispatch requires attribution");
        assert_eq!(error, ProviderDispatchRefusal::Attribution);
    }

    #[tokio::test]
    async fn an_ordinary_vision_send_without_attribution_never_takes_the_probe_branch() {
        let authority = DispatchAuthority::ordinary_vision(
            "probe",
            "probe-model",
            "https://provider.example",
            None,
        );
        let (body, url) = evidence();
        let evidence =
            DispatchRequestEvidence { method: &reqwest::Method::POST, url: &url, body: &body };
        let error = admission(None, true)
            .admit(&ctx(authority), &evidence)
            .await
            .expect_err("product dispatch requires attribution");
        assert_eq!(error, ProviderDispatchRefusal::Attribution);
    }

    #[tokio::test]
    async fn the_probe_is_refused_when_ordinary_sends_are_not_proven_safe() {
        let authority = DispatchAuthority::account_validation(
            "probe",
            Uuid::now_v7(),
            None,
            "probe-model",
            "https://provider.example",
            None,
        );
        let (body, url) = evidence();
        let evidence =
            DispatchRequestEvidence { method: &reqwest::Method::POST, url: &url, body: &body };
        let error = admission(None, false)
            .admit(&ctx(authority), &evidence)
            .await
            .expect_err("unproven ordinary runtime stays closed");
        assert_eq!(error, ProviderDispatchRefusal::Authority);
    }

    #[test]
    fn a_refused_send_admission_maps_to_a_typed_refusal() {
        assert_eq!(
            map_provider_send_admission_error(ProviderSendAdmissionError::Attribution),
            ProviderDispatchRefusal::Attribution
        );
    }
}
