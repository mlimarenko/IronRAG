use std::sync::Arc;

use crate::{
    app::config::Settings,
    infra::persistence::Persistence,
    integrations::llm::{LlmGateway, UnifiedGateway},
};

#[derive(Clone)]
pub struct AppState {
    pub settings: Settings,
    pub persistence: Persistence,
    pub llm_gateway: Arc<dyn LlmGateway>,
}

impl AppState {
    /// Creates shared application state and initializes persistence/gateway dependencies.
    ///
    /// # Errors
    /// Returns any initialization error from persistence setup.
    pub async fn new(settings: Settings) -> anyhow::Result<Self> {
        let persistence = Persistence::connect(&settings).await?;
        Ok(Self {
            llm_gateway: Arc::new(UnifiedGateway::from_settings(&settings)),
            settings,
            persistence,
        })
    }
}
