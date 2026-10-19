use super::*;

// --- L2: exact-label-term profile for short identifier-shaped mentions ---
// IR helper with no document_focus so the entity label drives profile
// building exclusively (document_focus would produce its own profile first,
// masking the short-mention branch under test).

fn short_mention_ir(label: &str) -> QueryIR {
    wildcard_inventory_ir(label)
}

#[test]
fn profile_is_built_for_two_char_uppercase_identifier_shaped_mention() {
    // A 2-char all-uppercase mention produces no alnum tokens under min-3
    // and no wildcard prefixes; the profile builder must create an
    // exact-label-term profile instead of bailing.
    let ir = short_mention_ir("AS");
    let graph_index = graph_index_with_nodes(vec![node("Alpha Suite", "process", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty(), "profile must be built for identifier-shaped 2-char mention");
    // All profiles must be of the exact-label-term kind for a pure short mention IR.
    assert!(
        profiles.iter().any(|p| !p.exact_label_terms.is_empty()),
        "at least one profile must carry exact_label_terms for the short mention"
    );
    let short_profile = profiles.iter().find(|p| !p.exact_label_terms.is_empty()).unwrap();
    assert!(
        short_profile.target_label_tokens.is_empty(),
        "target_label_tokens must be empty for short-mention profile"
    );
    assert!(
        short_profile.wildcard_prefixes.is_empty(),
        "wildcard_prefixes must be empty for short-mention profile"
    );
}

#[test]
fn profile_is_not_built_for_lowercase_two_char_prose_word() {
    // A lowercase 2-char token is not identifier-shaped; the profile
    // builder must bail rather than emit an exact-label-term profile.
    let ir = short_mention_ir("as");
    let graph_index = graph_index_with_nodes(vec![node("Alpha Suite", "process", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    // "as" is all-lowercase → not identifier-shaped → no profile produced.
    assert!(profiles.is_empty(), "no profile must be built for lowercase short prose word");
}

#[test]
fn exact_label_term_scorer_matches_node_label() {
    // When the profile carries an exact-label term, a node whose label
    // (case-insensitive) equals that term must receive a positive score.
    let ir = short_mention_ir("AS");
    let graph_index = graph_index_with_nodes(vec![node("AS", "artifact", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "AS",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );
    assert!(score > 0, "exact-label-term scorer must match label field");
}

#[test]
fn exact_label_term_scorer_matches_alias_field() {
    // When a node's alias entry equals the exact-label term the scorer
    // must match it — this is the consumption path for L4/L5 aliases.
    let ir = short_mention_ir("AS");
    let graph_index = graph_index_with_nodes(vec![node("Alpha Suite", "process", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let score = graph_target_entity_coverage_score(
        &[
            GraphTargetEntityCoverageField {
                text: "Alpha Suite",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            },
            GraphTargetEntityCoverageField {
                text: "AS",
                kind: GraphTargetEntityCoverageFieldKind::Alias,
            },
        ],
        &profiles,
    );
    assert!(score > 0, "exact-label-term scorer must match alias field");
}

#[test]
fn token_profile_matches_short_node_via_full_form_alias() {
    // A multi-word subject produces a *token* profile [alpha, suite]
    // (no wildcard, no exact-label term). A node whose label is the short
    // acronym "AS" must cover that profile ONLY through the corpus-gloss
    // full-form alias "Alpha Suite" — the consumption path the reverse
    // backfill feeds.
    let ir = describe_ir("Alpha Suite");
    let graph_index = graph_index_with_nodes(vec![node("AS", "artifact", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let token_profile = profiles
        .iter()
        .find(|p| p.target_label_tokens == ["alpha", "suite"])
        .expect("multi-word subject must build a [alpha, suite] token profile");
    assert!(
        token_profile.wildcard_prefixes.is_empty() && token_profile.exact_label_terms.is_empty(),
        "the [alpha, suite] profile must be a pure token profile"
    );

    // Bare short label "AS" does not cover the full-form token profile.
    let score_label_only = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "AS",
            kind: GraphTargetEntityCoverageFieldKind::Label,
        }],
        &profiles,
    );
    assert_eq!(score_label_only, 0, "bare short label must not cover the full-form token profile");

    // The full-form alias lets the short node match via the token path.
    let score_with_alias = graph_target_entity_coverage_score(
        &[
            GraphTargetEntityCoverageField {
                text: "AS",
                kind: GraphTargetEntityCoverageFieldKind::Label,
            },
            GraphTargetEntityCoverageField {
                text: "Alpha Suite",
                kind: GraphTargetEntityCoverageFieldKind::Alias,
            },
        ],
        &profiles,
    );
    assert!(
        score_with_alias > 0,
        "full-form alias must let the short node match the [alpha, suite] token profile"
    );
}

#[test]
fn exact_label_term_scorer_does_not_match_summary_or_evidence() {
    // The exact-label-term scorer is restricted to label and alias fields;
    // it must not fire on summary or evidence text.
    let ir = short_mention_ir("AS");
    let graph_index = graph_index_with_nodes(vec![node("Alpha Suite", "process", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let score_summary = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "AS",
            kind: GraphTargetEntityCoverageFieldKind::Summary,
        }],
        &profiles,
    );
    let score_evidence = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "AS",
            kind: GraphTargetEntityCoverageFieldKind::Evidence,
        }],
        &profiles,
    );
    assert_eq!(score_summary, 0, "summary field must not match exact-label-term");
    assert_eq!(score_evidence, 0, "evidence field must not match exact-label-term");
}

#[test]
fn exact_label_term_scorer_is_case_insensitive() {
    // The exact-label-term comparison is case-insensitive on both sides.
    let ir = short_mention_ir("AS");
    let graph_index = graph_index_with_nodes(vec![node("Alpha Suite", "process", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    // Field text lowercased should still match.
    let score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "as",
            kind: GraphTargetEntityCoverageFieldKind::Alias,
        }],
        &profiles,
    );
    assert!(score > 0, "exact-label-term match must be case-insensitive");
}

#[test]
fn exact_label_term_multibyte_mention_is_supported() {
    // Synthetic non-ASCII uppercase short mention — exercised through
    // Unicode predicates only (no script-specific logic).
    let ir = short_mention_ir("ΩΣ");
    let graph_index = graph_index_with_nodes(vec![node("Omega Sigma System", "process", None)]);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(
        !profiles.is_empty(),
        "profile must be built for 2-char multibyte identifier-shaped mention"
    );
    let score = graph_target_entity_coverage_score(
        &[GraphTargetEntityCoverageField {
            text: "ΩΣ",
            kind: GraphTargetEntityCoverageFieldKind::Alias,
        }],
        &profiles,
    );
    assert!(score > 0, "multibyte alias must match exact-label-term");
}

// ── Sense-dominance filter tests ──────────────────────────────────────────

fn node_with_support(
    label: &str,
    node_type: &str,
    alias: Option<&str>,
    support: i32,
) -> RuntimeGraphQueryNodeRow {
    let mut n = node(label, node_type, None);
    n.support_count = support;
    if let Some(a) = alias {
        n.aliases_json = serde_json::json!([a]);
    }
    n
}

#[test]
fn sense_dominance_filter_keeps_dominant_family_drops_noise() {
    // Six nodes all carry the short identifier "AS" as their label.
    // Supports: 1000, 400, 50, 3, 2, 1.
    // Threshold = floor(1000 / 150) = 6; nodes with support >= 6 survive.
    // Expected survivors: 1000, 400, 50. Noise (3, 2, 1) is dropped.
    let ir = short_mention_ir("AS");
    let nodes = vec![
        node_with_support("AS", "artifact", None, 1000),
        node_with_support("AS", "artifact", None, 400),
        node_with_support("AS", "artifact", None, 50),
        node_with_support("AS", "artifact", None, 3),
        node_with_support("AS", "artifact", None, 2),
        node_with_support("AS", "artifact", None, 1),
    ];
    let graph_index = graph_index_with_nodes(nodes);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let plan = build_query_plan("update AS", None, Some(8), None);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);
    // Survivors: support 1000, 400, 50 (threshold = floor(1000/150) = 6).
    assert_eq!(
        hits.len(),
        3,
        "dominance filter must keep only nodes above threshold; got {} hits",
        hits.len()
    );
}

#[test]
fn sense_dominance_filter_keeps_all_when_single_match() {
    // When only one node matches the exact-label term, no filtering occurs.
    let ir = short_mention_ir("AS");
    let nodes = vec![node_with_support("AS", "artifact", None, 5)];
    let graph_index = graph_index_with_nodes(nodes);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let plan = build_query_plan("update AS", None, Some(8), None);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);
    assert_eq!(hits.len(), 1, "single exact-match node must survive without filtering");
}

#[test]
fn sense_dominance_filter_alias_matched_node_participates() {
    // A node that carries the identifier as an alias (not label) must also
    // be subject to the dominance filter.
    let ir = short_mention_ir("AS");
    // dominant: label="Alpha Suite", alias="AS", support=1000
    // noise:    label="Aux Server",  alias="AS", support=2
    let nodes = vec![
        node_with_support("Alpha Suite", "artifact", Some("AS"), 1000),
        node_with_support("Aux Server", "artifact", Some("AS"), 2),
    ];
    let graph_index = graph_index_with_nodes(nodes);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    assert!(!profiles.is_empty());
    let plan = build_query_plan("update AS", None, Some(8), None);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);
    // threshold = floor(1000/150) = 6; support=2 is below → only dominant survives.
    assert_eq!(hits.len(), 1, "alias-matched noise node must be dropped by dominance filter");
    assert_eq!(hits[0].label, "Alpha Suite");
}

#[test]
fn sense_dominance_filter_does_not_affect_non_exact_label_profiles() {
    // A normal multi-char label ("Alpha Suite") uses token/wildcard path, not
    // exact_label_terms. The dominance filter must not touch those hits.
    let ir = wildcard_inventory_ir("Alpha Suite");
    let nodes = vec![
        node_with_support("Alpha Suite Primary", "artifact", None, 1000),
        node_with_support("Alpha Suite Backup", "artifact", None, 2),
    ];
    let graph_index = graph_index_with_nodes(nodes);
    let profiles = graph_target_entity_profiles(Some(&ir), &graph_index);
    // No profile should use exact_label_terms for a long label.
    let has_exact = profiles.iter().any(|p| !p.exact_label_terms.is_empty());
    assert!(!has_exact, "wildcard profile must not use exact_label_terms path");
    let plan = build_query_plan("update Alpha Suite", None, Some(8), None);
    let hits = lexical_entity_hits(&plan, Some(&ir), &profiles, &graph_index);
    // Both nodes must appear — dominance filter is inactive.
    assert_eq!(hits.len(), 2, "both nodes must survive when exact-label path is not active");
}
