use super::source_coverage::broad_unfocused_procedure_source_coverage;
use super::{
    AppState, AuthoritativeAnswerEvidence, BTreeSet, Context, HashMap, HashSet, KnowledgeChunkRow,
    MAX_AUTHORITATIVE_ANSWER_TECHNICAL_FACTS, QueryAct, QueryIR, QueryScope, QueryTargetKind,
    RuntimeMatchedChunk, Uuid, explicit_target_document_ids_from_values,
    extract_explicit_path_literals, extract_parameter_literals, near_token_match,
    normalized_alnum_tokens, question_asks_table_aggregation,
    render_authoritative_technical_fact_section, render_prepared_segment_section,
    render_primary_chunk_section, render_table_summary_chunk_section, score_value,
    structured_current_question_segment,
};

pub(crate) async fn load_authoritative_answer_evidence(
    state: &AppState,
    execution_id: Uuid,
) -> anyhow::Result<AuthoritativeAnswerEvidence> {
    let Some(bundle_refs) = state
        .context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for answer evidence {execution_id}")
        })?
    else {
        return Ok(AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        });
    };

    let chunk_ids =
        bundle_refs.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    let evidence_rows = state
        .graph_store
        .list_evidence_by_ids(
            &bundle_refs
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await
        .context("failed to load evidence rows for authoritative answer context")?;
    let chunk_rows = state
        .document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load chunks for authoritative answer context")?;
    let readable_scopes = load_current_readable_prepared_scopes(
        state,
        evidence_rows
            .iter()
            .map(|evidence| evidence.revision_id)
            .chain(chunk_rows.iter().map(|chunk| chunk.revision_id)),
    )
    .await?;
    let evidence_rows = evidence_rows
        .into_iter()
        .filter(|evidence| {
            readable_scopes.contains(&(
                evidence.revision_id,
                evidence.document_id,
                evidence.workspace_id,
                evidence.library_id,
            ))
        })
        .collect::<Vec<_>>();
    let chunk_rows = chunk_rows
        .into_iter()
        .filter(|chunk| {
            readable_scopes.contains(&(
                chunk.revision_id,
                chunk.document_id,
                chunk.workspace_id,
                chunk.library_id,
            ))
        })
        .collect::<Vec<_>>();
    let chunk_ids = chunk_rows.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
    let chunk_supported_facts =
        state.document_store.list_technical_facts_by_chunk_ids(&chunk_ids).await.context(
            "failed to load chunk-supported technical facts for authoritative answer context",
        )?;
    let fact_ids = selected_fact_ids_for_authoritative_evidence(
        &bundle_refs.bundle.selected_fact_ids,
        &evidence_rows,
        &chunk_supported_facts,
    );
    let technical_facts =
        load_authoritative_technical_facts(state, &fact_ids, chunk_supported_facts).await?;
    let block_ids = answer_source_block_ids(&evidence_rows, &chunk_rows, &technical_facts);
    let structured_blocks = state
        .document_store
        .list_structured_blocks_by_ids(&block_ids)
        .await
        .context("failed to load structured blocks for authoritative answer context")?;
    let final_readable_scopes = load_current_readable_prepared_scopes(
        state,
        evidence_rows
            .iter()
            .map(|evidence| evidence.revision_id)
            .chain(chunk_rows.iter().map(|chunk| chunk.revision_id))
            .chain(technical_facts.iter().map(|fact| fact.revision_id))
            .chain(structured_blocks.iter().map(|block| block.revision_id)),
    )
    .await?;
    let chunk_rows = chunk_rows
        .into_iter()
        .filter(|chunk| {
            final_readable_scopes.contains(&(
                chunk.revision_id,
                chunk.document_id,
                chunk.workspace_id,
                chunk.library_id,
            ))
        })
        .collect::<Vec<_>>();
    let technical_facts = technical_facts
        .into_iter()
        .filter(|fact| {
            final_readable_scopes.contains(&(
                fact.revision_id,
                fact.document_id,
                fact.workspace_id,
                fact.library_id,
            ))
        })
        .collect::<Vec<_>>();
    let structured_blocks = structured_blocks
        .into_iter()
        .filter(|block| {
            final_readable_scopes.contains(&(
                block.revision_id,
                block.document_id,
                block.workspace_id,
                block.library_id,
            ))
        })
        .collect::<Vec<_>>();
    Ok(AuthoritativeAnswerEvidence {
        bundle: Some(bundle_refs.bundle),
        chunk_rows,
        structured_blocks,
        technical_facts,
    })
}

