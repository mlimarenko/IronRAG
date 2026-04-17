use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::shared::extraction::technical_facts::TechnicalFactKind;

use super::concise_document_subject_label;
use super::fact_lookup::{best_matching_fact, build_document_labels};
use super::question_intent::question_mentions_port;
use super::technical_literals::{
    question_mentions_protocol, technical_chunk_selection_score,
    technical_literal_focus_keyword_segments, technical_literal_focus_keywords,
    technical_literal_focus_segments_text,
};
use super::types::RuntimeMatchedChunk;
use super::{CanonicalAnswerEvidence, technical_answer::document_focus_preference};

#[cfg(test)]
use super::retrieve::{focused_excerpt_for, score_value};
#[cfg(test)]
use super::technical_literal_extractors::extract_protocol_literals;
#[cfg(test)]
use super::technical_literals::{
    document_local_focus_keywords, extract_url_literals, trim_literal_token,
};

fn fact_kind_matches(
    fact: &crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow,
    kind: TechnicalFactKind,
) -> bool {
    fact.fact_kind.parse::<TechnicalFactKind>().ok() == Some(kind)
}

fn chunks_by_document(
    chunks: &[RuntimeMatchedChunk],
) -> (Vec<Uuid>, HashMap<Uuid, Vec<&RuntimeMatchedChunk>>) {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    (ordered_document_ids, per_document_chunks)
}

fn select_segment_document(
    ordered_document_ids: &[Uuid],
    per_document_chunks: &HashMap<Uuid, Vec<&RuntimeMatchedChunk>>,
    segment_keywords: &[String],
) -> Option<Uuid> {
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
}

fn select_port_scope_ids(
    ordered_document_ids: &[Uuid],
    per_document_chunks: &HashMap<Uuid, Vec<&RuntimeMatchedChunk>>,
    focus_segments: &[Vec<String>],
) -> Vec<Uuid> {
    if focus_segments.is_empty() {
        return ordered_document_ids.to_vec();
    }

    let mut selected = Vec::new();
    let mut seen = HashSet::new();
    for segment_keywords in focus_segments {
        if let Some(document_id) =
            select_segment_document(ordered_document_ids, per_document_chunks, segment_keywords)
            && seen.insert(document_id)
        {
            selected.push(document_id);
        }
    }

    if selected.is_empty() { ordered_document_ids.to_vec() } else { selected }
}

