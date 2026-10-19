use super::{
    BTreeMap, BTreeSet, DeterministicAnswerLabels, EntityRole, LiteralKind, QueryAct, QueryIR,
    QueryTargetKind, RuntimeMatchedChunk, SetupConfigurationLiterals, Uuid,
    deterministic_answer_labels, extract_config_assignment_literals,
    extract_configuration_path_literals, extract_configuration_section_literals,
    extract_package_command_literals, extract_parameter_literals, extract_semver_like_version,
    extract_setup_configuration_command_literals, extract_setup_configuration_parameter_rows,
    filter_setup_configuration_parameters, normalized_alnum_token_sequence,
    normalized_alnum_tokens, push_unique_parameter_rows, push_unique_values,
    query_has_multi_document_setup_anchors, query_ir_has_setup_configuration_target,
    query_ir_is_unambiguous_versioned_procedure, query_ir_requires_remediation_synthesis,
    repair_technical_layout_noise, setup_configuration_default_value_is_assignment_scalar,
    setup_configuration_label_focus_score, setup_configuration_literal_key,
    setup_configuration_variant_score,
};

#[derive(Debug, Clone)]
pub(in crate::services::query::execution) struct SetupConfigurationAnchorCandidate {
    answer: String,
    is_multi_variant: bool,
    has_parameter_details: bool,
    has_actionable_anchor: bool,
}

impl SetupConfigurationAnchorCandidate {
    pub(in crate::services::query::execution) fn into_answer(self) -> String {
        self.answer
    }
    pub(in crate::services::query::execution) fn is_multi_variant(&self) -> bool {
        self.is_multi_variant
    }
    pub(in crate::services::query::execution) fn has_parameter_details(&self) -> bool {
        self.has_parameter_details
    }
    pub(in crate::services::query::execution) fn should_use_as_direct_answer(
        &self,
        query_ir: &QueryIR,
        chunks: &[RuntimeMatchedChunk],
    ) -> bool {
        !query_ir_requires_remediation_synthesis(query_ir)
            && !self.is_multi_variant()
            && query_ir_has_setup_configuration_target(query_ir)
            && (self.should_use_as_preflight_answer(query_ir, chunks) || self.has_actionable_anchor)
    }
    pub(in crate::services::query::execution) fn should_use_as_preflight_answer(
        &self,
        query_ir: &QueryIR,
        chunks: &[RuntimeMatchedChunk],
    ) -> bool {
        !query_ir_requires_remediation_synthesis(query_ir)
            && !self.is_multi_variant()
            && query_ir_has_setup_configuration_target(query_ir)
            && (query_has_multi_document_setup_anchors(query_ir, chunks)
                || self.has_parameter_details())
    }
}