type ReadablePreparedScope = (Uuid, Uuid, Uuid, Uuid);

pub(super) const CURRENT_READABLE_PREPARED_SCOPES_SQL: &str =
    "select sr.revision_id, sr.document_id, sr.workspace_id, sr.library_id
     from knowledge_structured_revision sr
     join knowledge_document d
       on d.document_id = sr.document_id
      and d.workspace_id = sr.workspace_id
      and d.library_id = sr.library_id
      and d.readable_revision_id = sr.revision_id
      and d.document_state = 'active'
      and d.deleted_at is null
     where sr.revision_id = any($1::uuid[])
       and sr.preparation_state = 'prepared'";

async fn load_current_readable_prepared_scopes(
    state: &AppState,
    revision_ids: impl IntoIterator<Item = Uuid>,
) -> anyhow::Result<HashSet<ReadablePreparedScope>> {
    let revision_ids =
        revision_ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
    if revision_ids.is_empty() {
        return Ok(HashSet::new());
    }
    sqlx::query_as::<_, ReadablePreparedScope>(CURRENT_READABLE_PREPARED_SCOPES_SQL)
        .bind(&revision_ids)
        .fetch_all(&state.persistence.postgres)
        .await
        .context("failed to resolve current readable prepared answer evidence")
        .map(|rows| rows.into_iter().collect())
}

async fn load_authoritative_technical_facts(
    state: &AppState,
    fact_ids: &[Uuid],
    chunk_supported_facts: Vec<crate::infra::knowledge_rows::KnowledgeTechnicalFactRow>,
) -> anyhow::Result<Vec<crate::infra::knowledge_rows::KnowledgeTechnicalFactRow>> {
    let mut technical_facts = state
        .document_store
        .list_technical_facts_by_ids(fact_ids)
        .await
        .context("failed to load technical facts for authoritative answer context")?;
    let mut seen_fact_ids = technical_facts.iter().map(|fact| fact.fact_id).collect::<HashSet<_>>();
    technical_facts.extend(
        chunk_supported_facts
            .into_iter()
            .filter(|fact| fact_ids.contains(&fact.fact_id) && seen_fact_ids.insert(fact.fact_id)),
    );
    technical_facts.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    Ok(technical_facts)
}

fn answer_source_block_ids(
    evidence_rows: &[crate::infra::knowledge_rows::KnowledgeEvidenceRow],
    chunk_rows: &[KnowledgeChunkRow],
    technical_facts: &[crate::infra::knowledge_rows::KnowledgeTechnicalFactRow],
) -> Vec<Uuid> {
    let mut block_ids =
        evidence_rows.iter().filter_map(|evidence| evidence.block_id).collect::<Vec<_>>();
    let mut seen = block_ids.iter().copied().collect::<HashSet<_>>();
    block_ids.extend(
        chunk_rows
            .iter()
            .flat_map(|chunk| chunk.support_block_ids.iter().copied())
            .chain(technical_facts.iter().flat_map(|fact| fact.support_block_ids.iter().copied()))
            .filter(|block_id| seen.insert(*block_id)),
    );
    block_ids
}

