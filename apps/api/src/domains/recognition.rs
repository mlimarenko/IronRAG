use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionEngine {
    Native,
    Docling,
    Vision,
}

impl RecognitionEngine {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Docling => "docling",
            Self::Vision => "vision",
        }
    }
}

impl fmt::Display for RecognitionEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RecognitionEngine {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "native" => Ok(Self::Native),
            "docling" => Ok(Self::Docling),
            "vision" => Ok(Self::Vision),
            other => Err(format!("unsupported recognition engine: {other}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionCapability {
    TextDecode,
    DocumentLayout,
    TabularParse,
    ImageOcr,
}

impl RecognitionCapability {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TextDecode => "text_decode",
            Self::DocumentLayout => "document_layout",
            Self::TabularParse => "tabular_parse",
            Self::ImageOcr => "image_ocr",
        }
    }
}

impl fmt::Display for RecognitionCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecognitionStructureTier {
    Layout,
    Paragraph,
    Flat,
}

impl RecognitionStructureTier {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Layout => "layout",
            Self::Paragraph => "paragraph",
            Self::Flat => "flat",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecognitionProfile {
    pub capability: RecognitionCapability,
    pub engine: RecognitionEngine,
    pub structure_tier: RecognitionStructureTier,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(deny_unknown_fields)]
pub struct LibraryRecognitionPolicy {
    pub raster_image_engine: RecognitionEngine,
}

impl Default for LibraryRecognitionPolicy {
    fn default() -> Self {
        Self { raster_image_engine: default_raster_image_engine() }
    }
}

impl LibraryRecognitionPolicy {
    /// Builds a policy from persisted JSON.
    ///
    /// # Errors
    /// Returns an error when the JSON shape is not the canonical recognition policy shape.
    pub fn from_json(value: serde_json::Value) -> Result<Self, String> {
        let policy = serde_json::from_value::<Self>(value)
            .map_err(|error| format!("invalid recognition policy: {error}"))?;
        policy.validate()?;
        Ok(policy)
    }

    /// Serializes the policy into the canonical database/API JSON representation.
    ///
    /// # Errors
    /// Returns an error if serialization unexpectedly fails.
    pub fn to_json(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::to_value(self)
    }

    /// Validates policy engines against the support matrix exposed in this release.
    ///
    /// # Errors
    /// Returns a human-readable validation error when an engine is not allowed.
    pub fn validate(&self) -> Result<(), String> {
        match self.raster_image_engine {
            RecognitionEngine::Docling | RecognitionEngine::Vision => Ok(()),
            RecognitionEngine::Native => {
                Err("rasterImageEngine must be either docling or vision".to_string())
            }
        }
    }
}

const fn default_raster_image_engine() -> RecognitionEngine {
    RecognitionEngine::Docling
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_uses_docling_for_static_raster_images() {
        assert_eq!(
            LibraryRecognitionPolicy::default().raster_image_engine,
            RecognitionEngine::Docling
        );
    }

    #[test]
    fn policy_rejects_native_for_raster_images() {
        let policy = LibraryRecognitionPolicy { raster_image_engine: RecognitionEngine::Native };

        assert!(policy.validate().is_err());
    }

    #[test]
    fn policy_round_trips_camel_case_json() {
        let policy = LibraryRecognitionPolicy { raster_image_engine: RecognitionEngine::Vision };
        let json = policy.to_json().expect("policy should serialize");

        assert_eq!(json["rasterImageEngine"], serde_json::json!("vision"));
        assert_eq!(LibraryRecognitionPolicy::from_json(json).expect("policy should parse"), policy);
    }

    #[test]
    fn policy_rejects_unknown_json_fields() {
        let json = serde_json::json!({
            "rasterImageEngine": "docling",
            "legacyMode": "ignored",
        });

        assert!(LibraryRecognitionPolicy::from_json(json).is_err());
    }
}
