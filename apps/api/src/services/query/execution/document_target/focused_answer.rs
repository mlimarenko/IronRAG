use super::matching::{DOMINANT_DOCUMENT_SCORE_MULTIPLIER, KNOWN_DOCUMENT_LABEL_EXTENSIONS};
use super::{
    HashMap, RuntimeMatchedChunk, Uuid, current_question_segment,
    explicit_target_document_ids_from_values, question_requests_multi_document_scope, score_value,
};

pub(crate) fn focused_answer_document_id(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    let question = current_question_segment(question);
    if chunks.is_empty() || question_requests_multi_document_scope(question, None) {
        return None;
    }

    let explicit_targets = explicit_target_document_ids_from_values(
        question,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    if explicit_targets.len() == 1 {
        return explicit_targets.iter().next().copied();
    }

    #[derive(Debug, Clone)]
    struct DocumentFocusScore {
        document_id: Uuid,
        document_label: String,
        score_sum: f32,
        chunk_count: usize,
        first_rank: usize,
        label_keyword_hits: usize,
        label_marker_hits: usize,
    }

    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let mut per_document = HashMap::<Uuid, DocumentFocusScore>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        let lowered_label = chunk.document_label.to_lowercase();
        let entry = per_document.entry(chunk.document_id).or_insert_with(|| DocumentFocusScore {
            document_id: chunk.document_id,
            document_label: chunk.document_label.clone(),
            score_sum: 0.0,
            chunk_count: 0,
            first_rank: rank,
            label_keyword_hits: question_keywords
                .iter()
                .filter(|keyword| lowered_label.contains(keyword.as_str()))
                .count(),
            label_marker_hits: document_focus_marker_hits(question, &chunk.document_label),
        });
        entry.score_sum += score_value(chunk.score);
        entry.chunk_count += 1;
        entry.first_rank = entry.first_rank.min(rank);
    }

    let mut ranked = per_document.into_values().collect::<Vec<_>>();
    if ranked.is_empty() {
        return None;
    }
    ranked.sort_by(|left, right| {
        right.label_marker_hits.cmp(&left.label_marker_hits).then_with(|| {
            right
                .score_sum
                .total_cmp(&left.score_sum)
                .then_with(|| right.chunk_count.cmp(&left.chunk_count))
                .then_with(|| right.label_keyword_hits.cmp(&left.label_keyword_hits))
                .then_with(|| left.first_rank.cmp(&right.first_rank))
                .then_with(|| left.document_label.cmp(&right.document_label))
        })
    });

    if ranked.len() == 1 {
        return Some(ranked[0].document_id);
    }

    let top = &ranked[0];
    let second = &ranked[1];
    if top.label_marker_hits > second.label_marker_hits && top.label_marker_hits > 0 {
        return Some(top.document_id);
    }

    let has_explicit_single_source_anchor = question_mentions_single_source_anchor(question);
    let materially_higher_score =
        top.score_sum >= second.score_sum * DOMINANT_DOCUMENT_SCORE_MULTIPLIER;
    let materially_more_chunks = top.chunk_count > second.chunk_count;
    let stronger_label_match = top.label_keyword_hits > second.label_keyword_hits;

    if has_explicit_single_source_anchor
        || materially_higher_score
        || materially_more_chunks
        || stronger_label_match
    {
        Some(top.document_id)
    } else {
        None
    }
}

pub(crate) fn document_focus_marker_hits(question: &str, document_label: &str) -> usize {
    let lowered_question = current_question_segment(question).to_lowercase();
    document_label_focus_markers(document_label)
        .into_iter()
        .filter(|marker| question_mentions_document_marker(&lowered_question, marker))
        .count()
}

pub(crate) fn concise_document_subject_label(document_label: &str) -> String {
    let normalized = strip_known_document_label_extension(
        document_label
            .split(" - ")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(document_label),
    )
    .replace(['_', '-'], " ");
    let normalized = normalized.trim();
    if normalized.is_empty() {
        return document_label.to_string();
    }

    normalized
        .split_whitespace()
        .enumerate()
        .map(|(index, word)| format_document_label_word(word, index == 0))
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_known_document_label_extension(document_label: &str) -> &str {
    let trimmed = document_label.trim();
    let Some((stem, extension)) = trimmed.rsplit_once('.') else {
        return trimmed;
    };
    let lowered_extension = extension.to_ascii_lowercase();
    if KNOWN_DOCUMENT_LABEL_EXTENSIONS.contains(&lowered_extension.as_str()) {
        stem
    } else {
        trimmed
    }
}

fn document_label_focus_markers(document_label: &str) -> Vec<&'static str> {
    let lowered_label = document_label.to_lowercase();
    let mut markers = Vec::new();
    if let Some(extension_marker) = document_label_extension_marker(&lowered_label) {
        markers.push(extension_marker);
    }
    markers
}

fn document_label_extension_marker(lowered_label: &str) -> Option<&'static str> {
    let (_, extension) = lowered_label.rsplit_once('.')?;
    match extension {
        "pdf" => Some("pdf"),
        "docx" => Some("docx"),
        "csv" => Some("csv"),
        "tsv" => Some("tsv"),
        "xls" => Some("xls"),
        "xlsx" => Some("xlsx"),
        "xlsb" => Some("xlsb"),
        "ods" => Some("ods"),
        "pptx" => Some("pptx"),
        "png" => Some("png"),
        "jpg" => Some("jpg"),
        "jpeg" => Some("jpeg"),
        _ => None,
    }
}

fn question_mentions_document_marker(lowered_question: &str, marker: &str) -> bool {
    let extension_marker = format!(".{marker}");
    let extension_match = lowered_question.match_indices(&extension_marker).any(|(start, _)| {
        let end = start + extension_marker.len();
        lowered_question[end..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric())
    });
    extension_match
        || lowered_question
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| token == marker)
}

fn question_mentions_single_source_anchor(question: &str) -> bool {
    let _ = question;
    false
}

fn format_document_label_word(word: &str, is_first: bool) -> String {
    if word.is_empty() {
        return String::new();
    }
    if document_word_has_explicit_source_casing(word) {
        return word.to_string();
    }

    let lowered = word.to_lowercase();
    if !is_first {
        return lowered;
    }
    let mut chars = lowered.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

fn document_word_has_explicit_source_casing(word: &str) -> bool {
    let mut cased_characters =
        word.chars().filter(|character| character.is_uppercase() || character.is_lowercase());
    cased_characters.next().is_some() && cased_characters.any(|character| character.is_uppercase())
}