fn collect_document_fact_values(
    evidence: &CanonicalAnswerEvidence,
    document_id: Uuid,
    kind: TechnicalFactKind,
) -> Vec<String> {
    evidence
        .technical_facts
        .iter()
        .filter(|fact| fact.document_id == document_id && fact_kind_matches(fact, kind))
        .map(|fact| fact.display_value.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn protocol_specificity_rank(protocol: &str) -> usize {
    match protocol.to_ascii_lowercase().as_str() {
        "graphql" | "soap" | "rest" | "grpc" | "websocket" | "ws" | "wss" => 2,
        "http" | "https" | "tcp" | "udp" => 1,
        _ => 0,
    }
}

fn best_document_protocol(
    evidence: &CanonicalAnswerEvidence,
    document_labels: &HashMap<Uuid, String>,
    document_id: Uuid,
    segment_keywords: &[String],
) -> Option<String> {
    let document_label = document_labels.get(&document_id).map(String::as_str).unwrap_or_default();
    best_matching_fact(
        evidence,
        document_labels,
        TechnicalFactKind::Protocol,
        |fact| fact.document_id == document_id,
        |fact, _| {
            let lowered_value = fact.display_value.to_ascii_lowercase();
            let lowered_label = document_label.to_ascii_lowercase();
            protocol_specificity_rank(&fact.display_value) * 100
                + segment_keywords
                    .iter()
                    .map(|keyword| {
                        usize::from(lowered_label.contains(keyword)) * 20
                            + usize::from(lowered_value.contains(keyword)) * 8
                    })
                    .sum::<usize>()
        },
    )
    .map(|matched| matched.fact.display_value.to_ascii_uppercase())
}

fn port_fact_score(
    port: &str,
    document_label: &str,
    candidate_document_id: Uuid,
    focused_document_id: Option<Uuid>,
    question_keywords: &[String],
) -> usize {
    let lowered_port = port.to_ascii_lowercase();
    let lowered_label = document_label.to_ascii_lowercase();
    usize::try_from(document_focus_preference(candidate_document_id, focused_document_id))
        .unwrap_or_default()
        + question_keywords
            .iter()
            .map(|keyword| {
                usize::from(lowered_label.contains(keyword)) * 20
                    + usize::from(lowered_port.contains(keyword)) * 8
            })
            .sum::<usize>()
}

pub(super) fn build_port_and_protocol_answer_from_facts(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_mentions_port(question)
        || !question_mentions_protocol(question)
        || chunks.is_empty()
    {
        return None;
    }

    let focus_segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| {
            let keywords = technical_literal_focus_keywords(&segment, None);
            (segment, keywords)
        })
        .filter(|(_, keywords)| !keywords.is_empty())
        .collect::<Vec<_>>();
    if focus_segments.len() < 2 {
        return None;
    }

    let (ordered_document_ids, per_document_chunks) = chunks_by_document(chunks);
    let document_labels = build_document_labels(chunks);
    let mut port_line = None;
    let mut protocol_line = None;

    for (segment_text, segment_keywords) in focus_segments {
        let Some(document_id) =
            select_segment_document(&ordered_document_ids, &per_document_chunks, &segment_keywords)
        else {
            continue;
        };
        let document_label =
            document_labels.get(&document_id).map(String::as_str).unwrap_or_default();
        let subject = concise_document_subject_label(document_label);

        if port_line.is_none() && question_mentions_port(&segment_text) {
            if let Some(port) =
                collect_document_fact_values(evidence, document_id, TechnicalFactKind::Port)
                    .into_iter()
                    .next()
            {
                port_line = Some(format!("{subject}: port `{port}`"));
            }
        }

        if protocol_line.is_none() && question_mentions_protocol(&segment_text) {
            if let Some(protocol) =
                best_document_protocol(evidence, &document_labels, document_id, &segment_keywords)
            {
                protocol_line = Some(format!("{subject}: protocol `{protocol}`"));
            }
        }
    }

    match (port_line, protocol_line) {
        (Some(port), Some(protocol)) => Some(format!("{port}. {protocol}.")),
        _ => None,
    }
}

pub(super) fn build_port_answer_from_facts(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_mentions_port(question)
        || question_mentions_protocol(question)
        || technical_literal_focus_keyword_segments(question, None).len() > 1
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, None);
    let focus_segments = technical_literal_focus_keyword_segments(question, None);
    let (ordered_document_ids, per_document_chunks) = chunks_by_document(chunks);
    let document_labels = build_document_labels(chunks);
    let focused_document_id = if focus_segments.len() == 1 {
        select_segment_document(&ordered_document_ids, &per_document_chunks, &focus_segments[0])
    } else {
        None
    };
    let scoped_document_ids =
        select_port_scope_ids(&ordered_document_ids, &per_document_chunks, &focus_segments);

    for document_id in scoped_document_ids {
        let document_label =
            document_labels.get(&document_id).map(String::as_str).unwrap_or_default();
        let mut ports =
            collect_document_fact_values(evidence, document_id, TechnicalFactKind::Port);
        ports.sort_by(|left, right| {
            port_fact_score(
                right,
                document_label,
                document_id,
                focused_document_id,
                &question_keywords,
            )
            .cmp(&port_fact_score(
                left,
                document_label,
                document_id,
                focused_document_id,
                &question_keywords,
            ))
            .then_with(|| left.cmp(right))
        });

        let subject = concise_document_subject_label(document_label);
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

#[cfg(test)]
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

#[cfg(test)]
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

    let question_keywords = technical_literal_focus_keywords(question, None);
    let focus_segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| {
            let keywords = technical_literal_focus_keywords(&segment, None);
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
            document_local_focus_keywords(question, None, document_chunks, &question_keywords);
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

#[cfg(test)]
pub(super) fn build_port_answer(question: &str, chunks: &[RuntimeMatchedChunk]) -> Option<String> {
    if !question_mentions_port(question)
        || question_mentions_protocol(question)
        || technical_literal_focus_keyword_segments(question, None).len() > 1
        || chunks.is_empty()
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, None);
    let focus_segments = technical_literal_focus_keyword_segments(question, None);
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
            document_local_focus_keywords(question, None, document_chunks, &question_keywords);
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
