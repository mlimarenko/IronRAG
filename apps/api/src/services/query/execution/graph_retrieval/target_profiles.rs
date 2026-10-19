use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

use crate::{
    domains::query_ir::{EntityRole, QueryAct, QueryIR, literal_text_is_identifier_shaped},
    services::query::text_match::{
        RelatedTokenCandidate, RelatedTokenSelection, build_related_token_candidates,
        normalized_alnum_token_sequence, select_related_overlap_tokens_from_candidates,
        token_sequence_exact_or_contains_tokens,
    },
    shared::text_tokens::literal_wildcard_prefixes,
};

use super::super::QueryGraphIndex;

#[derive(Debug, Clone)]
pub(crate) struct GraphTargetEntityProfile {
    pub(super) profile_key: String,
    pub(super) target_label_tokens: Vec<String>,
    pub(super) wildcard_prefixes: Vec<String>,
    /// Exact-match terms for short identifier-shaped mentions (e.g. all-uppercase
    /// acronyms) that produce no alnum tokens under the min-3 filter and no wildcard
    /// prefixes. A non-empty list signals that this profile was built for an
    /// identifier-shaped short mention; the field scorer matches a graph node when
    /// its label or any alias_json entry equals one of these terms
    /// (case-insensitive, trimmed). The wildcard and token paths are both empty when
    /// this list is non-empty.
    pub(super) exact_label_terms: Vec<String>,
    pub(super) related_tokens: RelatedTokenSelection,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum GraphTargetEntityCoverageFieldKind {
    Label,
    Alias,
    Summary,
    Evidence,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GraphTargetEntityCoverageField<'a> {
    pub(crate) text: &'a str,
    pub(crate) kind: GraphTargetEntityCoverageFieldKind,
}

pub(crate) fn graph_target_entity_profiles(
    query_ir: Option<&QueryIR>,
    graph_index: &QueryGraphIndex,
) -> Vec<GraphTargetEntityProfile> {
    let Some(ir) = query_ir else {
        return Vec::new();
    };
    let mut seen = BTreeSet::new();
    let related_candidates =
        build_related_token_candidates(graph_index.nodes().map(|node| node.label.as_str()), 3);
    let mut profiles = graph_target_profiles_from_labels(
        graph_target_profile_labels(ir),
        &related_candidates,
        &mut seen,
    );
    if graph_target_profiles_need_entity_fallback(ir, &profiles, graph_index) {
        profiles.extend(graph_target_profiles_from_labels(
            graph_target_entity_profile_labels(ir),
            &related_candidates,
            &mut seen,
        ));
    }
    profiles
}

fn graph_target_profiles_from_labels(
    labels: Vec<String>,
    related_candidates: &[RelatedTokenCandidate],
    seen: &mut BTreeSet<String>,
) -> Vec<GraphTargetEntityProfile> {
    labels
        .into_iter()
        .filter_map(|label| graph_target_profile_from_label(&label, related_candidates, seen))
        .collect()
}

fn graph_target_profile_from_label(
    label: &str,
    related_candidates: &[RelatedTokenCandidate],
    seen: &mut BTreeSet<String>,
) -> Option<GraphTargetEntityProfile> {
    let label = label.trim();
    if label.is_empty() {
        return None;
    }

    let target_label_tokens = normalized_alnum_token_sequence(label, 3);
    let target_tokens = target_label_tokens.iter().cloned().collect::<BTreeSet<_>>();
    let wildcard_prefixes = literal_wildcard_prefixes(label, 2);
    let exact_label_terms =
        exact_label_terms_for_profile(label, &target_tokens, &wildcard_prefixes)?;
    let profile_key =
        graph_target_profile_key(&target_tokens, &wildcard_prefixes, &exact_label_terms);
    if !seen.insert(profile_key.clone()) {
        return None;
    }

    Some(GraphTargetEntityProfile {
        profile_key,
        target_label_tokens,
        wildcard_prefixes,
        exact_label_terms,
        related_tokens: select_related_overlap_tokens_from_candidates(label, related_candidates, 3),
    })
}

fn exact_label_terms_for_profile(
    label: &str,
    target_tokens: &BTreeSet<String>,
    wildcard_prefixes: &[String],
) -> Option<Vec<String>> {
    if !target_tokens.is_empty() || !wildcard_prefixes.is_empty() {
        return Some(Vec::new());
    }

    let alphanumeric_count = label.chars().filter(|ch| ch.is_alphanumeric()).count();
    (alphanumeric_count >= 2 && literal_text_is_identifier_shaped(label))
        .then(|| vec![label.to_string()])
}

fn graph_target_profile_key(
    target_tokens: &BTreeSet<String>,
    wildcard_prefixes: &[String],
    exact_label_terms: &[String],
) -> String {
    if !exact_label_terms.is_empty() {
        return format!("exact:{}", exact_label_terms.join("\u{0}"));
    }
    if wildcard_prefixes.is_empty() {
        return target_tokens.iter().cloned().collect::<Vec<_>>().join("\u{0}");
    }
    format!("wildcard:{}", wildcard_prefixes.join("\u{0}"))
}

fn graph_target_profiles_need_entity_fallback(
    ir: &QueryIR,
    profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
) -> bool {
    graph_target_profiles_suppress_incomplete_compare_entities(ir)
        && !ir.target_entities.is_empty()
        && !profiles
            .iter()
            .any(|profile| graph_target_entity_profile_matches_graph(profile, graph_index))
}

fn graph_target_entity_profile_matches_graph(
    profile: &GraphTargetEntityProfile,
    graph_index: &QueryGraphIndex,
) -> bool {
    graph_index.nodes().any(|node| {
        let aliases = crate::shared::json_coercion::from_value_or_default::<Vec<String>>(
            "runtime_graph_node.aliases_json",
            &node.aliases_json,
        );
        let mut fields = vec![GraphTargetEntityCoverageField {
            text: node.label.as_str(),
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }];
        for alias in &aliases {
            fields.push(GraphTargetEntityCoverageField {
                text: alias.as_str(),
                kind: GraphTargetEntityCoverageFieldKind::Alias,
            });
        }
        if let Some(summary) = node.summary.as_deref() {
            fields.push(GraphTargetEntityCoverageField {
                text: summary,
                kind: GraphTargetEntityCoverageFieldKind::Summary,
            });
        }
        graph_target_entity_profile_field_score(&fields, profile).is_some()
    })
}

fn graph_target_profile_labels(ir: &QueryIR) -> Vec<String> {
    let mut labels = Vec::new();
    if let Some(document_focus) = ir.document_focus.as_ref() {
        push_unique_graph_target_profile_label(&mut labels, &document_focus.hint);
    }

    if !graph_target_profiles_suppress_incomplete_compare_entities(ir) {
        push_graph_target_entity_profile_labels(&mut labels, ir);
    }
    labels
}

fn graph_target_entity_profile_labels(ir: &QueryIR) -> Vec<String> {
    let mut labels = Vec::new();
    push_graph_target_entity_profile_labels(&mut labels, ir);
    labels
}

fn push_graph_target_entity_profile_labels(labels: &mut Vec<String>, ir: &QueryIR) {
    for mention in ir.target_entities.iter().filter(|mention| mention.role == EntityRole::Subject) {
        push_unique_graph_target_profile_label(labels, &mention.label);
    }

    if labels.is_empty() || matches!(ir.act, QueryAct::Compare) || ir.is_multi_document() {
        for mention in &ir.target_entities {
            push_unique_graph_target_profile_label(labels, &mention.label);
        }
    }
}

fn graph_target_profiles_suppress_incomplete_compare_entities(ir: &QueryIR) -> bool {
    matches!(ir.act, QueryAct::Compare)
        && ir.document_focus.is_some()
        && comparison_operand_count(ir) < 2
}

fn comparison_operand_count(ir: &QueryIR) -> usize {
    let Some(comparison) = &ir.comparison else {
        return 0;
    };
    [&comparison.a, &comparison.b]
        .into_iter()
        .filter_map(|value| value.as_deref())
        .filter(|value| !value.trim().is_empty())
        .count()
}

fn push_unique_graph_target_profile_label(labels: &mut Vec<String>, label: &str) {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return;
    }
    let normalized = trimmed.to_lowercase();
    if labels.iter().any(|existing| existing.to_lowercase() == normalized) {
        return;
    }
    labels.push(trimmed.to_string());
}

