use serde::{Deserialize, Serialize};

pub mod docx;
pub mod image;
pub mod pdf;
pub mod text_like;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionOutput {
    pub extraction_kind: String,
    pub content_text: String,
    pub page_count: Option<u32>,
    pub warnings: Vec<String>,
    pub source_map: serde_json::Value,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
}
