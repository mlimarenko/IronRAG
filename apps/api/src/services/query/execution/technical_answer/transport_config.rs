use super::{
    AuthoritativeAnswerEvidence, BTreeSet, LazyLock, QueryAct, QueryIR, QueryTargetKind,
    QuestionIntent, RuntimeMatchedChunk, TechnicalFactKind, Uuid, classify_question_or_ir_intents,
    has_question_intent, transport_assignment_source_addresses_question_focus,
};

pub(super) static CONFIG_ASSIGNMENT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(
        r"(?:^|[;\r\n])\s*[#;]?\s*([A-Za-z][A-Za-z0-9_.-]{2,160})\s*=\s*([^;\r\n]{1,220})",
    )
    .build()
    .expect("config assignment regex must compile")
});

#[derive(Debug)]
struct ConfigAssignmentCandidate {
    document_id: Uuid,
    document_label: String,
    entries: Vec<(String, String)>,
    source_excerpt: String,
    score: usize,
}

pub(super) fn build_transport_config_assignment_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if query_ir_requests_port_inventory_without_connection(query_ir, &intents) {
        return None;
    }
    if !query_ir_requests_transport_config_assignment(query_ir, &intents) {
        return None;
    }

    let mut candidates = chunks
        .iter()
        .enumerate()
        .filter_map(|(rank, chunk)| {
            config_assignment_candidate_from_chunk(rank, query_ir, evidence, chunk)
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| left.document_label.cmp(&right.document_label))
    });
    let candidate = candidates.into_iter().next()?;
    if !transport_assignment_source_addresses_question_focus(
        query_ir,
        candidate.document_id,
        chunks,
    ) {
        tracing::info!(
            stage = "answer.transport_assignment_question_fit",
            "transport assignment rejected: selected source does not address the typed question focus"
        );
        return None;
    }
    let mut answer = format!("`{}`", candidate.document_label);
    for (name, value) in candidate.entries.into_iter().take(8) {
        answer.push_str(&format!("\n- `{name}` = `{value}`"));
    }
    if !candidate.source_excerpt.is_empty() {
        answer.push_str("\n```text\n");
        answer.push_str(&candidate.source_excerpt);
        answer.push_str("\n```");
    }
    Some(answer)
}

fn query_ir_requests_transport_config_assignment(
    query_ir: &QueryIR,
    intents: &[QuestionIntent],
) -> bool {
    if !matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::ConfigureHow) {
        return false;
    }

    let has_transport_intent = has_question_intent(intents, QuestionIntent::Port)
        || has_question_intent(intents, QuestionIntent::Protocol);
    let mut has_connection_target = false;
    let mut has_configuration_target = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Connection
            | QueryTargetKind::Endpoint
            | QueryTargetKind::Url
            | QueryTargetKind::BaseUrl
            | QueryTargetKind::Wsdl => {
                has_connection_target = true;
            }
            QueryTargetKind::ConfigurationFile
            | QueryTargetKind::ConfigKey
            | QueryTargetKind::EnvVar
            | QueryTargetKind::Parameter => {
                has_configuration_target = true;
            }
            _ => {}
        }
    }

    has_connection_target || has_transport_intent && has_configuration_target
}

fn query_ir_requests_port_inventory_without_connection(
    query_ir: &QueryIR,
    intents: &[QuestionIntent],
) -> bool {
    let has_port = has_question_intent(intents, QuestionIntent::Port);
    let has_non_port_target =
        query_ir.target_types.iter().any(|target| !matches!(target, QueryTargetKind::Port));
    let has_connection = has_question_intent(intents, QuestionIntent::Protocol)
        || query_ir.targets_any(&[
            QueryTargetKind::Connection,
            QueryTargetKind::Endpoint,
            QueryTargetKind::Url,
            QueryTargetKind::BaseUrl,
            QueryTargetKind::Wsdl,
            QueryTargetKind::Protocol,
        ]);
    has_port && has_non_port_target && !has_connection
}

fn config_assignment_candidate_from_chunk(
    rank: usize,
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunk: &RuntimeMatchedChunk,
) -> Option<ConfigAssignmentCandidate> {
    let text = if chunk.source_text.trim().is_empty() {
        chunk.excerpt.as_str()
    } else {
        chunk.source_text.as_str()
    };
    let mut seen = std::collections::HashSet::new();
    let entries = CONFIG_ASSIGNMENT_REGEX
        .captures_iter(text)
        .filter_map(|capture| {
            let name = capture.get(1)?.as_str().trim().trim_matches('`');
            let value = clean_config_assignment_value(capture.get(2)?.as_str());
            if name.is_empty()
                || value.is_empty()
                || value.len() > 160
                || !seen.insert(name.to_ascii_lowercase())
            {
                return None;
            }
            Some((name.to_string(), value))
        })
        .collect::<Vec<_>>();
    if entries.len() < 2 {
        return None;
    }
    let score = (64usize.saturating_sub(rank.min(64)) * 100)
        + typed_config_assignment_fact_score(query_ir, evidence, chunk, &entries)
        + entries
            .iter()
            .map(|(_, value)| formal_config_assignment_value_score(value))
            .sum::<usize>()
            .saturating_add(entries.len() * 12);
    (score > 0).then_some(ConfigAssignmentCandidate {
        document_id: chunk.document_id,
        document_label: chunk.document_label.clone(),
        entries,
        source_excerpt: config_assignment_source_excerpt(text),
        score,
    })
}

