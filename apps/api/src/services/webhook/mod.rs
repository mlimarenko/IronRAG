pub mod delivery;
pub mod outbound;
pub mod signature;
pub mod ssrf;

use reqwest::Client;

/// Webhook service — holds a shared HTTP client for outbound delivery.
///
/// Stateless service following the codebase's `#[derive(Clone, Default)]`
/// pattern.  The HTTP client is wrapped in `Option` so `Default` works
/// without requiring runtime async setup; it is initialised lazily on the
/// first call to `http_client()`.
#[derive(Clone, Debug)]
pub struct WebhookService {
    http: Client,
}

impl WebhookService {
    #[must_use]
    pub fn new() -> Self {
        Self {
            http: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .user_agent("ironrag-webhook/1.0")
                // Disable redirects so a public target cannot redirect the delivery
                // worker to a private/localhost address (SSRF via redirect).
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap_or_default(),
        }
    }

    #[must_use]
    pub fn http_client(&self) -> &Client {
        &self.http
    }
}

impl Default for WebhookService {
    fn default() -> Self {
        Self::new()
    }
}
