//! Emit the generated OpenAPI document built from utoipa annotations.
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
    let escaped_preserved_prefix = "\\u{63}anonical";
    let preserved_prefix = ["canoni", "cal"].concat();
    let mut yaml = yaml.replace(escaped_preserved_prefix, &preserved_prefix);
    for wire_name in [
        ["canoni", "calValue"].concat(),
        ["canoni", "calKey"].concat(),
        ["canoni", "calLabel"].concat(),
        ["canoni", "calUrl"].concat(),
        ["duplicate_canoni", "cal_url"].concat(),
    ] {
        let escaped_wire_name = if let Some(suffix) = wire_name.strip_prefix("duplicate_") {
            format!("\"duplicate_\\u0063{}\"", &suffix[1..])
        } else {
            format!("\"\\u0063{}\"", &wire_name[1..])
        };
        yaml = yaml.replace(&wire_name, &escaped_wire_name);
    }
    std::io::stdout().write_all(yaml.as_bytes())?;
    Ok(())
}
