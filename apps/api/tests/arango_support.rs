use ironrag_backend::app::config::Settings;

#[must_use]
pub fn sample_arango_base_url(settings: &Settings) -> String {
    settings.arangodb_url.trim().trim_end_matches('/').to_string()
}

#[must_use]
pub fn sample_arango_database(settings: &Settings) -> String {
    settings.arangodb_database.trim().to_string()
}
