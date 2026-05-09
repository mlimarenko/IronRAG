use axum::{Router, routing::get};
use axum_prometheus::PrometheusMetricLayer;

use crate::app::state::AppState;

pub fn apply(router: Router<AppState>) -> Router<AppState> {
    let (prometheus_layer, prometheus_handle) = PrometheusMetricLayer::pair();
    router
        .route(
            "/metrics",
            get(move || {
                let handle = prometheus_handle.clone();
                async move { handle.render() }
            }),
        )
        .layer(prometheus_layer)
}
