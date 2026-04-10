use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::{
    RuntimeMatchedChunk, concise_document_subject_label, document_local_focus_keywords,
    extract_protocol_literals, extract_url_literals, focused_excerpt_for,
    question_mentions_protocol, score_value, technical_chunk_selection_score,
    technical_literal_focus_keyword_segments, technical_literal_focus_keywords,
    technical_literal_focus_segments_text, trim_literal_token,
};

pub(super) fn build_graphql_absence_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !lowered.contains("graphql") {
        return None;
    }
    let has_graphql =
        chunks.iter().any(|chunk| chunk.source_text.to_lowercase().contains("graphql"));
    (!has_graphql)
        .then_some("В библиотеке нет описания GraphQL API или GraphQL endpoint.".to_string())
}

pub(super) fn question_mentions_port(question: &str) -> bool {
    question.to_lowercase().split(|ch: char| !ch.is_alphanumeric() && ch != '_').any(|token| {
        matches!(token, "port" | "ports" | "tcp_port" | "udp_port" | "порт" | "порта" | "порты")
    })
}

pub(super) fn extract_port_literals(text: &str, limit: usize) -> Vec<String> {
    let mut values = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();

    for url in extract_url_literals(text, limit) {
        let Some((_, remainder)) = url.split_once("://") else {
            continue;
        };
        let authority = remainder.split('/').next().unwrap_or_default();
        let Some((_, port)) = authority.rsplit_once(':') else {
            continue;
        };
        let port = port.trim();
        if (2..=5).contains(&port.len())
            && port.chars().all(|character| character.is_ascii_digit())
            && seen.insert(port.to_string())
        {
            values.push(port.to_string());
            if values.len() >= limit {
                return values;
            }
        }
    }

    let cleaned = text.replace('\n', " ");
    for separator in [":", "="] {
        for keyword in ["port", "tcp_port", "udp_port", "порт"] {
            let pattern = format!("{keyword}{separator}");
            for fragment in cleaned.match_indices(&pattern) {
                let value_start = fragment.0 + pattern.len();
                let suffix = cleaned[value_start..].trim_start();
                let digits = suffix
                    .chars()
                    .take_while(|character| character.is_ascii_digit())
                    .collect::<String>();
                if (2..=5).contains(&digits.len()) && seen.insert(digits.clone()) {
                    values.push(digits);
                    if values.len() >= limit {
                        return values;
                    }
                }
            }
        }
    }

    let tokens = cleaned.split_whitespace().collect::<Vec<_>>();
    for window in tokens.windows(2) {
        let keyword = trim_literal_token(window[0]).trim_matches(':');
        let value = trim_literal_token(window[1]).trim_matches(':');
        if ["port", "tcp_port", "udp_port", "порт"]
            .iter()
            .any(|candidate| keyword.eq_ignore_ascii_case(candidate))
            && (2..=5).contains(&value.len())
            && value.chars().all(|character| character.is_ascii_digit())
            && seen.insert(value.to_string())
        {
            values.push(value.to_string());
            if values.len() >= limit {
                return values;
            }
        }
    }

    values
}

