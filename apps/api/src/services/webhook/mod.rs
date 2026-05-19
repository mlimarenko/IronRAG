pub mod delivery;
pub mod error;
pub mod outbound;
pub mod signature;
pub mod ssrf;

/// Webhook service marker.
///
/// Outbound delivery clients are built per target from the canonical
/// public-URL resolver so each attempt pins DNS to already validated
/// public addresses.
#[derive(Clone, Debug)]
pub struct WebhookService;

impl WebhookService {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for WebhookService {
    fn default() -> Self {
        Self::new()
    }
}
