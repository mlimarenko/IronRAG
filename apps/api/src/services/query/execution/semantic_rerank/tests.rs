use super::provider_call::{ProviderCallReservationTask, wait_for_provider_call_reservation};
use super::scoring::{ProviderSemanticScore, parse_semantic_scores, validate_semantic_scores};
use super::*;

fn candidate(id: &str, text: &str) -> RerankCandidate {
    RerankCandidate { id: id.to_string(), text: text.to_string(), score: Some(0.5) }
}

#[test]
fn semantic_rerank_resolves_only_the_query_rerank_purpose() {
    assert_eq!(SEMANTIC_RERANK_BINDING_PURPOSE, AiBindingPurpose::QueryRerank);
    assert_ne!(
        SEMANTIC_RERANK_BINDING_PURPOSE,
        AiBindingPurpose::QueryCompile,
        "semantic rerank must never fall back to the compiler binding",
    );
}

#[test]
fn policy_enforces_hard_timeout_and_prompt_budgets() {
    let policy = SemanticRerankPolicy::bounded(60_000, 500, 50_000, 500_000);

    assert_eq!(policy.timeout.as_millis(), 5_000);
    assert_eq!(policy.turn_wait.as_millis(), u128::from(SEMANTIC_RERANK_TURN_WAIT_MS));
    assert_eq!(policy.candidate_limit, 32);
    assert_eq!(policy.candidate_text_chars, 2_400);
    assert_eq!(policy.total_text_chars, 32_000);
}

#[test]
fn policy_clamps_zero_values_to_nonzero_budgets() {
    let policy = SemanticRerankPolicy::bounded(0, 0, 0, 0);

    assert_eq!(policy.timeout.as_millis(), 1);
    assert_eq!(policy.turn_wait.as_millis(), u128::from(SEMANTIC_RERANK_TURN_WAIT_MS));
    assert_eq!(policy.candidate_limit, 1);
    assert_eq!(policy.candidate_text_chars, 1);
    assert_eq!(policy.total_text_chars, 1);
}

#[test]
fn provider_budget_larger_than_the_turn_wait_is_not_attempted() {
    let slow = SemanticRerankPolicy::bounded(5_000, 16, 1_200, 18_000);
    assert_eq!(slow.turn_wait.as_millis(), u128::from(SEMANTIC_RERANK_TURN_WAIT_MS));
    assert!(
        !slow.provider_fits_turn_wait(),
        "a provider allowed to run past the turn wait must not be started"
    );

    let fitting = SemanticRerankPolicy::bounded(SEMANTIC_RERANK_TURN_WAIT_MS, 16, 1_200, 18_000);
    assert!(fitting.provider_fits_turn_wait());
}

#[tokio::test]
async fn slow_semantic_rerank_does_not_hold_the_turn_past_the_wait_budget() {
    let turn_wait = Duration::from_millis(50);
    let stage = async {
        tokio::time::sleep(Duration::from_millis(400)).await;
        SemanticRerankAttempt::Failed {
            failure: SemanticRerankFailure::ProviderFailure,
            prepared_candidate_count: 1,
        }
    };

    let started = Instant::now();
    let attempt = await_semantic_rerank_within_turn_wait(turn_wait, stage).await;

    assert!(attempt.is_none(), "a provider slower than the turn wait must not win the race");
    assert!(
        started.elapsed() < Duration::from_millis(300),
        "the turn must not wait for the slow provider tail"
    );
}

#[test]
fn semantic_rerank_degradation_reasons_are_counted_and_named() {
    let before = semantic_rerank_degradation_count();
    record_semantic_rerank_degradation(SemanticRerankDegradation::ProviderBudgetExceedsTurnWait);
    record_semantic_rerank_degradation(SemanticRerankDegradation::TurnWaitExceeded);

    assert_eq!(semantic_rerank_degradation_count(), before + 2);
    assert_eq!(
        SemanticRerankDegradation::ProviderBudgetExceedsTurnWait.reason(),
        "provider_budget_exceeds_turn_wait"
    );
    assert_eq!(SemanticRerankDegradation::TurnWaitExceeded.reason(), "turn_wait_exceeded");
}

#[test]
fn degradation_reason_names_every_stage_failure() {
    let missing = SemanticRerankDegradation::from_failure(SemanticRerankFailure::MissingBinding);
    assert_eq!(missing.reason(), "missing_binding");
    assert_eq!(
        SemanticRerankDegradation::from_failure(SemanticRerankFailure::ProviderFailure).reason(),
        "provider_failure"
    );
    assert_eq!(
        SemanticRerankDegradation::from_failure(SemanticRerankFailure::InvalidResponse).reason(),
        "invalid_response"
    );
    assert_eq!(
        SemanticRerankDegradation::from_failure(SemanticRerankFailure::TimedOut).reason(),
        "timed_out"
    );
}

