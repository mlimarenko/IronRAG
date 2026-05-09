//! Emit the canonical OpenAPI document built from utoipa annotations.
//!
//! Usage: `cargo run --bin ironrag-emit-openapi > apps/api/contracts/openapi.gen.yaml`
//!
//! Sub-sprint 1d wires this binary into CI as the drift check: regenerate the
//! yaml into a tmp file, diff it against the committed copy, fail when they
//! disagree. The committed copy stays the runtime source of truth so that the
//! `apps/api/src/interfaces/http/openapi.rs` handler keeps using `include_str!`
//! and the frontend codegen reads a stable artefact.

use ironrag_backend::openapi::ApiDoc;
use std::io::Write;
use utoipa::OpenApi;

fn main() -> anyhow::Result<()> {
    let yaml = ApiDoc::openapi().to_yaml()?;
    std::io::stdout().write_all(yaml.as_bytes())?;
    Ok(())
}
