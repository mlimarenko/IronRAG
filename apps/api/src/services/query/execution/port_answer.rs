use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::domains::query_ir::QueryIR;
use crate::shared::extraction::technical_facts::TechnicalFactKind;

use super::concise_document_subject_label;
use super::fact_lookup::build_document_labels;
use super::question_intent::{
    QuestionIntent, classify_question_or_ir_intents, has_question_intent,
};
use super::technical_literals::{
    technical_chunk_selection_score, technical_literal_focus_keyword_segments,
    technical_literal_focus_keywords,
};
use super::types::RuntimeMatchedChunk;
use super::{CanonicalAnswerEvidence, technical_answer::document_focus_preference};

fn fact_kind_matches(
    fact: &crate::infra::knowledge_rows::KnowledgeTechnicalFactRow,
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

fn unique_document_protocol(
    evidence: &CanonicalAnswerEvidence,
    document_id: Uuid,
) -> Option<String> {
    let protocols =
        collect_document_fact_values(evidence, document_id, TechnicalFactKind::Protocol);
    // Multiple protocols can describe different layers of the same interface
    // (for example, an application protocol over a transport protocol).  Their
    // names do not encode a domain-neutral precedence rule.  Yield to grounded
    // synthesis instead of maintaining a handwritten protocol ranking.
    (protocols.len() == 1).then(|| protocols[0].to_ascii_uppercase())
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
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if !has_question_intent(&intents, QuestionIntent::Port)
        || !has_question_intent(&intents, QuestionIntent::Protocol)
        || chunks.is_empty()
    {
        return None;
    }

    let focus_segments = technical_literal_focus_keyword_segments(question, Some(query_ir))
        .into_iter()
        .filter(|keywords| !keywords.is_empty())
        .collect::<Vec<_>>();
    if focus_segments.len() < 2 {
        return None;
    }

    let (ordered_document_ids, per_document_chunks) = chunks_by_document(chunks);
    let document_labels = build_document_labels(chunks);
    let mut port_line = None;
    let mut port_document_id = None;
    let mut protocol_line = None;
    let mut fallback_protocol_line = None;

    for segment_keywords in focus_segments {
        let Some(document_id) =
            select_segment_document(&ordered_document_ids, &per_document_chunks, &segment_keywords)
        else {
            continue;
        };
        let document_label =
            document_labels.get(&document_id).map(String::as_str).unwrap_or_default();
        let subject = concise_document_subject_label(document_label);

        if port_line.is_none()
            && let Some(port) =
                collect_document_fact_values(evidence, document_id, TechnicalFactKind::Port)
                    .into_iter()
                    .next()
        {
            port_line = Some(format!("{subject}: port `{port}`"));
            port_document_id = Some(document_id);
        }

        if (protocol_line.is_none() || Some(document_id) == port_document_id)
            && let Some(protocol) = unique_document_protocol(evidence, document_id)
        {
            let line = format!("{subject}: protocol `{protocol}`");
            if Some(document_id) == port_document_id {
                fallback_protocol_line.get_or_insert(line);
            } else {
                protocol_line = Some(line);
            }
        }
    }

    if protocol_line.is_none() {
        protocol_line = fallback_protocol_line;
    }

    match (port_line, protocol_line) {
        (Some(port), Some(protocol)) => Some(format!("{port}. {protocol}.")),
        _ => None,
    }
}

pub(super) fn build_port_answer_from_facts(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if !has_question_intent(&intents, QuestionIntent::Port)
        || has_question_intent(&intents, QuestionIntent::Protocol)
        || technical_literal_focus_keyword_segments(question, Some(query_ir)).len() > 1
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let focus_segments = technical_literal_focus_keyword_segments(question, Some(query_ir));
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
            continue;
        }
        if ports.len() == 1 {
            return Some(format!("{subject}: port `{}`.", ports[0]));
        }

        let rendered_ports =
            ports.iter().map(|port| format!("`{port}`")).collect::<Vec<_>>().join(", ");
        return Some(format!("{subject}: ports {rendered_ports}."));
    }

    None
}