#[test]
fn slow_binding_consumes_the_provider_decision_budget() {
    let started = Instant::now();
    let deadline =
        SemanticRerankDecisionDeadline::starting_at(started, Duration::from_millis(1_000));

    assert_eq!(
        deadline.remaining_at(started + Duration::from_millis(650)),
        Some(Duration::from_millis(350))
    );
}

#[tokio::test]
async fn reservation_deadline_does_not_cancel_the_reconcilable_insert_task() {
    let provider_call_id = Uuid::now_v7();
    let (release, blocked) = tokio::sync::oneshot::channel::<()>();
    let mut task: ProviderCallReservationTask = tokio::spawn(async move {
        let _ = blocked.await;
        Ok(provider_call_id)
    });

    let before_deadline =
        wait_for_provider_call_reservation(&mut task, Duration::from_millis(1)).await;
    assert!(before_deadline.is_none());

    assert!(release.send(()).is_ok());
    let completed = task.await.expect("reservation task should remain joinable");
    assert_eq!(completed.expect("synthetic reservation should succeed"), provider_call_id);
}

#[test]
fn pre_call_deadline_check_rejects_an_exhausted_budget() {
    let started = Instant::now();
    let deadline =
        SemanticRerankDecisionDeadline::starting_at(started, Duration::from_millis(1_000));

    assert_eq!(
        deadline.remaining_at(started + Duration::from_millis(999)),
        Some(Duration::from_millis(1))
    );
    assert_eq!(deadline.remaining_at(started + Duration::from_millis(1_000)), None);
    assert_eq!(deadline.remaining_at(started + Duration::from_millis(1_500)), None);
}

#[test]
fn request_rejects_empty_queries_and_reserves_candidate_budget() {
    let chunks = vec![candidate("chunk-a", "candidate evidence")];

    assert!(
        prepare_semantic_rerank_request(
            "   ",
            &[],
            &[],
            &chunks,
            SemanticRerankPolicy::bounded(500, 1, 10, 20),
        )
        .is_none()
    );

    let prepared = prepare_semantic_rerank_request(
        &"query".repeat(100),
        &[],
        &[],
        &chunks,
        SemanticRerankPolicy::bounded(500, 1, 10, 20),
    )
    .expect("the query budget should leave room for candidate evidence");
    assert_eq!(prepared.prepared_candidate_count(), 1);
    assert!(prepared.provider_text_chars() <= 20);
}

#[test]
fn prepared_request_exposes_only_opaque_indices_and_bounded_text() {
    let entities = vec![candidate("entity-id-must-not-leak", &"e".repeat(200))];
    let relationships = vec![candidate("relation-id-must-not-leak", &"r".repeat(200))];
    let chunks = vec![candidate("chunk-id-must-not-leak", &"c".repeat(200))];
    let policy = SemanticRerankPolicy::bounded(500, 3, 24, 96);

    let prepared = prepare_semantic_rerank_request(
        "rank these candidates",
        &entities,
        &relationships,
        &chunks,
        policy,
    )
    .expect("bounded candidates should produce a request");
    let payload = prepared.provider_payload_json().to_string();

    assert_eq!(prepared.prepared_candidate_count(), 3);
    assert!(prepared.provider_text_chars() <= 96);
    assert!(!payload.contains("entity-id-must-not-leak"));
    assert!(!payload.contains("relation-id-must-not-leak"));
    assert!(!payload.contains("chunk-id-must-not-leak"));
    assert!(payload.contains("\"index\":0"));
}

#[test]
fn prepared_request_caps_the_json_encoded_user_message() {
    let chunks = (0..SEMANTIC_RERANK_HARD_MAX_CANDIDATES)
        .map(|index| candidate(&format!("chunk-{index}"), &"\0\"\\".repeat(800)))
        .collect::<Vec<_>>();

    let prepared = prepare_semantic_rerank_request(
        "rank encoded evidence",
        &[],
        &[],
        &chunks,
        SemanticRerankPolicy::bounded(
            SEMANTIC_RERANK_HARD_MAX_TIMEOUT_MS,
            SEMANTIC_RERANK_HARD_MAX_CANDIDATES,
            SEMANTIC_RERANK_HARD_MAX_CANDIDATE_TEXT_CHARS,
            SEMANTIC_RERANK_HARD_MAX_TOTAL_TEXT_CHARS,
        ),
    )
    .expect("encoded payload should retain a bounded candidate prefix");

    assert!(prepared.user_message_bytes() <= HARD_MAX_USER_MESSAGE_BYTES);
    assert!(prepared.prepared_candidate_count() < SEMANTIC_RERANK_HARD_MAX_CANDIDATES);
}

