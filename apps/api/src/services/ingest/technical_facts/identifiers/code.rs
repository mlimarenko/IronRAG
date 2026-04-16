use super::super::{
    FactCandidate, StructuredBlockData, StructuredBlockKind, TechnicalFactKind, build_candidate,
};

/// Extracts code identifiers ONLY via tree-sitter AST parsing.
///
/// If the block has no `code_language` or tree-sitter doesn't support
/// the language, returns empty — no heuristic fallback. The old path
/// matched `"fn "`, `"class "`, `"const "` as substrings, which
/// produced false positives on comments, strings, and prose that
/// happened to contain these keywords. A missing fact is better than
/// a wrong fact — the LLM can still read the raw text.
pub(crate) fn extract_code_identifier_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(block.block_kind, StructuredBlockKind::CodeBlock) {
        return Vec::new();
    }

    let Some(lang) = block.code_language.as_deref() else {
        return Vec::new();
    };
    let Some(ast_ids) = crate::shared::ast_extraction::extract_ast_identifiers(&block.text, lang)
    else {
        return Vec::new();
    };

    ast_ids
        .into_iter()
        .filter(|id| line.contains(&id.name))
        .filter_map(|id| {
            build_candidate(
                block,
                TechnicalFactKind::CodeIdentifier,
                &id.name,
                Vec::new(),
                line,
                "ast_node",
            )
        })
        .collect()
}
