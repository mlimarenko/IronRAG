use super::*;

pub(crate) fn seed_chunk_refs_from_answer_context(
    answer_chunk_references: &[QueryChunkReferenceSnapshot],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut chunk_refs = HashMap::new();
    for reference in answer_chunk_references {
        merge_ranked_reference(
            &mut chunk_refs,
            reference.chunk_id,
            reference.rank,
            reference.score,
            "answer_context",
        );
    }
    chunk_refs
}

pub(crate) fn seed_entity_refs_from_answer_graph(
    answer_entity_references: &[RuntimeMatchedEntity],
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    for (index, reference) in answer_entity_references.iter().enumerate() {
        merge_ranked_reference(
            entity_refs,
            reference.node_id,
            saturating_rank(index),
            reference.score.map_or(0.0, f64::from),
            "answer_graph_context",
        );
    }
}

pub(crate) fn seed_relation_refs_from_answer_graph(
    answer_relation_references: &[RuntimeMatchedRelationship],
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    for (index, reference) in answer_relation_references.iter().enumerate() {
        merge_ranked_reference(
            relation_refs,
            reference.edge_id,
            saturating_rank(index),
            reference.score.map_or(0.0, f64::from),
            "answer_graph_context",
        );
    }
}

pub(crate) fn seed_relation_endpoint_refs_from_answer_graph(
    answer_relation_references: &[RuntimeMatchedRelationship],
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    for (index, reference) in answer_relation_references.iter().enumerate() {
        let rank = saturating_rank(index);
        let score = reference.score.map_or(0.0, f64::from);
        merge_ranked_reference(
            entity_refs,
            reference.from_node_id,
            rank,
            score,
            "answer_relation_endpoint",
        );
        merge_ranked_reference(
            entity_refs,
            reference.to_node_id,
            rank,
            score,
            "answer_relation_endpoint",
        );
    }
}

pub(super) fn absorb_traversal_row(
    row: &KnowledgeGraphTraversalRow,
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
    evidence_refs: &mut HashMap<Uuid, RankedBundleReference>,
    reason: &str,
) {
    let rank = traversal_rank(row.path_length);
    let score = row.edge_score.unwrap_or_else(|| traversal_score(row.path_length));
    match row.vertex_kind.as_str() {
        "knowledge_chunk" => {}
        "knowledge_entity" => {
            merge_ranked_reference(entity_refs, row.vertex_id, rank, score, reason);
        }
        "knowledge_relation" => {
            merge_ranked_reference(relation_refs, row.vertex_id, rank, score, reason);
        }
        "knowledge_evidence" => {
            merge_ranked_reference(evidence_refs, row.vertex_id, rank, score, reason);
        }
        _ => {}
    }
}

fn traversal_rank(path_length: i64) -> i32 {
    i32::try_from(path_length.saturating_add(1)).unwrap_or(i32::MAX)
}

fn traversal_score(path_length: i64) -> f64 {
    match path_length {
        0 => 1.0,
        1 => 0.8,
        2 => 0.6,
        3 => 0.4,
        _ => 0.2,
    }
}

pub(super) fn build_chunk_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleChunkEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(chunk_id, reference)| KnowledgeBundleChunkEdgeRow {
            bundle_id,
            chunk_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

pub(super) fn build_entity_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleEntityEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(entity_id, reference)| KnowledgeBundleEntityEdgeRow {
            bundle_id,
            entity_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

pub(super) fn build_relation_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleRelationEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(relation_id, reference)| KnowledgeBundleRelationEdgeRow {
            bundle_id,
            relation_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

pub(super) fn build_evidence_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleEvidenceEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(evidence_id, reference)| KnowledgeBundleEvidenceEdgeRow {
            bundle_id,
            evidence_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

pub(crate) fn evidence_rank_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    evidence_id: Uuid,
) -> i32 {
    refs.get(&evidence_id).map_or(i32::MAX, |reference| reference.rank)
}

pub(crate) fn evidence_score_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    evidence_id: Uuid,
) -> f64 {
    refs.get(&evidence_id).map_or(0.0, |reference| reference.score)
}

pub(crate) fn fact_rank_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    fact_id: Uuid,
) -> i32 {
    refs.get(&fact_id).map_or(i32::MAX, |reference| reference.rank)
}

pub(crate) fn fact_score_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    fact_id: Uuid,
) -> f64 {
    refs.get(&fact_id).map_or(0.0, |reference| reference.score)
}

