use anyhow::{Context, Result};
use url::Url;

pub(crate) fn database_url_with_name(database_url: &str, database_name: &str) -> Result<String> {
    let mut parsed = Url::parse(database_url).context("failed to parse PostgreSQL database URL")?;
    parsed.set_path(&format!("/{database_name}"));
    Ok(parsed.into())
}
