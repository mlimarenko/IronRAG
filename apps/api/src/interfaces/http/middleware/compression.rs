use axum::Router;
use tower_http::compression::CompressionLayer;

pub fn apply<S: Clone + Send + Sync + 'static>(router: Router<S>) -> Router<S> {
    router.layer(CompressionLayer::new().gzip(true).br(true).zstd(true).no_deflate())
}
