use super::{
    HashSet, LiteralKind, QueryAct, QueryIR, QueryTargetKind, RuntimeMatchedChunk,
    is_version_token_continuation,
};

pub(crate) fn build_exact_version_change_summary_answer(
    query_ir: &QueryIR,
    context_chunks: &[RuntimeMatchedChunk],
    graph_evidence_context_lines: &[String],
) -> Option<String> {
    if query_ir.source_slice.is_some()
        || !matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
    {
        return None;
    }
    if !query_ir.targets_any(&[
        QueryTargetKind::Version,
        QueryTargetKind::Release,
        QueryTargetKind::Changelog,
    ]) {
        return None;
    }
    let version_literals = query_ir
        .literal_constraints
        .iter()
        .filter(|literal| literal.kind == LiteralKind::Version)
        .map(|literal| literal.text.trim())
        .filter(|literal| !literal.is_empty())
        .collect::<Vec<_>>();
    if version_literals.is_empty() {
        return None;
    }

    exact_version_change_candidate_from_graph_lines(&version_literals, graph_evidence_context_lines)
        .or_else(|| exact_version_change_candidate_from_chunks(&version_literals, context_chunks))
        .and_then(render_exact_version_change_candidate)
}

#[derive(Debug)]
struct ExactVersionChangeCandidate {
    title: String,
    bullets: Vec<String>,
}

fn exact_version_change_candidate_from_graph_lines(
    version_literals: &[&str],
    graph_evidence_context_lines: &[String],
) -> Option<ExactVersionChangeCandidate> {
    graph_evidence_context_lines
        .iter()
        .filter_map(|line| {
            let (_header, body) = line.split_once('\n')?;
            if !contains_any_exact_version_literal(body, version_literals) {
                return None;
            }
            let title = body
                .lines()
                .map(str::trim)
                .find(|candidate| {
                    !candidate.is_empty()
                        && contains_any_exact_version_literal(candidate, version_literals)
                })
                .or_else(|| body.lines().map(str::trim).find(|candidate| !candidate.is_empty()))?
                .to_string();
            let bullets = collect_change_bullets(body);
            (bullets.len() >= 2).then_some(ExactVersionChangeCandidate { title, bullets })
        })
        .max_by(|left, right| {
            left.bullets
                .len()
                .cmp(&right.bullets.len())
                .then_with(|| right.title.len().cmp(&left.title.len()))
        })
}

fn exact_version_change_candidate_from_chunks(
    version_literals: &[&str],
    context_chunks: &[RuntimeMatchedChunk],
) -> Option<ExactVersionChangeCandidate> {
    let mut chunks = context_chunks
        .iter()
        .filter(|chunk| {
            contains_any_exact_version_literal(&chunk.document_label, version_literals)
                || contains_any_exact_version_literal(&chunk.source_text, version_literals)
        })
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        return None;
    }
    chunks.sort_by_key(|chunk| {
        (chunk.document_id, chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id)
    });

    let mut candidates = Vec::new();
    for document_chunks in chunks.chunk_by(|left, right| left.document_id == right.document_id) {
        let Some(first) = document_chunks.first() else {
            continue;
        };
        let mut bullets = Vec::new();
        let mut seen = HashSet::new();
        for chunk in document_chunks {
            for bullet in collect_change_bullets(&chunk.source_text) {
                if seen.insert(bullet.clone()) {
                    bullets.push(bullet);
                }
            }
        }
        if bullets.len() >= 2 {
            candidates
                .push(ExactVersionChangeCandidate { title: first.document_label.clone(), bullets });
        }
    }

    candidates.into_iter().max_by(|left, right| left.bullets.len().cmp(&right.bullets.len()))
}

fn render_exact_version_change_candidate(candidate: ExactVersionChangeCandidate) -> Option<String> {
    if candidate.bullets.is_empty() {
        return None;
    }
    let mut lines = vec![format!("**{}**", candidate.title.trim()), String::new()];
    for bullet in candidate.bullets.into_iter().take(16) {
        lines.push(format!("- {bullet}"));
    }
    Some(lines.join("\n"))
}

fn contains_any_exact_version_literal(text: &str, version_literals: &[&str]) -> bool {
    version_literals.iter().any(|literal| contains_exact_version_literal(text, literal))
}

fn contains_exact_version_literal(text: &str, literal: &str) -> bool {
    text.match_indices(literal).any(|(start, _)| {
        let end = start + literal.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        !before.is_some_and(is_version_token_continuation)
            && !after.is_some_and(is_version_token_continuation)
    })
}

fn collect_change_bullets(text: &str) -> Vec<String> {
    let mut bullets = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines().map(str::trim) {
        let bullet = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
            .map(str::trim);
        let Some(bullet) = bullet else {
            continue;
        };
        if bullet.is_empty() || bullet.starts_with("![") {
            continue;
        }
        let normalized = bullet.to_lowercase();
        if seen.insert(normalized) {
            bullets.push(bullet.to_string());
        }
    }
    bullets
}
