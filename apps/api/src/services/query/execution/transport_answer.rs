use std::collections::HashMap;

use uuid::Uuid;

use super::concise_document_subject_label;
use super::question_intent::{question_asks_transport_comparison, question_mentions_graphql};
use super::question_prefers_russian;
use super::technical_literals::technical_literal_focus_keywords;
use super::types::RuntimeMatchedChunk;

pub(super) fn build_graphql_absence_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_mentions_graphql(question) {
        return None;
    }
    let has_absence_evidence =
        chunks.iter().any(|chunk| graphql_absence_supported(&chunk.source_text));
    if !has_absence_evidence {
        return None;
    }
    Some(if question_prefers_russian(question) {
        "Нет, в этой библиотеке GraphQL API не публикуется.".to_string()
    } else {
        "No, this library does not publish a GraphQL API.".to_string()
    })
}

fn graphql_absence_supported(text: &str) -> bool {
    let lowered = text.to_lowercase();
    if !lowered.contains("graphql") {
        return false;
    }
    [
        "does not publish",
        "does not expose",
        "no /graphql",
        "no graphql schema",
        "no graphql introspection",
        "not graphql",
        "не публикует",
        "не поддерживает",
        "нет graphql",
        "без graphql",
        "не graphql",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

pub(super) fn build_transport_contract_comparison_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_asks_transport_comparison(question) {
        return None;
    }
    let question_keywords = technical_literal_focus_keywords(question);

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    #[derive(Debug, Clone)]
    struct TransportDocumentSummary {
        subject: String,
        supports_rest_json_http: bool,
        supports_soap_http: bool,
        supports_wsdl: bool,
        subject_match_score: usize,
        rank: usize,
    }

    let summarize_document = |document_chunks: &[&RuntimeMatchedChunk],
                              rank: usize|
     -> Option<TransportDocumentSummary> {
        let subject = concise_document_subject_label(&document_chunks[0].document_label);
        let corpus = document_chunks
            .iter()
            .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        let subject_lower = subject.to_lowercase();
        let has_rest = corpus.contains("rest");
        let has_soap = corpus.contains("soap");
        let has_wsdl = corpus.contains("wsdl");
        let has_json = corpus.contains("json");
        let has_http = corpus.contains("http");
        if !(has_rest || has_soap || has_wsdl) {
            return None;
        }
        let subject_match_score = question_keywords
            .iter()
            .map(|keyword| {
                usize::from(subject_lower.contains(keyword)) * 100
                    + usize::from(corpus.contains(keyword)) * 10
            })
            .sum::<usize>();
        Some(TransportDocumentSummary {
            subject,
            supports_rest_json_http: has_rest && has_http && has_json,
            supports_soap_http: has_soap && has_http,
            supports_wsdl: has_wsdl,
            subject_match_score,
            rank,
        })
    };

    let mut transport_documents = Vec::<TransportDocumentSummary>::new();
    for (rank, document_id) in ordered_document_ids.into_iter().enumerate() {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let Some(summary) = summarize_document(document_chunks, rank) else {
            continue;
        };
        transport_documents.push(summary);
    }
    let rest_document = transport_documents
        .iter()
        .filter(|summary| summary.supports_rest_json_http)
        .max_by(|left, right| {
            left.subject_match_score
                .cmp(&right.subject_match_score)
                .then_with(|| right.rank.cmp(&left.rank))
        })?;
    let soap_document = transport_documents
        .iter()
        .filter(|summary| {
            summary.supports_soap_http && summary.supports_wsdl && !summary.supports_rest_json_http
        })
        .max_by(|left, right| {
            left.subject_match_score
                .cmp(&right.subject_match_score)
                .then_with(|| right.rank.cmp(&left.rank))
        })
        .or_else(|| {
            transport_documents
                .iter()
                .filter(|summary| summary.supports_soap_http && summary.supports_wsdl)
                .max_by(|left, right| {
                    left.subject_match_score
                        .cmp(&right.subject_match_score)
                        .then_with(|| right.rank.cmp(&left.rank))
                })
        })?;

    Some(if question_prefers_russian(question) {
        format!(
            "{} использует REST поверх HTTP с JSON, а {} использует SOAP поверх HTTP и описан через WSDL.",
            rest_document.subject, soap_document.subject
        )
    } else {
        format!(
            "{} uses REST over HTTP with JSON, while {} uses SOAP over HTTP and is described by WSDL.",
            rest_document.subject, soap_document.subject
        )
    })
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        build_graphql_absence_answer, build_transport_contract_comparison_answer,
        graphql_absence_supported,
    };
    use crate::services::query::execution::types::RuntimeMatchedChunk;

    fn sample_chunk(document_label: &str, excerpt: &str, source_text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            document_label: document_label.to_string(),
            excerpt: excerpt.to_string(),
            score: Some(1.0),
            source_text: source_text.to_string(),
        }
    }

    #[test]
    fn graphql_absence_support_detects_negative_graphql_evidence() {
        assert!(graphql_absence_supported(
            "The checkout server does not publish a GraphQL API. No /graphql endpoint is exposed."
        ));
        assert!(!graphql_absence_supported(
            "This document explains a GraphQL schema and GraphQL endpoint."
        ));
    }

    #[test]
    fn build_graphql_absence_answer_returns_explicit_negative_answer() {
        let chunks = vec![sample_chunk(
            "checkout_runtime_contract.md",
            "No /graphql endpoint is exposed.",
            "The checkout server does not publish a GraphQL API. No /graphql endpoint is exposed.",
        )];
        let answer =
            build_graphql_absence_answer("Есть ли в этой библиотеке GraphQL API?", &chunks)
                .expect("graphql absence answer");
        assert!(answer.starts_with("Нет"));
        assert!(answer.contains("GraphQL API"));
    }

    #[test]
    fn build_transport_contract_comparison_answer_avoids_graphql_noise() {
        let rewards = sample_chunk(
            "rewards_accounts_api_contract.md",
            "REST JSON over HTTP",
            "The rewards accounts surface is a REST API that returns JSON over HTTP.",
        );
        let inventory = sample_chunk(
            "inventory_soap_api_contract.md",
            "SOAP WSDL over HTTP",
            "The inventory integration surface is SOAP over HTTP and described by WSDL.",
        );
        let answer = build_transport_contract_comparison_answer(
            "Чем REST API rewards accounts отличается от inventory wsdl в транспортном контракте?",
            &[rewards, inventory],
        )
        .expect("transport comparison answer");
        assert!(answer.contains("REST"));
        assert!(answer.contains("SOAP"));
        assert!(answer.contains("WSDL"));
        assert!(!answer.contains("GraphQL"));
    }

    #[test]
    fn build_transport_contract_comparison_answer_prefers_named_subjects_over_unrelated_rest_doc() {
        let checkout = sample_chunk(
            "checkout_runtime_contract.md",
            "REST JSON over HTTP",
            "The checkout runtime contract is a REST API over HTTP with JSON.",
        );
        let rewards = sample_chunk(
            "rewards_accounts_api_contract.md",
            "REST JSON over HTTP",
            "The rewards accounts surface is a REST API that returns JSON over HTTP.",
        );
        let inventory = sample_chunk(
            "inventory_soap_api_contract.md",
            "SOAP WSDL over HTTP",
            "The inventory integration surface is SOAP over HTTP and described by WSDL.",
        );
        let answer = build_transport_contract_comparison_answer(
            "Чем REST API rewards accounts отличается от inventory wsdl в транспортном контракте?",
            &[checkout, rewards, inventory],
        )
        .expect("transport comparison answer");
        let lowered = answer.to_lowercase();
        assert!(lowered.contains("rewards accounts"));
        assert!(lowered.contains("inventory"));
        assert!(!lowered.contains("checkout"));
    }

    #[test]
    fn build_transport_contract_comparison_answer_handles_english_differ_from_questions() {
        let rewards = sample_chunk(
            "rewards_accounts_api_contract.md",
            "REST JSON over HTTP",
            "The rewards accounts surface is a REST API that returns JSON over HTTP.",
        );
        let inventory = sample_chunk(
            "inventory_soap_api_contract.md",
            "SOAP WSDL over HTTP",
            "The inventory integration surface is SOAP over HTTP and described by WSDL.",
        );
        let answer = build_transport_contract_comparison_answer(
            "How does the REST API for rewards accounts differ from the inventory WSDL transport contract?",
            &[rewards, inventory],
        )
        .expect("transport comparison answer");
        let lowered = answer.to_lowercase();
        assert!(lowered.contains("rewards accounts"));
        assert!(lowered.contains("inventory"));
        assert!(lowered.contains("rest"));
        assert!(lowered.contains("wsdl"));
        assert!(!lowered.contains("graphql"));
    }
}
