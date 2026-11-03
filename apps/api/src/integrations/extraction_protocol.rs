//! Shared wire contract for the extraction child and its application caller.
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const CONTRACT: &str = "ironrag-extraction/3";
pub const PRODUCER: &str = "ironrag-extract";
pub const ENGINE: &str = "xberg/1.3.11+git.b27b2d848048df778bf32be96fa2cfcccbf3d6ce";
pub const PROFILE: &str = "native-formats/vision-inputs/1";
// Process safety ceilings, not the application's per-request admission policy.
pub const MAX_INPUT: usize = 512 * 1024 * 1024;
pub const MAX_OUTPUT: usize = 128 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 25_000_000;
pub const MAX_UNITS: u32 = 100_000;
pub const MAX_TEXT: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub input_bytes: u64,
    pub output_bytes: usize,
    pub text_bytes: usize,
    pub units: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            input_bytes: 50 * 1024 * 1024,
            output_bytes: 64 * 1024 * 1024,
            text_bytes: 32 * 1024 * 1024,
            units: 10_000,
        }
    }
}

impl Limits {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (1..=MAX_INPUT as u64).contains(&self.input_bytes),
            "input_limit_out_of_bounds"
        );
        anyhow::ensure!(
            (4096..=MAX_OUTPUT).contains(&self.output_bytes),
            "output_limit_out_of_bounds"
        );
        anyhow::ensure!(
            self.text_bytes > 0
                && self.text_bytes <= MAX_TEXT
                && self.text_bytes <= self.output_bytes,
            "text_limit_out_of_bounds"
        );
        anyhow::ensure!((1..=MAX_UNITS).contains(&self.units), "unit_limit_out_of_bounds");
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub path: PathBuf,
    pub operation: Operation,
    pub mime: String,
    pub source_sha256: String,
    pub limits: Limits,
    #[serde(default)]
    pub start: Option<u32>,
    #[serde(default)]
    pub end: Option<u32>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Count,
    Extract,
}

impl Request {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(self.path.is_absolute(), "input path must be absolute");
        self.limits.validate()?;
        anyhow::ensure!(
            !self.mime.is_empty()
                && self.mime.len() <= 256
                && self.mime.is_ascii()
                && !self.mime.chars().any(char::is_control),
            "invalid_mime"
        );
        anyhow::ensure!(
            self.source_sha256.len() == 64
                && self
                    .source_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid_source_sha256"
        );
        anyhow::ensure!(self.start.is_some() == self.end.is_some(), "range requires start and end");
        if let (Some(start), Some(end)) = (self.start, self.end) {
            anyhow::ensure!(
                start >= 1 && end >= start && end - start < self.limits.units,
                "invalid_range"
            );
        }
        anyhow::ensure!(
            self.operation != Operation::Count || self.start.is_none(),
            "count does not accept a range"
        );
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Response {
    pub contract: String,
    pub producer: String,
    pub engine: String,
    pub profile: String,
    pub status: String,
    pub total_milliseconds: u128,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Extraction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

impl Response {
    pub fn new(
        result: anyhow::Result<Extraction>,
        duration: Duration,
        error_code: Option<String>,
    ) -> Self {
        let (status, result, error) = match result {
            Ok(value) => ("success", Some(value), None),
            Err(error) => ("error", None, Some(format!("{error:#}").chars().take(4096).collect())),
        };
        Self {
            contract: CONTRACT.into(),
            producer: PRODUCER.into(),
            engine: ENGINE.into(),
            profile: PROFILE.into(),
            status: status.into(),
            total_milliseconds: duration.as_millis(),
            result,
            error,
            error_code,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Extraction {
    pub source_sha256: String,
    pub text_byte_limit: usize,
    pub source_unit_count: u32,
    pub unit_kind: String,
    pub selected_start: u32,
    pub selected_end: u32,
    pub content: String,
    pub units: Vec<Unit>,
    pub visual_inputs: Vec<VisualInput>,
    pub warnings: Vec<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Unit {
    pub ordinal: u32,
    pub sheet_name: Option<String>,
    pub content: String,
    pub tables: serde_json::Value,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisualInputKind {
    PdfPage,
    EmbeddedPicture,
    RasterFrame,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VisualInput {
    pub kind: VisualInputKind,
    pub ordinal: usize,
    pub native_index: u32,
    pub source_unit: u32,
    pub source_path: Option<String>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_extent_96_dpi: Option<DisplayExtent>,
    pub content_base64: String,
    pub payload_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayExtent {
    pub width: u32,
    pub height: u32,
}
