use rustrag_backend::app::config::Settings;

pub fn sample_arango_base_url(settings: &Settings) -> String {
    settings.arangodb_url.trim().trim_end_matches('/').to_string()
}

pub fn sample_arango_database(settings: &Settings) -> String {
    settings.arangodb_database.trim().to_string()
}
