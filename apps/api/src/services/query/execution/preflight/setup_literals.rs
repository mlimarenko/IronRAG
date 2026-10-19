use super::document_scope::preflight_target_label_keywords;
use super::{
    HashMap, HashSet, QueryIR, RuntimeMatchedChunk, Uuid, document_local_focus_keywords,
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
    score_value, technical_chunk_selection_score, technical_keyword_weight,
    technical_literal_focus_keywords,
};

#[derive(Debug)]
struct SetupLiteralDocumentCandidate {
    document_id: Uuid,
    label_score: usize,
    setup_anchor_score: usize,
    setup_score: usize,
    best_chunk_signal: isize,
    retrieval_score_sum: f32,
    first_rank: usize,
}

pub(super) fn select_setup_literal_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    setup_literal_document_candidates(question, query_ir, chunks)
        .first()
        .map(|candidate| candidate.document_id)
}

pub(super) fn select_setup_literal_document_ids(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    limit: usize,
) -> Vec<Uuid> {
    setup_literal_document_candidates(question, query_ir, chunks)
        .into_iter()
        .take(limit)
        .map(|candidate| candidate.document_id)
        .collect()
}

fn setup_literal_document_candidates(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SetupLiteralDocumentCandidate> {
    if chunks.is_empty() {
        return Vec::new();
    }

    let label_keywords = preflight_target_label_keywords(query_ir);
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = false;
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let mut candidates = ordered_document_ids
        .iter()
        .enumerate()
        .filter_map(|(first_rank, document_id)| {
            let document_chunks = per_document_chunks.get(document_id)?;
            let document_label = document_chunks.first()?.document_label.to_lowercase();
            let label_score = label_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_label, keyword))
                .sum::<usize>();
            let local_keywords = document_local_focus_keywords(
                question,
                Some(query_ir),
                document_chunks,
                &question_keywords,
            );
            let mut setup_anchor_score = 0usize;
            let mut setup_score = 0usize;
            let mut best_chunk_signal = isize::MIN;
            let mut retrieval_score_sum = 0.0f32;
            for chunk in document_chunks {
                let text = format!("{} {}", chunk.excerpt, chunk.source_text);
                let chunk_setup_score = setup_literal_chunk_score(&text);
                setup_anchor_score =
                    setup_anchor_score.saturating_add(chunk_setup_score.anchor_score);
                setup_score = setup_score.saturating_add(chunk_setup_score.total_score);
                best_chunk_signal = best_chunk_signal.max(technical_chunk_selection_score(
                    &text,
                    &local_keywords,
                    pagination_requested,
                ));
                retrieval_score_sum += score_value(chunk.score);
            }
            (label_score > 0 || setup_score > 0).then_some(SetupLiteralDocumentCandidate {
                document_id: *document_id,
                label_score,
                setup_anchor_score,
                setup_score,
                best_chunk_signal,
                retrieval_score_sum,
                first_rank,
            })
        })
        .collect::<Vec<_>>();
    if candidates.iter().any(|candidate| candidate.setup_anchor_score > 0) {
        candidates.retain(|candidate| candidate.setup_anchor_score > 0);
    } else if candidates.iter().any(|candidate| candidate.setup_score > 0) {
        candidates.retain(|candidate| candidate.setup_score > 0);
    }

    candidates.sort_by(|left, right| {
        right
            .label_score
            .cmp(&left.label_score)
            .then_with(|| right.setup_anchor_score.cmp(&left.setup_anchor_score))
            .then_with(|| right.setup_score.cmp(&left.setup_score))
            .then_with(|| right.best_chunk_signal.cmp(&left.best_chunk_signal))
            .then_with(|| right.retrieval_score_sum.total_cmp(&left.retrieval_score_sum))
            .then_with(|| left.first_rank.cmp(&right.first_rank))
            .then_with(|| left.document_id.cmp(&right.document_id))
    });
    candidates
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct SetupLiteralChunkScore {
    pub(super) anchor_score: usize,
    pub(super) total_score: usize,
}