#[cfg(test)]
pub(in crate::services::query::execution) fn build_setup_configuration_anchor_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_setup_configuration_anchor_candidate(question, query_ir, chunks)
        .map(SetupConfigurationAnchorCandidate::into_answer)
}
pub(in crate::services::query::execution) fn build_setup_configuration_anchor_candidate(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<SetupConfigurationAnchorCandidate> {
    const SETUP_CONFIGURATION_RENDER_VARIANT_LIMIT: usize = 8;
    if !query_ir_requests_setup_configuration_answer(question, query_ir) {
        return None;
    }
    let variants = setup_configuration_variants_from_chunks(question, query_ir, chunks);
    if variants.is_empty() {
        return None;
    }
    let labels = deterministic_answer_labels(query_ir);
    let is_multi_variant = setup_configuration_has_distinct_variants(&variants);
    let has_parameter_details = variants.iter().any(|variant| !variant.parameter_rows.is_empty());
    let has_actionable_anchor = variants.iter().any(|variant| {
        !variant.packages.is_empty()
            || !variant.reconfigure_packages.is_empty()
            || !variant.paths.is_empty()
    });
    let mut lines = Vec::new();
    if is_multi_variant {
        lines.push(format!("**{}:**", labels.variants));
        lines.push(String::new());
    }
    for variant in variants.iter().take(SETUP_CONFIGURATION_RENDER_VARIANT_LIMIT) {
        render_setup_configuration_variant(&mut lines, variant, labels);
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    Some(SetupConfigurationAnchorCandidate {
        answer: lines.join("\n"),
        is_multi_variant,
        has_parameter_details,
        has_actionable_anchor,
    })
}

fn render_setup_configuration_variant(
    lines: &mut Vec<String>,
    variant: &SetupConfigurationVariant,
    labels: DeterministicAnswerLabels,
) {
    lines.push(format!("**{}:** **{}**", labels.source, variant.source));
    push_setup_configuration_values(lines, labels.package, &variant.packages);
    push_setup_configuration_values(lines, labels.reconfigure, &variant.reconfigure_packages);
    push_setup_configuration_values(lines, labels.path, &variant.paths);
    if !variant.sections.is_empty() {
        let sections =
            variant.sections.iter().map(|section| format!("[{section}]")).collect::<Vec<_>>();
        push_setup_configuration_values(lines, labels.section, &sections);
    }
    push_setup_configuration_values(lines, labels.parameter, &variant.parameters);
    if !variant.parameter_rows.is_empty() {
        lines.push(format!("- **{}:**", labels.parameter_details));
        lines.extend(
            variant
                .parameter_rows
                .iter()
                .map(|row| format!("  - `{}` — {}", row.name, row.render_details(labels))),
        );
    }
    lines.push(String::new());
}

fn push_setup_configuration_values(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if !values.is_empty() {
        lines.push(format!("- **{label}:** `{}`", values.join("`, `")));
    }
}

fn query_ir_requests_setup_configuration_answer(question: &str, query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) || query_ir.source_slice.is_some() {
        return false;
    }
    if query_ir_has_relational_focus(query_ir) {
        return false;
    }
    if query_ir_is_unambiguous_versioned_procedure(query_ir) {
        return false;
    }
    if query_ir_requests_service_port_answer(query_ir) {
        return false;
    }
    if query_ir_has_setup_configuration_target(query_ir) || query_ir.document_focus.is_some() {
        return true;
    }
    if !query_ir_has_subject_focus_identity(query_ir) {
        return false;
    }
    !question_requests_lifecycle_update_procedure(question, query_ir)
}

pub(super) fn question_requests_lifecycle_update_procedure(
    _question: &str,
    query_ir: &QueryIR,
) -> bool {
    if query_ir_is_unambiguous_versioned_procedure(query_ir) {
        return true;
    }
    if query_ir.targets_any(&[
        QueryTargetKind::Version,
        QueryTargetKind::Release,
        QueryTargetKind::Changelog,
    ]) {
        return true;
    }
    if query_ir.source_slice.as_ref().is_some_and(|slice| {
        matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
    }) {
        return true;
    }
    if query_ir
        .literal_constraints
        .iter()
        .any(|literal| matches!(literal.kind, LiteralKind::Version))
        || query_ir
            .retrieval_query
            .as_deref()
            .is_some_and(|query| extract_semver_like_version(query).is_some())
    {
        return true;
    }
    false
}

fn query_ir_has_relational_focus(query_ir: &QueryIR) -> bool {
    query_ir.target_entities.iter().any(|entity| !matches!(entity.role, EntityRole::Subject))
}

fn query_ir_has_subject_focus_identity(query_ir: &QueryIR) -> bool {
    query_ir
        .target_entities
        .iter()
        .any(|entity| matches!(entity.role, EntityRole::Subject) && !entity.label.trim().is_empty())
}

fn query_ir_requests_service_port_answer(query_ir: &QueryIR) -> bool {
    query_ir.targets_any(&[
        QueryTargetKind::Connection,
        QueryTargetKind::Port,
        QueryTargetKind::Protocol,
        QueryTargetKind::Service,
    ])
}

#[derive(Debug, Clone)]
struct SetupConfigurationVariant {
    source: String,
    score: usize,
    focus_score: usize,
    label_focus_score: usize,
    packages: Vec<String>,
    reconfigure_packages: Vec<String>,
    paths: Vec<String>,
    sections: Vec<String>,
    parameters: Vec<String>,
    parameter_rows: Vec<SetupConfigurationParameterRow>,
}

#[derive(Debug, Clone)]
pub(super) struct SetupConfigurationParameterRow {
    pub(super) name: String,
    pub(super) details: Vec<(String, String)>,
    pub(super) positional_assignment_value: Option<String>,
}

impl SetupConfigurationParameterRow {
    fn render_details(&self, _labels: DeterministicAnswerLabels) -> String {
        let mut rendered = self
            .details
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if rendered.is_empty()
            && let Some(value) = self.positional_assignment_value.as_deref()
        {
            rendered.push_str(value);
        }
        rendered
    }

    fn positional_assignment(&self) -> Option<String> {
        let value = self.positional_assignment_value.as_deref()?.trim();
        if !setup_configuration_default_value_is_assignment_scalar(value) {
            return None;
        }
        Some(format!("{} = {value}", self.name))
    }
}

fn setup_configuration_variants_from_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SetupConfigurationVariant> {
    const SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT: usize = 24;
    const SETUP_CONFIGURATION_PARAMETER_ROW_LIMIT: usize = 16;

    let focus_tokens = setup_configuration_focus_tokens(question, query_ir);
    let subject_label_sequences = setup_configuration_subject_label_sequences(query_ir);
    let subject_tokens = setup_configuration_subject_tokens(query_ir);
    let mut variants = BTreeMap::<Uuid, SetupConfigurationVariant>::new();
    for chunk in chunks {
        let Some(contribution) =
            setup_configuration_variant_from_chunk(chunk, &focus_tokens, &subject_label_sequences)
        else {
            continue;
        };
        merge_setup_configuration_variant(
            &mut variants,
            chunk,
            contribution,
            SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT,
            SETUP_CONFIGURATION_PARAMETER_ROW_LIMIT,
        );
    }
    let mut variants =
        variants.into_values().filter(setup_configuration_variant_has_anchor).collect::<Vec<_>>();
    let has_label_focus = !subject_label_sequences.is_empty()
        && variants.iter().any(|variant| variant.label_focus_score > 0);
    if has_label_focus {
        variants.retain(|variant| variant.label_focus_score > 0);
    } else if variants.iter().any(|variant| variant.focus_score > 0) {
        variants.retain(|variant| variant.focus_score > 0);
    }
    variants.sort_by(|left, right| {
        if has_label_focus {
            right
                .label_focus_score
                .cmp(&left.label_focus_score)
                .then_with(|| right.focus_score.cmp(&left.focus_score))
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.source.cmp(&right.source))
        } else if focus_tokens.is_empty() {
            right.score.cmp(&left.score).then_with(|| left.source.cmp(&right.source))
        } else {
            right
                .focus_score
                .cmp(&left.focus_score)
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.source.cmp(&right.source))
        }
    });
    let diversity_terms = if subject_tokens.is_empty() { &focus_tokens } else { &subject_tokens };
    diversify_setup_configuration_variants(variants, diversity_terms)
}

