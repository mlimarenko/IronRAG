mod code;
mod config;
mod environment;
mod error;
mod version;

// branding.rs is intentionally NOT re-exported. Branded identifier
// heuristics (heading phrase matching, catalog link guessing) were
// the noisiest extractor in the pipeline and are now fully delegated
// to the LLM's entity extraction — not the technical fact store.

pub(crate) use code::extract_code_identifier_candidates;
pub(crate) use config::extract_config_key_candidates;
pub(crate) use environment::extract_environment_variable_candidates;
pub(crate) use error::extract_error_code_candidates;
pub(crate) use version::extract_version_candidates;