pub(super) fn build_port_and_protocol_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_mentions_port(question)
        || !question_mentions_protocol(question)
        || chunks.is_empty()
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question);
    let focus_segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| {
            let keywords = technical_literal_focus_keywords(&segment);
            (segment, keywords)
        })
        .filter(|(_, keywords)| !keywords.is_empty())
        .collect::<Vec<_>>();
    if focus_segments.len() < 2 {
        return None;
    }

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let select_segment_document = |segment_keywords: &[String]| -> Option<Uuid> {
        ordered_document_ids
            .iter()
            .filter_map(|document_id| {
                let document_chunks = per_document_chunks.get(document_id)?;
                let best_chunk_score = document_chunks
                    .iter()
                    .map(|chunk| {
                        technical_chunk_selection_score(
                            &format!("{} {}", chunk.excerpt, chunk.source_text),
                            segment_keywords,
                            false,
                        )
                    })
                    .max()
                    .unwrap_or_default();
                (best_chunk_score > 0).then_some((best_chunk_score, *document_id))
            })
            .max_by(|left, right| {
                left.0.cmp(&right.0).then_with(|| {
                    let left_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &left.1)
                        .unwrap_or(usize::MAX);
                    let right_index = ordered_document_ids
                        .iter()
                        .position(|document_id| document_id == &right.1)
                        .unwrap_or(usize::MAX);
                    right_index.cmp(&left_index)
                })
            })
            .map(|(_, document_id)| document_id)
    };

    let mut port_line = None;
    let mut protocol_line = None;

    for (segment_text, segment_keywords) in focus_segments {
        let Some(document_id) = select_segment_document(&segment_keywords) else {
            continue;
        };
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords =
            document_local_focus_keywords(question, document_chunks, &question_keywords);
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                false,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                false,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let subject = concise_document_subject_label(&document_chunks[0].document_label);
        let mut ports = Vec::<String>::new();
        let mut protocols = Vec::<String>::new();
        let mut seen_ports = HashSet::<String>::new();
        let mut seen_protocols = HashSet::<String>::new();
        for chunk in ranked_chunks.iter().take(4) {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            if question_mentions_port(&segment_text) {
                for port in extract_port_literals(literal_source, 2) {
                    if seen_ports.insert(port.clone()) {
                        ports.push(port);
                    }
                }
            }
            if question_mentions_protocol(&segment_text) {
                for protocol in extract_protocol_literals(literal_source, 2) {
                    if seen_protocols.insert(protocol.clone()) {
                        protocols.push(protocol);
                    }
                }
            }
        }

        if port_line.is_none() && !ports.is_empty() {
            port_line = Some(format!("{subject}: port `{}`", ports[0]));
        }
        if protocol_line.is_none() && !protocols.is_empty() {
            protocol_line = Some(format!("{subject}: protocol `{}`", protocols[0]));
        }
    }

    match (port_line, protocol_line) {
        (Some(port), Some(protocol)) => Some(format!("{port}. {protocol}.")),
        _ => None,
    }
}

pub(super) fn build_port_answer(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<String> {
    if !question_mentions_port(question)
        || question_mentions_protocol(question)
        || technical_literal_focus_keyword_segments(question).len() > 1
        || chunks.is_empty()
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question);
    let focus_segments = technical_literal_focus_keyword_segments(question);
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    let scoped_document_ids = if focus_segments.is_empty() {
        ordered_document_ids.clone()
    } else {
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for segment_keywords in &focus_segments {
            let best_document = ordered_document_ids
                .iter()
                .filter_map(|document_id| {
                    let document_chunks = per_document_chunks.get(document_id)?;
                    let best_chunk_score = document_chunks
                        .iter()
                        .map(|chunk| {
                            technical_chunk_selection_score(
                                &format!("{} {}", chunk.excerpt, chunk.source_text),
                                segment_keywords,
                                false,
                            )
                        })
                        .max()
                        .unwrap_or_default();
                    (best_chunk_score > 0).then_some((best_chunk_score, *document_id))
                })
                .max_by(|left, right| {
                    left.0.cmp(&right.0).then_with(|| {
                        let left_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &left.1)
                            .unwrap_or(usize::MAX);
                        let right_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &right.1)
                            .unwrap_or(usize::MAX);
                        right_index.cmp(&left_index)
                    })
                });
            if let Some((_, document_id)) = best_document
                && seen.insert(document_id)
            {
                selected.push(document_id);
            }
        }
        if selected.is_empty() { ordered_document_ids.clone() } else { selected }
    };

    for document_id in scoped_document_ids {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords =
            document_local_focus_keywords(question, document_chunks, &question_keywords);
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                false,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                false,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let mut ports = Vec::<String>::new();
        let mut seen = HashSet::<String>::new();
        for chunk in ranked_chunks.iter().take(4) {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            for port in extract_port_literals(literal_source, 4) {
                if seen.insert(port.clone()) {
                    ports.push(port);
                }
            }
        }

        let subject = concise_document_subject_label(&document_chunks[0].document_label);
        if ports.is_empty() {
            if !focus_segments.is_empty() {
                return Some(format!(
                    "Точный порт для {subject} не подтвержден в выбранных доказательствах."
                ));
            }
            continue;
        }
        if ports.len() == 1 {
            return Some(format!(
                "Для {subject} в активной библиотеке найден порт `{}`.",
                ports[0]
            ));
        }

        let rendered_ports =
            ports.iter().map(|port| format!("`{port}`")).collect::<Vec<_>>().join(", ");
        return Some(format!(
            "Для {subject} в активной библиотеке найдены порты {rendered_ports}."
        ));
    }

    None
}
