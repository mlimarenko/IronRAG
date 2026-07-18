use super::super::{FactCandidate, StructuredBlockData};

pub(crate) const fn extract_error_code_candidates(
    _block: &StructuredBlockData,
    _line: &str,
) -> Vec<FactCandidate> {
    // There is no domain-neutral error-code grammar: shapes such as an
    // uppercase prefix or a leading letter plus digits are also ordinary
    // product, ticket, and model identifiers. Typed classifier or parser
    // evidence must provide the missing semantics; deterministic extraction
    // conservatively abstains.
    Vec::new()
}
