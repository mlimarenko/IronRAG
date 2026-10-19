use super::*;

#[test]
fn incomplete_grounded_follow_up_state_stays_required_after_repeated_incomplete_result() {
    assert!(next_incomplete_grounded_answer_follow_up_required(false, true, false));
    assert!(next_incomplete_grounded_answer_follow_up_required(true, true, true));
    assert!(!next_incomplete_grounded_answer_follow_up_required(true, false, true));
    assert!(next_incomplete_grounded_answer_follow_up_required(true, false, false));
}

#[test]
fn incomplete_grounded_follow_up_requires_grounding_evidence() {
    assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
        "list_documents",
        false,
        false,
        Some("document names only"),
    ));
    assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
        SEARCH_DOCUMENTS_TOOL_NAME,
        false,
        false,
        Some("   "),
    ));
    assert!(tool_result_satisfies_incomplete_grounded_follow_up(
        SEARCH_DOCUMENTS_TOOL_NAME,
        false,
        false,
        Some("content evidence"),
    ));
    assert!(tool_result_satisfies_incomplete_grounded_follow_up(
        GROUNDED_ANSWER_TOOL_NAME,
        false,
        true,
        None,
    ));
    assert!(!tool_result_satisfies_incomplete_grounded_follow_up(
        GROUNDED_ANSWER_TOOL_NAME,
        false,
        false,
        Some("clarification without final evidence"),
    ));
}

#[test]
fn final_answer_never_splices_raw_user_fragments_into_grounded_text() {
    let answer = "The verified evidence does not support that claim.".to_string();

    let finalized = finalize_agent_loop_answer(
        answer.clone(),
        "Explain «UNTRUSTED-USER-FRAGMENT».",
        None,
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, answer);
    assert!(!finalized.contains("UNTRUSTED-USER-FRAGMENT"));
}

#[test]
fn final_answer_keeps_parent_answer_when_verified_literals_match() {
    let grounded = "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `secretKey`.";
    let answer =
        "Use `/etc/alpha.ini` for `alphaPackage`; set `partnerId` and `secretKey`.".to_string();

    let finalized = finalize_agent_loop_answer(
        answer.clone(),
        "How do I configure it?",
        Some(grounded),
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, answer);
}

#[test]
fn final_answer_keeps_a_narrative_rewording_with_the_same_formal_literals() {
    let grounded = "The `mode` field documents the `stable` value.";
    let answer = "The documented value for `mode` is `stable`.".to_string();

    let finalized = finalize_agent_loop_answer(
        answer.clone(),
        "Describe the mode field.",
        Some(grounded),
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, answer);
}

#[test]
fn final_answer_keeps_user_literal_that_is_not_in_verified_grounding() {
    let grounded = "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `secretKey`.";
    let answer =
        "For `USER-42`, configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `secretKey`."
            .to_string();

    let finalized = finalize_agent_loop_answer(
        answer.clone(),
        "Apply this to `USER-42`.",
        Some(grounded),
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, answer);
}

#[test]
fn final_answer_prefers_verified_grounding_when_parent_drops_or_adds_literals() {
    let grounded =
        "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, `secretKey`, and `sendDetails`.";
    let answer =
        "Configure `alphaPackage`, `/etc/alpha.ini`, `partnerId`, and `madeUpFlag`.".to_string();

    let finalized = finalize_agent_loop_answer(
        answer,
        "How do I configure it?",
        Some(grounded),
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, grounded);
}

#[test]
fn final_answer_prefers_verified_grounding_when_parent_drops_plain_surface_anchors() {
    let grounded = "Use alphaPackage with /etc/alpha.ini. Set partnerId, secretKey, sendDetails, callbackUrl, and retryCount from the sourced parameter table.";
    let answer = "Use alphaPackage with /etc/alpha.ini, partnerId, and secretKey.".to_string();

    let finalized = finalize_agent_loop_answer(
        answer,
        "How do I configure it?",
        Some(grounded),
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, grounded);
}

#[test]
fn final_answer_keeps_parent_answer_when_plain_surface_anchor_loss_is_small() {
    let grounded = "Use alphaPackage with /etc/alpha.ini. Set partnerId, secretKey, sendDetails, callbackUrl, and retryCount from the sourced parameter table.";
    let answer = "Use alphaPackage with /etc/alpha.ini. Set partnerId, secretKey, sendDetails, and callbackUrl."
            .to_string();

    let finalized = finalize_agent_loop_answer(
        answer.clone(),
        "How do I configure it?",
        Some(grounded),
        "req",
        Uuid::nil(),
    );

    assert_eq!(finalized, answer);
}

#[test]
fn verified_grounded_literal_set_extracts_config_block_without_fence_language() {
    let literals = verified_grounded_answer_literal_set(
        "```ini\n[Main]\nalphaPackage = true\n/etc/alpha.ini\n```",
    );

    assert!(literals.contains("[Main]"));
    assert!(literals.contains("alphaPackage"));
    assert!(literals.contains("/etc/alpha.ini"));
    assert!(!literals.contains("ini"));
}

#[test]
fn grounded_answer_passthrough_question_normalizes_line_endings_and_outer_whitespace() {
    let normalized_question =
        grounded_answer_passthrough_question("\r\n  How do I configure this?\r\n  ");

    assert_eq!(normalized_question, "How do I configure this?");
    assert_eq!(
        grounded_answer_passthrough_question("How do I configure this?"),
        normalized_question,
    );
}

#[test]
fn verified_grounded_answer_guard_uses_guard_only_after_multiple_answers() {
    assert_eq!(
        verified_grounded_answer_guard_candidate(Some("first"), Some("guard"), 1, 1),
        Some("first"),
    );
    assert_eq!(
        verified_grounded_answer_guard_candidate(Some("first"), Some("guard"), 2, 3),
        Some("guard"),
    );
    assert_eq!(verified_grounded_answer_guard_candidate(None, Some("guard"), 1, 1), None,);
}