pub(crate) fn selected_fact_ids_for_authoritative_evidence(
    selected_fact_ids: &[Uuid],
    evidence_rows: &[crate::infra::knowledge_rows::KnowledgeEvidenceRow],
    chunk_supported_facts: &[crate::infra::knowledge_rows::KnowledgeTechnicalFactRow],
) -> Vec<Uuid> {
    let mut fact_ids = selected_fact_ids.to_vec();
    for evidence in evidence_rows {
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        if fact_ids.len() >= MAX_AUTHORITATIVE_ANSWER_TECHNICAL_FACTS {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    if fact_ids.is_empty() {
        for fact in chunk_supported_facts {
            if fact_ids.len() >= MAX_AUTHORITATIVE_ANSWER_TECHNICAL_FACTS {
                break;
            }
            if !fact_ids.contains(&fact.fact_id) {
                fact_ids.push(fact.fact_id);
            }
        }
    }
    fact_ids.truncate(MAX_AUTHORITATIVE_ANSWER_TECHNICAL_FACTS);
    fact_ids
}

pub(crate) fn build_authoritative_answer_context(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    technical_literals_text: Option<&str>,
    evidence: &AuthoritativeAnswerEvidence,
    primary_answer_chunks: &[RuntimeMatchedChunk],
    graph_evidence_context_lines: &[String],
) -> String {
    let focused_document_id = focus_document_id(question, query_ir, primary_answer_chunks);
    let focused_document_label = focused_document_id.and_then(|document_id| {
        primary_answer_chunks
            .iter()
            .find(|chunk| chunk.document_id == document_id)
            .map(|chunk| chunk.document_label.clone())
    });
    let filtered_technical_facts = focused_document_id.map_or_else(
        || evidence.technical_facts.clone(),
        |document_id| {
            evidence
                .technical_facts
                .iter()
                .filter(|fact| fact.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_structured_blocks = focused_document_id.map_or_else(
        || evidence.structured_blocks.clone(),
        |document_id| {
            evidence
                .structured_blocks
                .iter()
                .filter(|block| block.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_chunks = focused_document_id.map_or_else(
        || primary_answer_chunks.to_vec(),
        |document_id| {
            primary_answer_chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let mut sections = Vec::<String>::new();

    if let Some(technical_literals_text) = technical_literals_text
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }

    if let Some(document_label) = focused_document_label.as_deref() {
        sections.push(format!("Focused grounded document\n- {document_label}"));
        sections.push(
            "When a document summary is available in the context, use it to frame the answer."
                .to_string(),
        );
    }

    let graph_evidence_section = render_graph_evidence_context_lines_for_focus(
        question,
        graph_evidence_context_lines,
        focused_document_label.as_deref(),
        query_ir,
    );
    if !graph_evidence_section.is_empty() {
        sections.push(graph_evidence_section);
    }

    let table_summary_section =
        render_table_summary_chunk_section(question, Some(query_ir), &filtered_chunks);
    let suppress_tabular_detail = question_asks_table_aggregation(question, Some(query_ir))
        && !table_summary_section.is_empty();
    if !table_summary_section.is_empty() {
        sections.push(table_summary_section);
    }

    if !suppress_tabular_detail {
        let technical_fact_section =
            render_authoritative_technical_fact_section(&filtered_technical_facts);
        if !technical_fact_section.is_empty() {
            sections.push(technical_fact_section);
        }
    }

    let prepared_segment_section = render_prepared_segment_section(
        question,
        Some(query_ir),
        &filtered_structured_blocks,
        suppress_tabular_detail,
    );
    if !prepared_segment_section.is_empty() {
        sections.push(prepared_segment_section);
    }

    let chunk_section =
        render_primary_chunk_section(question, query_ir, &filtered_chunks, suppress_tabular_detail);
    if !chunk_section.is_empty() {
        sections.push(chunk_section);
    }

    if let Some(bundle) = evidence.bundle.as_ref() {
        sections.insert(
            0,
            format!(
                "Compiled query bundle\n- Strategy: {}\n- Requested mode: {}\n- Resolved mode: {}",
                bundle.bundle_strategy, bundle.requested_mode, bundle.resolved_mode
            ),
        );
    }

    sections.join("\n\n")
}

fn render_graph_evidence_context_lines(graph_evidence_context_lines: &[String]) -> String {
    let mut lines = Vec::<String>::new();
    let mut seen = BTreeSet::<String>::new();
    for line in graph_evidence_context_lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_string()) {
            lines.push(trimmed.to_string());
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("Retrieved graph evidence\n{}", lines.join("\n"))
    }
}

pub(super) fn render_graph_evidence_context_lines_for_focus(
    _question: &str,
    graph_evidence_context_lines: &[String],
    focused_document_label: Option<&str>,
    query_ir: &QueryIR,
) -> String {
    let Some(focused_document_label) = focused_document_label else {
        return render_graph_evidence_context_lines(graph_evidence_context_lines);
    };
    if !matches!(query_ir.scope, QueryScope::SingleDocument) {
        return render_graph_evidence_context_lines(graph_evidence_context_lines);
    }
    let focus_tokens = query_ir_document_focus_tokens(query_ir)
        .unwrap_or_else(|| normalized_alnum_tokens(focused_document_label, 3));
    if focus_tokens.is_empty() {
        return render_graph_evidence_context_lines(graph_evidence_context_lines);
    }
    let focused_lines = graph_evidence_context_lines
        .iter()
        .filter(|line| {
            let line_tokens = normalized_alnum_tokens(line, 3);
            focus_token_overlap_count(&focus_tokens, &line_tokens) > 0
        })
        .cloned()
        .collect::<Vec<_>>();
    render_graph_evidence_context_lines(&focused_lines)
}

fn focus_document_id(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if contextual_low_confidence_setup_context_should_stay_broad(question, query_ir, chunks) {
        return None;
    }
    let document_ids = explicit_target_document_ids_from_values(
        question,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    (document_ids.len() == 1).then(|| document_ids.iter().next().copied()).flatten().or_else(|| {
        query_ir_focus_document_id(
            query_ir,
            chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
        )
        .or_else(|| dominant_single_document_context_id(query_ir, chunks))
    })
}

fn dominant_single_document_context_id(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if !matches!(query_ir.scope, QueryScope::SingleDocument) || chunks.is_empty() {
        return None;
    }
    if !dominant_single_document_context_allowed(query_ir, chunks) {
        return None;
    }
    let mut by_document = HashMap::<Uuid, (f32, usize)>::new();
    for chunk in chunks {
        let score = score_value(chunk.score).max(0.0);
        by_document
            .entry(chunk.document_id)
            .and_modify(|entry| {
                entry.0 += score;
                entry.1 = entry.1.saturating_add(1);
            })
            .or_insert((score, 1));
    }
    let mut ranked = by_document.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .0
            .total_cmp(&left.1.0)
            .then_with(|| right.1.1.cmp(&left.1.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    let (best_document_id, (best_score, best_count)) = ranked.first().copied()?;
    let runner_score = ranked.get(1).map(|(_, (score, _))| *score).unwrap_or(0.0);
    (best_count >= 2 && best_score >= runner_score.mul_add(1.15, 1.0)).then_some(best_document_id)
}

fn dominant_single_document_context_allowed(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if query_ir.document_focus.is_some() {
        return true;
    }
    let document_count = chunks.iter().map(|chunk| chunk.document_id).collect::<HashSet<_>>().len();
    if document_count <= 1 {
        return true;
    }
    if matches!(query_ir.act, QueryAct::RetrieveValue | QueryAct::Enumerate | QueryAct::Compare)
        || broad_unfocused_procedure_source_coverage(query_ir)
    {
        return false;
    }
    !query_ir.targets(QueryTargetKind::TableRow) && !query_ir.targets(QueryTargetKind::TableSummary)
}

fn contextual_low_confidence_setup_context_should_stay_broad(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    structured_current_question_segment(question).is_some()
        && low_confidence_unfocused_descriptive_query(query_ir)
        && setup_like_document_count(chunks) > 1
}

fn low_confidence_unfocused_descriptive_query(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
}

fn setup_like_document_count(chunks: &[RuntimeMatchedChunk]) -> usize {
    chunks
        .iter()
        .filter(|chunk| {
            setup_like_text_signal(&chunk.excerpt) || setup_like_text_signal(&chunk.source_text)
        })
        .map(|chunk| chunk.document_id)
        .collect::<HashSet<_>>()
        .len()
}

fn setup_like_text_signal(text: &str) -> bool {
    setup_like_configuration_path_count(text) > 0
        || setup_like_assignment_count(text) > 0
        || setup_like_section_count(text) > 0
        || extract_parameter_literals(text, 8).len() >= 2
}

fn setup_like_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 8)
        .into_iter()
        .filter(|path| {
            let lowered = path.to_ascii_lowercase();
            [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
                .iter()
                .any(|extension| lowered.ends_with(extension))
        })
        .count()
}

fn setup_like_assignment_count(text: &str) -> usize {
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
        .take(8)
        .count()
}

fn setup_like_section_count(text: &str) -> usize {
    text.split_whitespace()
        .filter(|token| {
            let cleaned = token.trim_matches(|ch: char| {
                matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.' | '(' | ')' | '{' | '}')
            });
            cleaned.len() > 2 && cleaned.starts_with('[') && cleaned.ends_with(']')
        })
        .take(8)
        .count()
}

pub(super) fn query_ir_focus_document_id<'a, I>(
    query_ir: &QueryIR,
    document_values: I,
) -> Option<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    if !matches!(query_ir.scope, QueryScope::SingleDocument) || query_ir.document_focus.is_none() {
        return None;
    }
    let focus_tokens = query_ir_document_focus_tokens(query_ir)?;

    let mut best_scores = HashMap::<Uuid, usize>::new();
    for (document_id, value) in document_values {
        let value_tokens = normalized_alnum_tokens(value, 3);
        let overlap = focus_token_overlap_count(&focus_tokens, &value_tokens);
        if overlap == 0 {
            continue;
        }
        best_scores
            .entry(document_id)
            .and_modify(|score| *score = (*score).max(overlap))
            .or_insert(overlap);
    }
    let max_score = best_scores.values().copied().max().unwrap_or_default();
    if max_score == 0 {
        return None;
    }
    let best_document_ids = best_scores
        .into_iter()
        .filter_map(|(document_id, score)| (score == max_score).then_some(document_id))
        .collect::<BTreeSet<_>>();
    (best_document_ids.len() == 1).then(|| best_document_ids.iter().next().copied()).flatten()
}

pub(crate) fn query_ir_document_focus_tokens(query_ir: &QueryIR) -> Option<BTreeSet<String>> {
    let tokens = query_ir
        .document_focus
        .as_ref()
        .map(|document_focus| normalized_alnum_tokens(document_focus.hint.trim(), 3))
        .unwrap_or_default();
    (!tokens.is_empty()).then_some(tokens)
}

pub(crate) fn focus_token_overlap_count(
    focus_tokens: &BTreeSet<String>,
    value_tokens: &BTreeSet<String>,
) -> usize {
    focus_tokens
        .iter()
        .filter(|focus_token| {
            value_tokens.iter().any(|value_token| {
                focus_token == &value_token
                    || near_token_match(focus_token, value_token)
                    || focus_token_is_value_prefix(focus_token, value_token)
            })
        })
        .count()
}

fn focus_token_is_value_prefix(focus_token: &str, value_token: &str) -> bool {
    let focus_len = focus_token.chars().count();
    let value_len = value_token.chars().count();
    focus_len >= 3 && value_len > focus_len && value_token.starts_with(focus_token)
}

/// Collect the set of revision_ids that belong to image-source documents
/// (`source_format == "image"`) by batch-fetching the structured revision
/// records for every distinct revision referenced by the given chunks.
pub(super) async fn load_image_revision_ids(
    state: &AppState,
    chunks: &[RuntimeMatchedChunk],
) -> anyhow::Result<HashSet<Uuid>> {
    let revision_ids: Vec<Uuid> = {
        let mut seen = HashSet::new();
        chunks.iter().filter(|c| seen.insert(c.revision_id)).map(|c| c.revision_id).collect()
    };
    if revision_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let revisions = state
        .document_store
        .list_structured_revisions_by_revision_ids(&revision_ids)
        .await
        .context("failed to load structured revisions for image deprioritization")?;
    Ok(revisions
        .into_iter()
        .filter(|r| r.source_format == "image")
        .map(|r| r.revision_id)
        .collect())
}

/// Push chunks whose revision is an image-source document to the tail of the
/// list, preserving relative order within each group.  If every chunk in the
/// list is an image-source chunk (e.g. an image-only document query), the
/// order is left unchanged so image OCR stubs still surface.
pub(crate) fn deprioritize_image_source_chunks(
    chunks: &mut [RuntimeMatchedChunk],
    image_revision_ids: &HashSet<Uuid>,
) {
    if image_revision_ids.is_empty() {
        return;
    }
    let all_image = chunks.iter().all(|c| image_revision_ids.contains(&c.revision_id));
    if all_image {
        return;
    }
    // stable_partition: non-image first, image last
    chunks.sort_by_key(|c| image_revision_ids.contains(&c.revision_id));
}