pub(crate) fn derive_chunk_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> HashMap<Uuid, RankedBundleReference> {
    let mut chunk_refs = HashMap::<Uuid, RankedBundleReference>::new();
    for reference in &bundle.chunk_references {
        merge_ranked_reference(
            &mut chunk_refs,
            reference.chunk_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_chunk"),
        );
    }
    chunk_refs
}

pub(crate) fn augment_fact_rank_refs_from_chunk_support(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    fact_rank_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    let Some(bundle) = bundle else {
        return;
    };
    let chunk_rank_refs = derive_chunk_rank_refs(bundle);
    if chunk_rank_refs.is_empty() {
        return;
    }
    for fact in technical_fact_rows {
        let mut best_rank = None::<i32>;
        let mut best_score = 0.0_f64;
        for chunk_id in &fact.support_chunk_ids {
            let Some(reference) = chunk_rank_refs.get(chunk_id) else {
                continue;
            };
            best_rank = Some(best_rank.map_or(reference.rank, |rank| rank.min(reference.rank)));
            if reference.score > best_score {
                best_score = reference.score;
            }
        }
        let Some(rank) = best_rank else {
            continue;
        };
        merge_ranked_reference(
            fact_rank_refs,
            fact.fact_id,
            rank,
            best_score.max(1.0),
            "selected_chunk_support",
        );
    }
}

pub(crate) fn merge_technical_fact_rows(
    target: &mut Vec<KnowledgeTechnicalFactRow>,
    additional: &[KnowledgeTechnicalFactRow],
) {
    let mut seen = target.iter().map(|row| row.fact_id).collect::<std::collections::BTreeSet<_>>();
    for row in additional {
        if seen.insert(row.fact_id) {
            target.push(row.clone());
        }
    }
    target.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
}

#[derive(Debug, PartialEq, Eq, Hash)]
struct TechnicalFactDiversityKey {
    document_id: Uuid,
    revision_id: Uuid,
    fact_kind: TechnicalFactKind,
    normalized_value: String,
    qualifiers: Vec<(String, String)>,
}

#[derive(Debug, PartialEq, Eq, Hash)]
enum TechnicalFactDiversityIdentity {
    Known(TechnicalFactDiversityKey),
    Opaque(Uuid),
}

pub(crate) fn select_diversified_fact_ids(
    fact_rank_refs: &HashMap<Uuid, RankedBundleReference>,
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 || fact_rank_refs.is_empty() {
        return Vec::new();
    }

    let facts_by_id =
        technical_fact_rows.iter().map(|fact| (fact.fact_id, fact)).collect::<HashMap<_, _>>();
    let mut seen = HashSet::<TechnicalFactDiversityIdentity>::new();
    let mut selected = Vec::<Uuid>::with_capacity(limit.min(fact_rank_refs.len()));

    for fact_id in top_ranked_ids(fact_rank_refs, fact_rank_refs.len()) {
        let identity =
            facts_by_id.get(&fact_id).and_then(|fact| technical_fact_diversity_key(fact)).map_or(
                TechnicalFactDiversityIdentity::Opaque(fact_id),
                TechnicalFactDiversityIdentity::Known,
            );
        if seen.insert(identity) {
            selected.push(fact_id);
            if selected.len() >= limit {
                break;
            }
        }
    }

    selected
}

fn technical_fact_diversity_key(
    fact: &KnowledgeTechnicalFactRow,
) -> Option<TechnicalFactDiversityKey> {
    let fact_kind = fact.fact_kind.parse::<TechnicalFactKind>().ok()?;
    let normalized_value = fact.normalized_value_exact.trim();
    if normalized_value.is_empty() {
        return None;
    }
    let mut qualifiers =
        serde_json::from_value::<Vec<TechnicalFactQualifier>>(fact.qualifiers_json.clone())
            .ok()?
            .into_iter()
            .map(|qualifier| {
                (qualifier.key.trim().to_ascii_lowercase(), qualifier.value.trim().to_string())
            })
            .collect::<Vec<_>>();
    qualifiers.sort();
    qualifiers.dedup();

    Some(TechnicalFactDiversityKey {
        document_id: fact.document_id,
        revision_id: fact.revision_id,
        fact_kind,
        normalized_value: normalized_value.to_string(),
        qualifiers,
    })
}