pub(crate) fn graph_target_entity_coverage_score(
    fields: &[GraphTargetEntityCoverageField<'_>],
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> usize {
    const SINGLE_PROFILE_BASE_SCORE: usize = 10_000;
    const MULTI_PROFILE_BASE_SCORE: usize = 50_000;
    const MULTI_PROFILE_STEP_SCORE: usize = 1_000;

    if fields.is_empty() || target_entity_profiles.is_empty() {
        return 0;
    }

    let mut matched_profile_count = 0usize;
    let mut matched_score = 0usize;
    let mut matched_profiles = BTreeSet::new();
    for profile in target_entity_profiles {
        let Some(profile_score) = graph_target_entity_profile_field_score(fields, profile) else {
            continue;
        };
        if matched_profiles.insert(profile.profile_key.as_str()) {
            matched_profile_count += 1;
            matched_score = matched_score.saturating_add(profile_score);
        }
    }
    if matched_profile_count == 0 {
        return 0;
    }

    let base = if matched_profile_count > 1 {
        MULTI_PROFILE_BASE_SCORE
            .saturating_add(matched_profile_count.saturating_mul(MULTI_PROFILE_STEP_SCORE))
    } else {
        SINGLE_PROFILE_BASE_SCORE
    };
    base.saturating_add(matched_score)
}

pub(super) fn graph_target_entity_profile_field_score(
    fields: &[GraphTargetEntityCoverageField<'_>],
    profile: &GraphTargetEntityProfile,
) -> Option<usize> {
    if !profile.wildcard_prefixes.is_empty() {
        return fields
            .iter()
            .filter_map(|field| {
                if !matches!(
                    field.kind,
                    GraphTargetEntityCoverageFieldKind::Label
                        | GraphTargetEntityCoverageFieldKind::Alias
                ) {
                    return None;
                }
                let field_text = normalized_prefix_match_text(field.text);
                profile
                    .wildcard_prefixes
                    .iter()
                    .any(|prefix| field_text.starts_with(prefix))
                    .then_some(graph_target_entity_wildcard_field_score(field.kind))
            })
            .max();
    }

    // Exact-label-term branch: matches label or alias fields whose lowercased
    // trimmed text equals one of the profile's exact terms. Used for short
    // identifier-shaped mentions that produce no alnum tokens under min-3.
    if !profile.exact_label_terms.is_empty() {
        return fields
            .iter()
            .filter_map(|field| {
                if !matches!(
                    field.kind,
                    GraphTargetEntityCoverageFieldKind::Label
                        | GraphTargetEntityCoverageFieldKind::Alias
                ) {
                    return None;
                }
                let field_lower = field.text.trim().to_lowercase();
                profile
                    .exact_label_terms
                    .iter()
                    .any(|term| term.to_lowercase() == field_lower)
                    .then_some(graph_target_entity_exact_field_score(field.kind))
            })
            .max();
    }

    fields
        .iter()
        .filter_map(|field| {
            let field_text = field.text.trim();
            if field_text.is_empty() {
                return None;
            }
            let field_tokens = normalized_alnum_token_sequence(field_text, 3);
            if token_sequence_exact_or_contains_tokens(&field_tokens, &profile.target_label_tokens)
            {
                return Some(graph_target_entity_exact_field_score(field.kind));
            }
            let field_token_set = field_tokens.iter().cloned().collect::<BTreeSet<_>>();
            if !profile.related_tokens.is_empty()
                && profile.related_tokens.matches_tokens(&field_token_set)
            {
                return Some(graph_target_entity_related_field_score(field.kind));
            }
            None
        })
        .max()
}

fn normalized_prefix_match_text(value: &str) -> String {
    value.nfkc().flat_map(char::to_lowercase).collect::<String>().trim().to_string()
}

const fn graph_target_entity_wildcard_field_score(
    kind: GraphTargetEntityCoverageFieldKind,
) -> usize {
    match kind {
        GraphTargetEntityCoverageFieldKind::Label => 180,
        GraphTargetEntityCoverageFieldKind::Alias => 160,
        GraphTargetEntityCoverageFieldKind::Evidence
        | GraphTargetEntityCoverageFieldKind::Summary => 0,
    }
}

const fn graph_target_entity_exact_field_score(kind: GraphTargetEntityCoverageFieldKind) -> usize {
    match kind {
        GraphTargetEntityCoverageFieldKind::Label => 160,
        GraphTargetEntityCoverageFieldKind::Alias => 140,
        GraphTargetEntityCoverageFieldKind::Evidence => 110,
        GraphTargetEntityCoverageFieldKind::Summary => 60,
    }
}

const fn graph_target_entity_related_field_score(
    kind: GraphTargetEntityCoverageFieldKind,
) -> usize {
    match kind {
        GraphTargetEntityCoverageFieldKind::Label => 80,
        GraphTargetEntityCoverageFieldKind::Alias => 70,
        GraphTargetEntityCoverageFieldKind::Evidence => 55,
        GraphTargetEntityCoverageFieldKind::Summary => 30,
    }
}
