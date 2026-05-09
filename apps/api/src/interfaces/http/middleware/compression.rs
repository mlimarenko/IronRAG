use axum::Router;
use tower_http::compression::CompressionLayer;

use crate::app::state::AppState;

pub fn apply(router: Router<AppState>) -> Router<AppState> {
    router.layer(CompressionLayer::new().gzip(true).br(true).zstd(true).no_deflate())
}