pub(crate) fn derive_fact_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    evidence_rows: &[KnowledgeEvidenceRow],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut fact_refs = HashMap::<Uuid, RankedBundleReference>::new();
    let evidence_by_id = evidence_rows
        .iter()
        .map(|evidence| (evidence.evidence_id, evidence))
        .collect::<HashMap<_, _>>();
    for reference in &bundle.evidence_references {
        let Some(evidence) = evidence_by_id.get(&reference.evidence_id) else {
            continue;
        };
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        merge_ranked_reference(
            &mut fact_refs,
            fact_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_evidence"),
        );
    }
    for (index, fact_id) in bundle.bundle.selected_fact_ids.iter().copied().enumerate() {
        let score = fact_refs.get(&fact_id).map_or(1.0, |reference| reference.score.max(1.0));
        merge_ranked_reference(
            &mut fact_refs,
            fact_id,
            saturating_rank(index),
            score,
            "bundle_selected_fact",
        );
    }
    fact_refs
}

pub(crate) fn derive_block_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    evidence_rows: &[KnowledgeEvidenceRow],
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    chunk_rows: &[crate::infra::knowledge_rows::KnowledgeChunkRow],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut block_refs = HashMap::<Uuid, RankedBundleReference>::new();
    let evidence_by_id = evidence_rows
        .iter()
        .map(|evidence| (evidence.evidence_id, evidence))
        .collect::<HashMap<_, _>>();
    for reference in &bundle.evidence_references {
        let Some(evidence) = evidence_by_id.get(&reference.evidence_id) else {
            continue;
        };
        let Some(block_id) = evidence.block_id else {
            continue;
        };
        merge_ranked_reference(
            &mut block_refs,
            block_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_evidence"),
        );
    }
    let fact_rank_refs = derive_fact_rank_refs(bundle, evidence_rows);
    for fact in technical_fact_rows {
        let rank = fact_rank_for_bundle(&fact_rank_refs, fact.fact_id);
        let score = fact_score_for_bundle(&fact_rank_refs, fact.fact_id).max(1.0);
        for block_id in &fact.support_block_ids {
            merge_ranked_reference(
                &mut block_refs,
                *block_id,
                rank,
                score,
                "technical_fact_support",
            );
        }
    }
    let chunk_rank_refs = derive_chunk_rank_refs(bundle);
    for chunk in chunk_rows {
        let Some(reference) = chunk_rank_refs.get(&chunk.chunk_id) else {
            continue;
        };
        for block_id in &chunk.support_block_ids {
            merge_ranked_reference(
                &mut block_refs,
                *block_id,
                reference.rank,
                reference.score.max(1.0),
                "selected_chunk_support",
            );
        }
    }
    block_refs
}

pub(crate) fn selected_fact_ids_for_detail(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    fact_rank_refs: &HashMap<Uuid, RankedBundleReference>,
) -> Vec<Uuid> {
    let mut fact_ids = bundle.bundle.selected_fact_ids.clone();
    for fact_id in top_ranked_ids(fact_rank_refs, super::MAX_DETAIL_TECHNICAL_FACT_REFERENCES) {
        if fact_ids.len() >= super::MAX_DETAIL_TECHNICAL_FACT_REFERENCES {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    fact_ids.truncate(super::MAX_DETAIL_TECHNICAL_FACT_REFERENCES);
    fact_ids
}

pub(super) fn freshness_snapshot_json(row: &KnowledgeLibraryGenerationRow) -> serde_json::Value {
    json!({
        "generationId": row.generation_id,
        "activeTextGeneration": row.active_text_generation,
        "activeVectorGeneration": row.active_vector_generation,
        "activeGraphGeneration": row.active_graph_generation,
        "degradedState": row.degraded_state,
        "updatedAt": row.updated_at,
    })
}
