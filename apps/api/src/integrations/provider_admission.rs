//! The one durable owner that admits a physical provider request.
//!
//! It is injected into the gateway and the TypeSafe client, and required by
//! [`crate::shared::provider_http::send_provider_http_request`]: a caller that
//! does not hold it cannot dispatch at all. Under a required incident scope it
//! reserves one complete attempt unit on the authoritative provider row before
//! the first byte leaves; with no required scope it admits only while the
//! database proves no persisted incident scope exists, so removing the
//! configuration cannot silently restore unguarded sending.

use async_trait::async_trait;
use sqlx::PgPool;

use crate::{
    infra::repositories::ai_repository::{
        FinancialAdmissionError, RequiredFinancialScope, consume_financial_attempt,
    },
    shared::provider_http::{
        DispatchRequestEvidence, ProviderDispatchAdmission, ProviderDispatchAuthority,
        ProviderDispatchContext, ProviderDispatchPermit, ProviderDispatchRefusal,
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
}

#[async_trait]
impl ProviderDispatchAdmission for FinancialDispatchAdmission {
    async fn admit(
        &self,
        context: &ProviderDispatchContext,
        evidence: &DispatchRequestEvidence<'_>,
    ) -> Result<ProviderDispatchPermit, ProviderDispatchRefusal> {
        let Some(scope) = &self.required_scope else {
            // Ordinary runtime: the startup evaluation already proved no
            // persisted incident scope exists. The value is fixed for the
            // process lifetime, so no per-send read repeats it.
            return if self.ordinary_sends_allowed {
                Ok(ProviderDispatchPermit::unqualified())
            } else {
                Err(ProviderDispatchRefusal::Authority)
            };
        };
        // The narrow account-validation authority never covers incident traffic.
        if context.authority() != ProviderDispatchAuthority::RuntimeBinding
            || context.provider_catalog_id() != scope.provider_id
        {
            return Err(ProviderDispatchRefusal::Authority);
        }
        consume_financial_attempt(
            &self.postgres,
            scope,
            context.model_name(),
            evidence.url.as_str(),
            evidence.body,
        )
        .await
        .map(|permit| ProviderDispatchPermit::reserved(permit.scope_id))
        .map_err(classify_financial_admission_error)
    }
}

fn classify_financial_admission_error(error: FinancialAdmissionError) -> ProviderDispatchRefusal {
    match error {
        FinancialAdmissionError::InvalidScope | FinancialAdmissionError::NotActive => {
            ProviderDispatchRefusal::Authority
        }
        FinancialAdmissionError::Exhausted => ProviderDispatchRefusal::Exhausted,
        FinancialAdmissionError::Envelope => ProviderDispatchRefusal::Envelope,
        FinancialAdmissionError::Persistence(_) => ProviderDispatchRefusal::Admission,
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

    #[test]
    fn financial_admission_error_preserves_refusal_kind() {
        for (error, expected) in [
            (FinancialAdmissionError::InvalidScope, ProviderDispatchRefusal::Authority),
            (FinancialAdmissionError::NotActive, ProviderDispatchRefusal::Authority),
            (FinancialAdmissionError::Exhausted, ProviderDispatchRefusal::Exhausted),
            (FinancialAdmissionError::Envelope, ProviderDispatchRefusal::Envelope),
            (
                FinancialAdmissionError::Persistence(sqlx::Error::PoolClosed),
                ProviderDispatchRefusal::Admission,
            ),
        ] {
            assert_eq!(classify_financial_admission_error(error), expected);
        }
    }
}
