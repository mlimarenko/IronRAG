//! Declarative per-library retrieval-lane configuration.
//!
//! This is the policy-compliant home for language-specific retrieval choices:
//! the Postgres full-text-search analyzer (`regconfig`) is operator data carried
//! on the library, not a hardcoded code-side constant. A Russian library can opt
//! into `russian` stemming without any code change, and the language-bearing
//! choice lives in configuration rather than an inline keyword list.
//!
//! v1 exposes only the single lexical knob that existing code reads AND that this
//! change actually wires: the Postgres FTS text-search config. The default object
//! reproduces today's hardcoded behaviour byte-for-byte (`simple`). Fusion knobs,
//! per-lane thresholds, and the prefix-token minimum are intentionally omitted
//! until there is a clean wiring point for each — exposing them now would be dead
//! config (no reader), which the project's "no dead config" rule forbids.

use serde::{Deserialize, Serialize};

/// Historical default Postgres FTS text-search config baked into the lexical SQL
/// before this configuration existed. The default [`RetrievalConfig`] resolves to
/// exactly this value, keeping rendered SQL byte-identical for unconfigured
/// libraries.
pub const DEFAULT_TEXT_SEARCH_CONFIG: &str = "simple";

/// Top-level per-library retrieval configuration persisted as the
/// `catalog_library.retrieval_config` JSONB column.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct RetrievalConfig {
    /// Lexical (full-text-search) lane configuration.
    #[serde(default)]
    pub lexical: RetrievalLexicalConfig,
}

/// Lexical-lane knobs sourced by the Postgres search store when rendering the
/// full-text-search SQL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct RetrievalLexicalConfig {
    /// Name of the Postgres text-search configuration (`regconfig`) used by the
    /// `to_tsquery` / `websearch_to_tsquery` constructors in the lexical lane.
    /// Validated against the live `pg_ts_config` catalog at write time; defaults
    /// to [`DEFAULT_TEXT_SEARCH_CONFIG`].
    #[serde(default = "default_text_search_config")]
    pub text_search_config: String,
}

impl Default for RetrievalLexicalConfig {
    fn default() -> Self {
        Self { text_search_config: default_text_search_config() }
    }
}

fn default_text_search_config() -> String {
    DEFAULT_TEXT_SEARCH_CONFIG.to_string()
}

impl RetrievalConfig {
    /// Builds a configuration from persisted JSON, applying defaults for absent
    /// keys.
    ///
    /// # Errors
    /// Returns an error when the JSON is not the canonical retrieval-config shape
    /// (for example unknown fields or wrong value types).
    pub fn from_json(value: serde_json::Value) -> Result<Self, String> {
        serde_json::from_value::<Self>(value)
            .map_err(|error| format!("invalid retrieval config: {error}"))
    }

    /// Serializes the configuration into the canonical database/API JSON
    /// representation.
    ///
    /// # Errors
    /// Returns an error if serialization unexpectedly fails.
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Validates structural invariants that do not require database access.
    ///
    /// The text-search config name is additionally checked against the live
    /// `pg_ts_config` catalog at the API boundary; this method only rejects a
    /// blatantly empty analyzer name so the lexical SQL never renders an empty
    /// `regconfig` literal.
    ///
    /// # Errors
    /// Returns a human-readable validation error when an invariant is violated.
    pub fn validate(&self) -> Result<(), String> {
        if self.lexical.text_search_config.trim().is_empty() {
            return Err("lexical.textSearchConfig must not be empty".to_string());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_uses_simple_text_search_config() {
        let config = RetrievalConfig::default();
        assert_eq!(config.lexical.text_search_config, "simple");
    }

    #[test]
    fn empty_object_resolves_to_defaults() {
        let config = RetrievalConfig::from_json(serde_json::json!({}))
            .expect("empty object should parse to defaults");
        assert_eq!(config, RetrievalConfig::default());
    }

    #[test]
    fn lexical_without_text_search_config_keeps_default() {
        let config = RetrievalConfig::from_json(serde_json::json!({ "lexical": {} }))
            .expect("partial lexical object should parse");
        assert_eq!(config.lexical.text_search_config, "simple");
    }

    #[test]
    fn config_round_trips_camel_case_json() {
        let config = RetrievalConfig {
            lexical: RetrievalLexicalConfig { text_search_config: "russian".to_string() },
        };
        let json = config.to_json().expect("config should serialize");

        assert_eq!(json["lexical"]["textSearchConfig"], serde_json::json!("russian"));
        assert_eq!(RetrievalConfig::from_json(json).expect("config should parse"), config);
    }

    #[test]
    fn config_rejects_unknown_top_level_fields() {
        let json = serde_json::json!({ "fusion": { "rrfK": 60 } });
        assert!(RetrievalConfig::from_json(json).is_err());
    }

    #[test]
    fn config_rejects_unknown_lexical_fields() {
        let json = serde_json::json!({ "lexical": { "analyzer": "x" } });
        assert!(RetrievalConfig::from_json(json).is_err());
    }

    #[test]
    fn validate_rejects_blank_text_search_config() {
        let config = RetrievalConfig {
            lexical: RetrievalLexicalConfig { text_search_config: "   ".to_string() },
        };
        assert!(config.validate().is_err());
    }
}
