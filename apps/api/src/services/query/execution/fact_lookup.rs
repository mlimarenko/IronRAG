use std::collections::HashMap;

use uuid::Uuid;

use crate::{
    infra::arangodb::document_store::KnowledgeTechnicalFactRow,
    shared::extraction::technical_facts::TechnicalFactKind,
};

use super::{CanonicalAnswerEvidence, RuntimeMatchedChunk};

pub(super) struct TechnicalFactMatch<'a> {
    pub(super) fact: &'a KnowledgeTechnicalFactRow,
    pub(super) document_label: Option<&'a str>,
}

pub(super) fn build_document_labels(chunks: &[RuntimeMatchedChunk]) -> HashMap<Uuid, String> {
    let mut document_labels = HashMap::<Uuid, String>::new();
    for chunk in chunks {
        document_labels.entry(chunk.document_id).or_insert_with(|| chunk.document_label.clone());
    }
    document_labels
}

pub(super) fn best_matching_fact<'a, Predicate, Score>(
    evidence: &'a CanonicalAnswerEvidence,
    document_labels: &'a HashMap<Uuid, String>,
    kind: TechnicalFactKind,
    predicate: Predicate,
    score: Score,
) -> Option<TechnicalFactMatch<'a>>
where
    Predicate: Fn(&KnowledgeTechnicalFactRow) -> bool,
    Score: Fn(&KnowledgeTechnicalFactRow, Option<&str>) -> usize,
{
    evidence
        .technical_facts
        .iter()
        .filter(|fact| fact_kind_matches(fact, kind))
        .filter(|fact| predicate(fact))
        .max_by_key(|fact| score(fact, document_labels.get(&fact.document_id).map(String::as_str)))
        .map(|fact| TechnicalFactMatch {
            fact,
            document_label: document_labels.get(&fact.document_id).map(String::as_str),
        })
}

fn fact_kind_matches(fact: &KnowledgeTechnicalFactRow, kind: TechnicalFactKind) -> bool {
    fact.fact_kind.parse::<TechnicalFactKind>().ok() == Some(kind)
}