struct SetupConfigurationVariantContribution {
    score: usize,
    focus_score: usize,
    label_focus_score: usize,
    packages: Vec<String>,
    reconfigure_packages: Vec<String>,
    paths: Vec<String>,
    sections: Vec<String>,
    parameters: Vec<String>,
    parameter_rows: Vec<SetupConfigurationParameterRow>,
}

fn setup_configuration_variant_from_chunk(
    chunk: &RuntimeMatchedChunk,
    focus_tokens: &BTreeSet<String>,
    subject_label_sequences: &[Vec<String>],
) -> Option<SetupConfigurationVariantContribution> {
    const PARAMETER_LITERAL_LIMIT: usize = 24;
    const PARAMETER_ROW_LIMIT: usize = 16;
    let text = repair_technical_layout_noise(&format!("{}\n{}", chunk.source_text, chunk.excerpt));
    let packages = extract_package_command_literals(&text, 4);
    let paths = extract_configuration_path_literals(&text, 4);
    let sections = extract_configuration_section_literals(&text, 4);
    let reconfigure_packages = extract_setup_configuration_command_literals(&text, 6);
    let parameter_rows = extract_setup_configuration_parameter_rows(&text, PARAMETER_ROW_LIMIT);
    let parameters = filter_setup_configuration_parameters(
        extract_config_assignment_literals(&text, PARAMETER_LITERAL_LIMIT)
            .into_iter()
            .chain(
                parameter_rows
                    .iter()
                    .filter_map(SetupConfigurationParameterRow::positional_assignment),
            )
            .chain(extract_parameter_literals(&text, PARAMETER_LITERAL_LIMIT))
            .chain(parameter_rows.iter().map(|row| row.name.clone()))
            .collect(),
        &packages,
        &reconfigure_packages,
        &paths,
        &sections,
    );
    if packages.is_empty()
        && reconfigure_packages.is_empty()
        && paths.is_empty()
        && sections.is_empty()
        && parameters.is_empty()
    {
        return None;
    }
    let focus_score = setup_configuration_focus_score(focus_tokens, chunk, &text);
    let label_focus_score = setup_configuration_label_focus_score(subject_label_sequences, chunk);
    if !focus_tokens.is_empty() && focus_score == 0 && label_focus_score == 0 {
        return None;
    }
    let score = setup_configuration_variant_score(
        SetupConfigurationLiterals {
            packages: &packages,
            reconfigure_packages: &reconfigure_packages,
            paths: &paths,
            sections: &sections,
            parameters: &parameters,
            parameter_rows: &parameter_rows,
        },
        chunk,
    );
    let parameter_only = packages.is_empty()
        && reconfigure_packages.is_empty()
        && paths.is_empty()
        && sections.is_empty();
    (score >= 16 || parameter_only).then_some(SetupConfigurationVariantContribution {
        score,
        focus_score,
        label_focus_score,
        packages,
        reconfigure_packages,
        paths,
        sections,
        parameters,
        parameter_rows,
    })
}

