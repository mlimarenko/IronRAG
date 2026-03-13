use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Settings {
    pub bind_addr: String,
    pub database_url: String,
    pub database_max_connections: u32,
    pub redis_url: String,
    pub service_name: String,
    pub environment: String,
    pub log_filter: String,
    pub openai_api_key: Option<String>,
    pub deepseek_api_key: Option<String>,
    pub openai_input_price_per_1m: f64,
    pub openai_output_price_per_1m: f64,
    pub deepseek_input_price_per_1m: f64,
    pub deepseek_output_price_per_1m: f64,
}

impl Settings {
    /// Loads application settings from environment variables with defaults.
    ///
    /// # Errors
    /// Returns a [`config::ConfigError`] if configuration defaults cannot be built
    /// or environment values fail deserialization.
    pub fn from_env() -> Result<Self, config::ConfigError> {
        let cfg = config::Config::builder()
            .set_default("bind_addr", "0.0.0.0:8080")?
            .set_default("service_name", "rustrag-backend")?
            .set_default("environment", "local")?
            .set_default("database_url", "postgres://postgres:postgres@127.0.0.1:5432/rustrag")?
            .set_default("database_max_connections", 20)?
            .set_default("redis_url", "redis://127.0.0.1:6379")?
            .set_default("log_filter", "info")?
            .set_default("openai_input_price_per_1m", 0.25)?
            .set_default("openai_output_price_per_1m", 2.0)?
            .set_default("deepseek_input_price_per_1m", 0.27)?
            .set_default("deepseek_output_price_per_1m", 1.10)?
            .add_source(config::Environment::default().separator("__"))
            .add_source(config::Environment::with_prefix("RUSTRAG").separator("__"))
            .build()?;

        cfg.try_deserialize()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_has_sane_local_defaults() {
        let settings = Settings::from_env().expect("settings should load with defaults");

        assert_eq!(settings.bind_addr, "0.0.0.0:8080");
        assert_eq!(settings.service_name, "rustrag-backend");
        assert_eq!(settings.environment, "local");
        assert_eq!(settings.database_max_connections, 20);
        assert_eq!(settings.redis_url, "redis://127.0.0.1:6379");
        assert_eq!(settings.log_filter, "info");
    }

    #[test]
    fn from_env_provides_default_database_url() {
        let settings = Settings::from_env().expect("settings should load with defaults");

        assert_eq!(
            settings.database_url,
            "postgres://postgres:postgres@127.0.0.1:5432/rustrag"
        );
    }
}