pub(super) fn setup_literal_chunk_score(text: &str) -> SetupLiteralChunkScore {
    let package_score = extract_package_command_literals(text, 4).len().saturating_mul(16);
    let path_score = setup_literal_configuration_path_count(text).saturating_mul(24);
    let assignment_score = setup_literal_assignment_count(text).saturating_mul(10);
    let section_score = setup_literal_section_count(text).saturating_mul(8);
    let parameter_score = extract_parameter_literals(text, 32).len().saturating_mul(3);
    let anchor_score = package_score
        .saturating_add(path_score)
        .saturating_add(assignment_score)
        .saturating_add(section_score);
    SetupLiteralChunkScore {
        anchor_score,
        total_score: anchor_score.saturating_add(parameter_score),
    }
}

pub(super) fn setup_literal_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 16)
        .into_iter()
        .filter(|path| setup_literal_path_has_configuration_extension(path))
        .count()
}

fn setup_literal_path_has_configuration_extension(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
        .iter()
        .any(|extension| lowered.ends_with(extension))
}

pub(super) fn setup_literal_assignment_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let Some((name, _)) = token.split_once('=') else {
                return false;
            };
            let name = name.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}')
            });
            let Some(first) = name.chars().next() else {
                return false;
            };
            first.is_ascii_alphabetic()
                && name.chars().any(|ch| ch.is_ascii_alphabetic())
                && name
                    .chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        })
        .take(16)
        .count()
}

pub(super) fn setup_literal_section_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let cleaned = token.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '{' | '}')
            });
            cleaned.len() > 2 && cleaned.starts_with('[') && cleaned.ends_with(']')
        })
        .take(16)
        .count()
}

pub(super) fn append_setup_literal_chunks(
    selected: &mut Vec<RuntimeMatchedChunk>,
    candidate_chunks: &[RuntimeMatchedChunk],
    max_total_chunks: usize,
) {
    if selected.len() >= max_total_chunks {
        return;
    }
    let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut candidates = candidate_chunks
        .iter()
        .filter(|chunk| !selected_ids.contains(&chunk.chunk_id))
        .filter_map(|chunk| {
            let package_count = extract_package_command_literals(&chunk.source_text, 2).len();
            let config_path_count = extract_explicit_path_literals(&chunk.source_text, 4)
                .into_iter()
                .filter(|path| {
                    let lowered = path.to_ascii_lowercase();
                    lowered.ends_with(".conf") || lowered.ends_with(".ini")
                })
                .count();
            (package_count > 0 && config_path_count > 0).then_some((
                package_count,
                config_path_count,
                chunk,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(
        |(left_packages, left_paths, left), (right_packages, right_paths, right)| {
            right_packages
                .cmp(left_packages)
                .then_with(|| right_paths.cmp(left_paths))
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
                .then_with(|| left.document_id.cmp(&right.document_id))
                .then_with(|| left.chunk_index.cmp(&right.chunk_index))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        },
    );
    for (_, _, chunk) in candidates {
        if selected.len() >= max_total_chunks {
            break;
        }
        selected.push(chunk.clone());
    }
    if selected.len() >= max_total_chunks {
        return;
    }

    let selected_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let selected_documents = selected.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>();
    let mut parameter_candidates = candidate_chunks
        .iter()
        .filter(|chunk| {
            selected_documents.is_empty() || selected_documents.contains(&chunk.document_id)
        })
        .filter(|chunk| !selected_ids.contains(&chunk.chunk_id))
        .filter_map(|chunk| {
            let parameter_count = extract_parameter_literals(&chunk.source_text, 16).len();
            (parameter_count > 0).then_some((parameter_count, chunk))
        })
        .collect::<Vec<_>>();
    parameter_candidates.sort_by(|(left_count, left), (right_count, right)| {
        right_count
            .cmp(left_count)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for (_, chunk) in parameter_candidates {
        if selected.len() >= max_total_chunks {
            break;
        }
        selected.push(chunk.clone());
    }
}