fn merge_setup_configuration_variant(
    variants: &mut BTreeMap<Uuid, SetupConfigurationVariant>,
    chunk: &RuntimeMatchedChunk,
    contribution: SetupConfigurationVariantContribution,
    parameter_limit: usize,
    parameter_row_limit: usize,
) {
    let entry = variants.entry(chunk.document_id).or_insert_with(|| SetupConfigurationVariant {
        source: chunk.document_label.clone(),
        score: 0,
        focus_score: 0,
        label_focus_score: 0,
        packages: Vec::new(),
        reconfigure_packages: Vec::new(),
        paths: Vec::new(),
        sections: Vec::new(),
        parameters: Vec::new(),
        parameter_rows: Vec::new(),
    });
    entry.score = entry.score.saturating_add(contribution.score);
    entry.focus_score = entry.focus_score.saturating_add(contribution.focus_score);
    entry.label_focus_score = entry.label_focus_score.max(contribution.label_focus_score);
    push_unique_values(&mut entry.packages, contribution.packages, 4);
    push_unique_values(&mut entry.reconfigure_packages, contribution.reconfigure_packages, 4);
    push_unique_values(&mut entry.paths, contribution.paths, 4);
    push_unique_values(&mut entry.sections, contribution.sections, 4);
    push_unique_values(&mut entry.parameters, contribution.parameters, parameter_limit);
    push_unique_parameter_rows(
        &mut entry.parameter_rows,
        contribution.parameter_rows,
        parameter_row_limit,
    );
}

fn setup_configuration_variant_has_anchor(variant: &SetupConfigurationVariant) -> bool {
    !variant.packages.is_empty()
        || !variant.reconfigure_packages.is_empty()
        || !variant.paths.is_empty()
        || (!variant.sections.is_empty() && !variant.parameters.is_empty())
        || (!variant.sections.is_empty() && !variant.parameter_rows.is_empty())
}

fn setup_configuration_has_distinct_variants(variants: &[SetupConfigurationVariant]) -> bool {
    if variants.len() <= 1 {
        return false;
    }
    let anchor_sets =
        variants.iter().map(setup_configuration_variant_anchor_keys).collect::<Vec<_>>();
    anchor_sets.iter().enumerate().any(|(left_index, left)| {
        anchor_sets.iter().skip(left_index + 1).any(|right| {
            left.iter().any(|anchor| !right.contains(anchor))
                && right.iter().any(|anchor| !left.contains(anchor))
        })
    })
}

fn setup_configuration_variant_anchor_keys(
    variant: &SetupConfigurationVariant,
) -> BTreeSet<String> {
    let identity_anchors = variant
        .packages
        .iter()
        .chain(&variant.reconfigure_packages)
        .chain(&variant.sections)
        .map(|value| setup_configuration_literal_key(value))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    if identity_anchors.is_empty() {
        return variant
            .paths
            .iter()
            .map(|value| setup_configuration_literal_key(value))
            .filter(|value| !value.is_empty())
            .collect();
    }
    identity_anchors
}

