use tracing_subscriber::{EnvFilter, fmt};

pub fn init(filter: &str) {
    let _ = fmt().with_env_filter(EnvFilter::new(filter)).with_target(false).try_init();
}
