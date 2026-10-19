use super::{
    HARD_MAX_QUERY_TEXT_CHARS, HARD_MAX_USER_MESSAGE_BYTES, HashSet, PreparedSemanticRerankRequest,
    ProviderSemanticCandidate, RerankCandidate, SEMANTIC_RERANK_USER_PROMPT_PREFIX,
    SemanticCandidateKind, SemanticCandidateLocator, SemanticRerankPolicy,
};

pub(crate) fn prepare_semantic_rerank_request(
    question: &str,
    entities: &[RerankCandidate],
    relationships: &[RerankCandidate],
    chunks: &[RerankCandidate],
    policy: SemanticRerankPolicy,
) -> Option<PreparedSemanticRerankRequest> {
    if !all_candidate_ids_are_unique(entities, relationships, chunks) {
        return None;
    }
    let question = bounded_semantic_question(question, policy)?;
    let (mut provider_candidates, mut locators) = prepare_provider_candidates(
        entities,
        relationships,
        chunks,
        policy,
        policy.total_text_chars.saturating_sub(question.chars().count()),
    );
    enforce_semantic_message_byte_limit(&question, &mut provider_candidates, &mut locators);
    if provider_candidates.is_empty() {
        return None;
    }

    Some(PreparedSemanticRerankRequest {
        question,
        provider_candidates,
        locators,
        entity_ids: entities.iter().map(|candidate| candidate.id.clone()).collect(),
        relationship_ids: relationships.iter().map(|candidate| candidate.id.clone()).collect(),
        chunk_ids: chunks.iter().map(|candidate| candidate.id.clone()).collect(),
    })
}

fn all_candidate_ids_are_unique(
    entities: &[RerankCandidate],
    relationships: &[RerankCandidate],
    chunks: &[RerankCandidate],
) -> bool {
    candidate_ids_are_unique(entities)
        && candidate_ids_are_unique(relationships)
        && candidate_ids_are_unique(chunks)
}

fn bounded_semantic_question(question: &str, policy: SemanticRerankPolicy) -> Option<String> {
    let question = question.trim();
    if question.is_empty() {
        return None;
    }
    // Reserve at least one character for candidate evidence. An unusually long
    // standalone query must not consume the complete provider payload budget.
    let question_limit = HARD_MAX_QUERY_TEXT_CHARS.min(policy.total_text_chars.saturating_sub(1));
    let question = truncate_chars(question, question_limit);
    (!question.is_empty()).then_some(question)
}

fn prepare_provider_candidates(
    entities: &[RerankCandidate],
    relationships: &[RerankCandidate],
    chunks: &[RerankCandidate],
    policy: SemanticRerankPolicy,
    mut remaining_chars: usize,
) -> (Vec<ProviderSemanticCandidate>, Vec<SemanticCandidateLocator>) {
    let mut provider_candidates = Vec::new();
    let mut locators = Vec::new();
    let mut positions = [0_usize; 3];

    while provider_candidates.len() < policy.candidate_limit && remaining_chars > 0 {
        let added = add_semantic_candidate_round(
            entities,
            relationships,
            chunks,
            policy,
            &mut SemanticCandidateRoundState {
                positions: &mut positions,
                remaining_chars: &mut remaining_chars,
                provider_candidates: &mut provider_candidates,
                locators: &mut locators,
            },
        );
        if !added {
            break;
        }
    }
    (provider_candidates, locators)
}

/// Mutable accumulators threaded through one candidate round.
struct SemanticCandidateRoundState<'a> {
    positions: &'a mut [usize; 3],
    remaining_chars: &'a mut usize,
    provider_candidates: &'a mut Vec<ProviderSemanticCandidate>,
    locators: &'a mut Vec<SemanticCandidateLocator>,
}

fn add_semantic_candidate_round(
    entities: &[RerankCandidate],
    relationships: &[RerankCandidate],
    chunks: &[RerankCandidate],
    policy: SemanticRerankPolicy,
    state: &mut SemanticCandidateRoundState<'_>,
) -> bool {
    let mut added = false;
    for (kind, candidates, position_slot) in [
        (SemanticCandidateKind::Chunk, chunks, 0),
        (SemanticCandidateKind::Entity, entities, 1),
        (SemanticCandidateKind::Relationship, relationships, 2),
    ] {
        if add_next_semantic_candidate(kind, candidates, position_slot, policy, state) {
            added = true;
        }
        if state.provider_candidates.len() == policy.candidate_limit || *state.remaining_chars == 0
        {
            break;
        }
    }
    added
}

fn add_next_semantic_candidate(
    kind: SemanticCandidateKind,
    candidates: &[RerankCandidate],
    position_slot: usize,
    policy: SemanticRerankPolicy,
    state: &mut SemanticCandidateRoundState<'_>,
) -> bool {
    let Some(candidate) = candidates.get(state.positions[position_slot]) else {
        return false;
    };
    state.positions[position_slot] += 1;
    let text_limit = policy.candidate_text_chars.min(*state.remaining_chars);
    let text = truncate_chars(candidate.text.trim(), text_limit);
    if text.is_empty() {
        return false;
    }
    let index = state.provider_candidates.len();
    *state.remaining_chars = state.remaining_chars.saturating_sub(text.chars().count());
    state.provider_candidates.push(ProviderSemanticCandidate { index, text });
    state.locators.push(SemanticCandidateLocator { kind, id: candidate.id.clone() });
    true
}

fn enforce_semantic_message_byte_limit(
    question: &str,
    provider_candidates: &mut Vec<ProviderSemanticCandidate>,
    locators: &mut Vec<SemanticCandidateLocator>,
) {
    // Raw character budgets are operator-facing; this second hard cap covers
    // UTF-8 and JSON escaping so adversarial quotes/control characters cannot
    // expand the actual user message without bound. Dropping only the tail
    // keeps opaque indices contiguous and deterministic.
    while semantic_user_message_bytes(question, provider_candidates) > HARD_MAX_USER_MESSAGE_BYTES {
        provider_candidates.pop();
        locators.pop();
    }
}

fn candidate_ids_are_unique(candidates: &[RerankCandidate]) -> bool {
    let mut ids = HashSet::with_capacity(candidates.len());
    candidates.iter().all(|candidate| ids.insert(candidate.id.as_str()))
}

fn semantic_user_message_bytes(question: &str, candidates: &[ProviderSemanticCandidate]) -> usize {
    let payload = serde_json::json!({
        "query": question,
        "candidates": candidates,
    });
    SEMANTIC_RERANK_USER_PROMPT_PREFIX.len() + payload.to_string().len()
}

fn truncate_chars(value: &str, limit: usize) -> String {
    value.chars().take(limit).collect()
}