fn diversify_setup_configuration_variants(
    variants: Vec<SetupConfigurationVariant>,
    query_terms: &BTreeSet<String>,
) -> Vec<SetupConfigurationVariant> {
    if variants.len() <= 2 {
        return variants;
    }
    let family_model = setup_configuration_variant_family_model(&variants, query_terms);
    let mut selected = Vec::with_capacity(variants.len());
    let mut selected_families = BTreeSet::<String>::new();
    for variant in &variants {
        let family = setup_configuration_variant_family(variant, query_terms, &family_model);
        if selected_families.insert(family) {
            selected.push(variant.clone());
        }
    }
    for variant in variants {
        if selected.iter().any(|selected| selected.source == variant.source) {
            continue;
        }
        selected.push(variant);
    }
    selected
}

#[derive(Default)]
struct SetupConfigurationVariantFamilyModel {
    variant_count: usize,
    token_frequency: BTreeMap<String, usize>,
}

fn setup_configuration_variant_family_model(
    variants: &[SetupConfigurationVariant],
    query_terms: &BTreeSet<String>,
) -> SetupConfigurationVariantFamilyModel {
    let mut token_frequency = BTreeMap::<String, usize>::new();
    let mut variant_count = 0usize;
    for variant in variants {
        let tokens = setup_configuration_variant_family_tokens(&variant.source, query_terms);
        if tokens.is_empty() {
            continue;
        }
        variant_count = variant_count.saturating_add(1);
        for token in tokens {
            *token_frequency.entry(token).or_default() += 1;
        }
    }
    SetupConfigurationVariantFamilyModel { variant_count, token_frequency }
}

fn setup_configuration_variant_family(
    variant: &SetupConfigurationVariant,
    query_terms: &BTreeSet<String>,
    family_model: &SetupConfigurationVariantFamilyModel,
) -> String {
    let tokens = setup_configuration_variant_family_tokens(&variant.source, query_terms);
    if let Some(token) = tokens.iter().find(|token| {
        family_model
            .token_frequency
            .get(*token)
            .is_some_and(|frequency| *frequency < family_model.variant_count)
    }) {
        return token.clone();
    }
    if !tokens.is_empty() {
        return tokens.join(" ");
    }
    variant.source.to_lowercase()
}

fn setup_configuration_variant_family_tokens(
    source: &str,
    query_terms: &BTreeSet<String>,
) -> Vec<String> {
    normalized_alnum_tokens(source, 3)
        .into_iter()
        .filter(|token| !query_terms.contains(token))
        .collect()
}

fn setup_configuration_focus_tokens(question: &str, query_ir: &QueryIR) -> BTreeSet<String> {
    let current_question =
        crate::services::query::effective_query::current_question_segment(question);
    let mut tokens = normalized_alnum_tokens(current_question, 3);
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    tokens
}

fn setup_configuration_subject_tokens(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    tokens
}

fn setup_configuration_subject_label_sequences(query_ir: &QueryIR) -> Vec<Vec<String>> {
    let mut sequences = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_setup_configuration_subject_label_sequence(
            &mut sequences,
            &mut seen,
            &document_focus.hint,
        );
    }
    for entity in &query_ir.target_entities {
        push_setup_configuration_subject_label_sequence(&mut sequences, &mut seen, &entity.label);
    }
    sequences
}

fn push_setup_configuration_subject_label_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    label: &str,
) {
    let sequence = normalized_alnum_token_sequence(label, 2);
    if !sequence.is_empty() && seen.insert(sequence.clone()) {
        sequences.push(sequence);
    }
}

fn setup_configuration_focus_score(
    focus_tokens: &BTreeSet<String>,
    chunk: &RuntimeMatchedChunk,
    text: &str,
) -> usize {
    if focus_tokens.is_empty() {
        return 0;
    }
    let mut available = normalized_alnum_tokens(&chunk.document_label, 3);
    available.extend(normalized_alnum_tokens(text, 3));
    focus_tokens.intersection(&available).count()
}