#[test]
fn duplicate_internal_ids_disable_provider_rerank() {
    let chunks = vec![candidate("duplicate", "first"), candidate("duplicate", "second")];

    assert!(
        prepare_semantic_rerank_request(
            "rank evidence",
            &[],
            &[],
            &chunks,
            SemanticRerankPolicy::bounded(500, 2, 100, 500),
        )
        .is_none()
    );
}

#[test]
fn shadow_task_budget_allows_only_one_concurrent_call() {
    let first = try_acquire_shadow_task_permit().expect("first shadow task should fit");
    assert!(try_acquire_shadow_task_permit().is_none());
    drop(first);
    assert!(try_acquire_shadow_task_permit().is_some());
}

#[test]
fn openai_strict_schema_uses_only_supported_constraint_keywords() {
    let response_format = semantic_rerank_response_format();
    let schema = &response_format["json_schema"]["schema"];
    let serialized = schema.to_string();

    assert!(!serialized.contains("\"minItems\""));
    assert!(!serialized.contains("\"maxItems\""));
    assert!(!serialized.contains("\"minimum\""));
    assert!(!serialized.contains("\"maximum\""));
}

#[test]
fn score_validation_requires_one_finite_score_per_submitted_index() {
    let ranking = validate_semantic_scores(
        vec![
            ProviderSemanticScore { index: 0, score: 0.4 },
            ProviderSemanticScore { index: 1, score: 0.9 },
            ProviderSemanticScore { index: 2, score: 0.1 },
        ],
        3,
    )
    .expect("complete score set should validate");

    assert_eq!(ranking.ordered_indices(), &[1, 0, 2]);
}

#[test]
fn structured_response_parser_rejects_unknown_fields_and_non_index_identifiers() {
    let valid = r#"{"scores":[{"index":0,"score":0.75}]}"#;
    let unknown = r#"{"scores":[{"index":0,"score":0.75}],"candidateId":"secret"}"#;
    let identifier = r#"{"scores":[{"index":0,"score":0.75,"id":"secret"}]}"#;

    assert_eq!(
        parse_semantic_scores(valid, 1).expect("expected response should parse").ordered_indices(),
        &[0]
    );
    assert!(parse_semantic_scores(unknown, 1).is_err());
    assert!(parse_semantic_scores(identifier, 1).is_err());
}

#[test]
fn score_validation_rejects_duplicate_out_of_range_missing_and_non_finite_scores() {
    let duplicate = vec![
        ProviderSemanticScore { index: 0, score: 0.4 },
        ProviderSemanticScore { index: 0, score: 0.3 },
    ];
    let out_of_range = vec![ProviderSemanticScore { index: 2, score: 0.4 }];
    let missing = vec![ProviderSemanticScore { index: 0, score: 0.4 }];
    let non_finite = vec![ProviderSemanticScore { index: 0, score: f64::NAN }];

    assert!(validate_semantic_scores(duplicate, 2).is_err());
    assert!(validate_semantic_scores(out_of_range, 1).is_err());
    assert!(validate_semantic_scores(missing, 2).is_err());
    assert!(validate_semantic_scores(non_finite, 1).is_err());
}

#[test]
fn provider_order_maps_indices_back_to_internal_ids_and_keeps_unsubmitted_tail() {
    let chunks = vec![
        candidate("chunk-a", "first"),
        candidate("chunk-b", "second"),
        candidate("chunk-c", "third"),
    ];
    let prepared = prepare_semantic_rerank_request(
        "second first",
        &[],
        &[],
        &chunks,
        SemanticRerankPolicy::bounded(500, 2, 100, 500),
    )
    .expect("chunks should produce a request");
    let ranking = validate_semantic_scores(
        vec![
            ProviderSemanticScore { index: 0, score: 0.1 },
            ProviderSemanticScore { index: 1, score: 0.9 },
        ],
        2,
    )
    .expect("scores should validate");

    let order = prepared.map_ranking_to_candidate_order(&ranking);

    assert_eq!(order.chunks, vec!["chunk-b", "chunk-a", "chunk-c"]);
    assert_eq!(
        order.provider_ranked_chunk_ids(),
        &["chunk-b".to_string(), "chunk-a".to_string()],
        "the explicit rank map must exclude the unsubmitted fallback tail",
    );
}