pub(super) fn clean_config_assignment_value(raw: &str) -> String {
    raw.trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_end_matches(['.', ',', ':'])
        .trim()
        .to_string()
}

fn formal_config_assignment_value_score(value: &str) -> usize {
    let trimmed = value.trim();
    usize::from(trimmed.contains("://")) * 120
        + usize::from(url_value_contains_port(trimmed)) * 240
        + usize::from(trimmed.parse::<bool>().is_ok()) * 20
        + usize::from(trimmed.parse::<i64>().is_ok()) * 20
}

fn typed_config_assignment_fact_score(
    query_ir: &QueryIR,
    evidence: &AuthoritativeAnswerEvidence,
    chunk: &RuntimeMatchedChunk,
    entries: &[(String, String)],
) -> usize {
    let mut seen = BTreeSet::<(TechnicalFactKind, String)>::new();
    evidence
        .technical_facts
        .iter()
        .filter(|fact| {
            fact.document_id == chunk.document_id && fact.revision_id == chunk.revision_id
        })
        .filter_map(|fact| {
            let kind = fact.fact_kind.parse::<TechnicalFactKind>().ok()?;
            if !query_target_supports_technical_fact(query_ir, kind) {
                return None;
            }
            let matches_entry = entries
                .iter()
                .any(|(name, value)| technical_fact_matches_config_entry(kind, fact, name, value));
            if !matches_entry {
                return None;
            }
            let normalized_value = if fact.normalized_value_exact.trim().is_empty() {
                fact.display_value.trim()
            } else {
                fact.normalized_value_exact.trim()
            };
            if !seen.insert((kind, normalized_value.to_string())) {
                return None;
            }
            let support_score =
                usize::from(fact.support_chunk_ids.contains(&chunk.chunk_id)) * 1_000;
            Some(20_000usize.saturating_add(support_score))
        })
        .fold(0usize, usize::saturating_add)
}

fn query_target_supports_technical_fact(query_ir: &QueryIR, kind: TechnicalFactKind) -> bool {
    query_ir.target_types.iter().any(|target_type| match kind {
        TechnicalFactKind::Url => matches!(
            target_type,
            QueryTargetKind::Url
                | QueryTargetKind::BaseUrl
                | QueryTargetKind::Wsdl
                | QueryTargetKind::Endpoint
                | QueryTargetKind::Connection
        ),
        TechnicalFactKind::EndpointPath => {
            matches!(
                target_type,
                QueryTargetKind::Endpoint | QueryTargetKind::Path | QueryTargetKind::Connection
            )
        }
        TechnicalFactKind::Port => {
            matches!(
                target_type,
                QueryTargetKind::Port | QueryTargetKind::Endpoint | QueryTargetKind::Connection
            )
        }
        TechnicalFactKind::Protocol => {
            matches!(
                target_type,
                QueryTargetKind::Protocol | QueryTargetKind::Endpoint | QueryTargetKind::Connection
            )
        }
        TechnicalFactKind::ConfigurationKey => {
            matches!(target_type, QueryTargetKind::ConfigKey | QueryTargetKind::Parameter)
        }
        TechnicalFactKind::ParameterName => {
            matches!(target_type, QueryTargetKind::Parameter | QueryTargetKind::ConfigKey)
        }
        TechnicalFactKind::EnvironmentVariable => {
            matches!(target_type, QueryTargetKind::EnvVar)
        }
        _ => false,
    })
}

fn technical_fact_matches_config_entry(
    kind: TechnicalFactKind,
    fact: &crate::infra::knowledge_rows::KnowledgeTechnicalFactRow,
    name: &str,
    value: &str,
) -> bool {
    let fact_values = [
        fact.display_value.as_str(),
        fact.normalized_value_exact.as_str(),
        fact.normalized_value_text.as_str(),
    ];
    match kind {
        TechnicalFactKind::ConfigurationKey
        | TechnicalFactKind::ParameterName
        | TechnicalFactKind::EnvironmentVariable => fact_values
            .iter()
            .any(|candidate| !candidate.is_empty() && name.eq_ignore_ascii_case(candidate)),
        TechnicalFactKind::Url
        | TechnicalFactKind::EndpointPath
        | TechnicalFactKind::Port
        | TechnicalFactKind::Protocol => fact_values
            .iter()
            .any(|candidate| formal_assignment_value_contains_fact(value, candidate)),
        _ => false,
    }
}

fn formal_assignment_value_contains_fact(value: &str, fact_value: &str) -> bool {
    let value = value.trim().trim_matches(['`', '\'', '"']);
    let fact_value = fact_value.trim().trim_matches(['`', '\'', '"']);
    if fact_value.is_empty() {
        return false;
    }
    value.eq_ignore_ascii_case(fact_value)
        || value
            .split(|ch: char| {
                ch.is_whitespace() || matches!(ch, ',' | ';' | '[' | ']' | '(' | ')' | '{' | '}')
            })
            .any(|token| token.eq_ignore_ascii_case(fact_value))
}

fn config_assignment_source_excerpt(text: &str) -> String {
    let cleaned = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut excerpt = cleaned.chars().take(1200).collect::<String>();
    if cleaned.chars().count() > 1200 {
        excerpt.push_str("...");
    }
    excerpt.replace("```", "'''")
}

fn url_value_contains_port(value: &str) -> bool {
    let Some(scheme_index) = value.find("://") else {
        return false;
    };
    let remainder = &value[(scheme_index + 3)..];
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    authority
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()))
}
