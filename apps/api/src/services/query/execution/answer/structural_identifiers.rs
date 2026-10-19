use super::{
    BTreeSet, QueryAct, QueryIR, RuntimeMatchedChunk, focused_answer_document_id,
    normalized_alnum_tokens,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StructuralIdentifier {
    qualified: String,
    leaf: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct StructuralIdentifierMapping {
    source: StructuralIdentifier,
    targets: Vec<StructuralIdentifier>,
}

pub(crate) fn build_structural_identifier_mapping_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let texts = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none_or(|document_id| chunk.document_id == document_id)
        })
        .flat_map(|chunk| [chunk.source_text.as_str(), chunk.excerpt.as_str()]);
    let mapping = focused_structural_identifier_mapping(question, query_ir, texts)?;
    Some(render_structural_identifier_mapping(&mapping))
}

pub(crate) fn answer_omits_structural_identifier_mapping_target(
    question: &str,
    answer: &str,
    query_ir: &QueryIR,
    prompt_context: &str,
) -> bool {
    let Some(mapping) =
        focused_structural_identifier_mapping(question, query_ir, std::iter::once(prompt_context))
    else {
        return false;
    };
    let answer_tokens = normalized_alnum_tokens(answer, 1);
    mapping.targets.iter().any(|target| !answer_tokens.contains(&target.leaf.to_lowercase()))
}

fn focused_structural_identifier_mapping<'a>(
    question: &str,
    query_ir: &QueryIR,
    texts: impl Iterator<Item = &'a str>,
) -> Option<StructuralIdentifierMapping> {
    if query_ir.source_slice.is_some() || !matches!(query_ir.act, QueryAct::Enumerate) {
        return None;
    }
    let all_mappings = texts
        .flat_map(structural_identifier_mappings)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let question_tokens = normalized_alnum_tokens(question, 1);
    let focus_tokens = if all_mappings
        .iter()
        .any(|mapping| question_tokens.contains(&mapping.source.leaf.to_lowercase()))
    {
        question_tokens
    } else {
        structural_identifier_ir_focus_tokens(query_ir)
    };
    let mut mappings = all_mappings
        .into_iter()
        .filter(|mapping| focus_tokens.contains(&mapping.source.leaf.to_lowercase()))
        .collect::<Vec<_>>();
    let largest_target_count = mappings.iter().map(|mapping| mapping.targets.len()).max()?;
    mappings.retain(|mapping| mapping.targets.len() == largest_target_count);
    (mappings.len() == 1).then(|| mappings.remove(0))
}

fn structural_identifier_ir_focus_tokens(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    for value in query_ir
        .target_entities
        .iter()
        .map(|entity| entity.label.as_str())
        .chain(query_ir.literal_constraints.iter().map(|literal| literal.text.as_str()))
        .chain(query_ir.retrieval_query.as_deref())
    {
        tokens.extend(normalized_alnum_tokens(value, 1));
    }
    tokens
}

fn structural_identifier_mappings(text: &str) -> Vec<StructuralIdentifierMapping> {
    text.lines().filter_map(parse_structural_identifier_mapping).collect()
}

fn parse_structural_identifier_mapping(line: &str) -> Option<StructuralIdentifierMapping> {
    let (left, right) = line.split_once("=>")?;
    if !right.contains('[') || !right.contains(']') {
        return None;
    }
    let source = qualified_identifiers(left).into_iter().next_back()?;
    let source_prefix = structural_identifier_prefix(&source.qualified)?;
    let mut targets = qualified_identifiers(right)
        .into_iter()
        .filter(|target| {
            structural_identifier_prefix(&target.qualified) == Some(source_prefix)
                && target.leaf != source.leaf
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    targets.sort_by_key(|target| right.find(&target.qualified).unwrap_or(usize::MAX));
    (targets.len() >= 2).then_some(StructuralIdentifierMapping { source, targets })
}

fn qualified_identifiers(text: &str) -> Vec<StructuralIdentifier> {
    text.split(|character: char| {
        !character.is_alphanumeric() && character != '_' && character != ':'
    })
    .filter_map(|candidate| {
        let qualified = candidate.trim_matches(':');
        let segments = qualified.split("::").collect::<Vec<_>>();
        if segments.len() < 2 || segments.iter().any(|segment| !identifier_segment(segment)) {
            return None;
        }
        Some(StructuralIdentifier {
            qualified: qualified.to_string(),
            leaf: segments.last()?.to_string(),
        })
    })
    .collect()
}

fn identifier_segment(segment: &str) -> bool {
    let mut characters = segment.chars();
    characters.next().is_some_and(|character| character.is_alphabetic() || character == '_')
        && characters.all(|character| character.is_alphanumeric() || character == '_')
}

fn structural_identifier_prefix(qualified: &str) -> Option<&str> {
    qualified.rsplit_once("::").map(|(prefix, _)| prefix)
}

fn render_structural_identifier_mapping(mapping: &StructuralIdentifierMapping) -> String {
    let mut lines = vec![format!("`{}`:", mapping.source.qualified)];
    lines.extend(mapping.targets.iter().map(|target| format!("- `{}`", target.qualified)));
    lines.join("\n")
}
