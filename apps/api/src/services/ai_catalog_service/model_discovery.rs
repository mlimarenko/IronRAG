use super::provider_validation::fetch_available_provider_models;
use super::*;

/// One model the provider reports for an account, paired with the catalog row
/// that already represents it, if any.
///
/// The pairing is what makes the binding screen usable: a router proxies
/// hundreds of models and seeds none of them, so without it the operator sees
/// an empty dropdown and no way to learn what their plan exposes.
#[derive(Debug, Clone)]
pub struct AvailableProviderModel {
    pub discovered: DiscoveredProviderModel,
    pub model_catalog_id: Option<Uuid>,
}

impl AiCatalogService {
    /// Live model listing for one account's credentials.
    ///
    /// This is deliberately its own read, requested explicitly, rather than an
    /// enrichment of the model catalog: the resolved-catalog paths must not
    /// decrypt credentials or call a provider, and a test in this module holds
    /// them to it.
    pub async fn list_available_provider_models(
        &self,
        state: &AppState,
        account_id: Uuid,
    ) -> Result<Vec<AvailableProviderModel>, ApiError> {
        let account = self.get_account(state, account_id).await?;
        let providers = self.list_provider_catalog(state).await?;
        let provider =
            providers.iter().find(|entry| entry.id == account.provider_catalog_id).ok_or_else(
                || ApiError::resource_not_found("provider_catalog", account.provider_catalog_id),
            )?;
        let discovered = fetch_available_provider_models(
            provider,
            account.api_key.as_deref(),
            account.base_url.as_deref(),
        )
        .await?;
        let catalog = self.list_model_catalog(state, Some(provider.id)).await?;
        Ok(discovered
            .into_iter()
            .map(|model| {
                let model_catalog_id = catalog
                    .iter()
                    .find(|entry| entry.model_name == model.model_name)
                    .map(|entry| entry.id);
                AvailableProviderModel { discovered: model, model_catalog_id }
            })
            .collect())
    }
}
