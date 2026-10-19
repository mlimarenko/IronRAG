use std::collections::BTreeMap;

use super::{
    protocol::{
        SystemOneAnswer, SystemOneDecisionRequest, SystemOneDecisionResponse,
        SystemOneNoulCriteria, SystemOneQuestion, SystemOneUsage,
    },
    validation::{
        SystemOneRequestError, SystemOneResponseError, probability_sum_tolerance,
        score_expectation_tolerance, validate_request, validate_response,
    },
};

fn noul_question() -> SystemOneQuestion {
    SystemOneQuestion::Noul {
        instructions: "Decide whether the state is healthy.".to_string(),
        criteria: Some(SystemOneNoulCriteria {
            r#true: Some("Healthy.".to_string()),
            r#false: None,
        }),
    }
}

fn choice_question() -> SystemOneQuestion {
    let mut criteria = BTreeMap::new();
    criteria.insert("alpha".to_string(), Some("First.".to_string()));
    criteria.insert("beta".to_string(), None);
    SystemOneQuestion::Choice { instructions: "Pick one.".to_string(), criteria }
}

fn score_question() -> SystemOneQuestion {
    SystemOneQuestion::Score {
        instructions: "Grade it.".to_string(),
        criteria: vec!["low".to_string(), "mid".to_string(), "high".to_string()],
    }
}

fn all_kinds_request() -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    questions.insert("n".to_string(), noul_question());
    questions.insert("c".to_string(), choice_question());
    questions.insert("s".to_string(), score_question());
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

fn single_noul_request() -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    questions.insert("n".to_string(), noul_question());
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

fn usage() -> SystemOneUsage {
    SystemOneUsage { input_tokens: 12, output_tokens: 3 }
}

fn request_with_one_choice() -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    questions.insert("c".to_string(), choice_question());
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

fn request_with_one_score() -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    questions.insert("s".to_string(), score_question());
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

/// A `choice` request whose question declares exactly `options`, so a
/// probability distribution can be exercised beyond the two-option fixture.
fn request_with_choice_options(options: &[&str]) -> SystemOneDecisionRequest {
    let mut criteria = BTreeMap::new();
    for option in options {
        criteria.insert((*option).to_string(), None);
    }
    let mut questions = BTreeMap::new();
    questions.insert(
        "c".to_string(),
        SystemOneQuestion::Choice { instructions: "Pick one.".to_string(), criteria },
    );
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

fn noul_answer(value: f64) -> SystemOneAnswer {
    SystemOneAnswer::Noul { noul: value }
}

fn choice_answer(choice: &str, probabilities: &[(&str, f64)], confidence: f64) -> SystemOneAnswer {
    let probabilities = probabilities
        .iter()
        .map(|(option, probability)| ((*option).to_string(), *probability))
        .collect();
    SystemOneAnswer::Choice { choice: choice.to_string(), probabilities, confidence }
}

fn score_answer(
    score: f64,
    levels: &[(&str, &str)],
    probabilities: &[(&str, f64)],
    confidence: f64,
) -> SystemOneAnswer {
    let legend =
        levels.iter().map(|(level, text)| ((*level).to_string(), (*text).to_string())).collect();
    let probabilities = probabilities
        .iter()
        .map(|(level, probability)| ((*level).to_string(), *probability))
        .collect();
    SystemOneAnswer::Score { score, legend, probabilities, confidence }
}

fn single_answer_response(
    request: &SystemOneDecisionRequest,
    id: &str,
    answer: SystemOneAnswer,
) -> SystemOneDecisionResponse {
    assert_eq!(request.questions.len(), 1);
    let mut answers = BTreeMap::new();
    answers.insert(id.to_string(), answer);
    SystemOneDecisionResponse { model: "server-model".to_string(), answers, usage: usage() }
}

// ---------------------------------------------------------------------------
// Request validation
// ---------------------------------------------------------------------------

#[test]
fn accepts_minimal_request_all_question_kinds() {
    let request = all_kinds_request();
    assert_eq!(validate_request(&request), Ok(()));

    let wire = serde_json::to_value(&request).expect("request serialises");
    let question = &wire["questions"]["n"];
    assert_eq!(question["type"], "noul");
    assert_eq!(question["criteria"]["true"], "Healthy.");
    assert_eq!(question["criteria"].get("false"), None);
    assert_eq!(wire["questions"]["c"]["criteria"]["beta"], serde_json::Value::Null);
    assert_eq!(wire["questions"]["s"]["criteria"][0], "low");
    assert_eq!(wire["questions"]["s"]["criteria"][2], "high");
}

#[test]
fn rejects_empty_model() {
    let mut request = all_kinds_request();
    request.model = String::new();
    assert_eq!(validate_request(&request), Err(SystemOneRequestError::EmptyModel));
}

#[test]
fn rejects_empty_state() {
    let mut request = all_kinds_request();
    request.state = String::new();
    assert_eq!(validate_request(&request), Err(SystemOneRequestError::EmptyState));
}

#[test]
fn rejects_empty_questions() {
    let mut request = all_kinds_request();
    request.questions.clear();
    assert_eq!(validate_request(&request), Err(SystemOneRequestError::EmptyQuestions));
}

#[test]
fn rejects_too_many_questions() {
    let mut request = all_kinds_request();
    request.questions.clear();
    for index in 0..super::validation::MAX_QUESTIONS {
        request.questions.insert(format!("q{index}"), noul_question());
    }
    request.questions.insert("over".to_string(), noul_question());
    let expected = SystemOneRequestError::TooManyQuestions {
        count: super::validation::MAX_QUESTIONS + 1,
        max: super::validation::MAX_QUESTIONS,
    };
    assert_eq!(validate_request(&request), Err(expected));
}

#[test]
fn rejects_invalid_question_id() {
    let mut request = all_kinds_request();
    request.questions.clear();
    request.questions.insert(String::new(), noul_question());
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::QuestionIdInvalid { id: String::new() })
    );

    let too_long = "x".repeat(super::validation::MAX_QUESTION_ID_CHARS + 1);
    let mut request = all_kinds_request();
    request.questions.clear();
    request.questions.insert(too_long.clone(), noul_question());
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::QuestionIdInvalid { id: too_long })
    );
}

#[test]
fn rejects_choice_criteria_empty() {
    let mut request = request_with_one_choice();
    request.questions.insert(
        "c".to_string(),
        SystemOneQuestion::Choice { instructions: "Pick.".to_string(), criteria: BTreeMap::new() },
    );
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::ChoiceCriteriaEmpty { id: "c".to_string() })
    );
}

#[test]
fn rejects_choice_256_options() {
    let mut criteria = BTreeMap::new();
    for index in 0..256 {
        criteria.insert(format!("option{index}"), None);
    }
    let mut request = request_with_one_choice();
    request.questions.insert(
        "c".to_string(),
        SystemOneQuestion::Choice { instructions: "Pick.".to_string(), criteria },
    );
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::TooManyChoiceOptions { id: "c".to_string(), count: 256 })
    );
}

#[test]
fn rejects_score_1_level() {
    let mut request = request_with_one_score();
    request.questions.insert(
        "s".to_string(),
        SystemOneQuestion::Score {
            instructions: "Grade.".to_string(),
            criteria: vec!["low".to_string()],
        },
    );
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::ScoreCriteriaOutOfRange { id: "s".to_string(), count: 1 })
    );
}

#[test]
fn rejects_score_11_level() {
    let levels = (0..11).map(|index| format!("level{index}")).collect();
    let mut request = request_with_one_score();
    request.questions.insert(
        "s".to_string(),
        SystemOneQuestion::Score { instructions: "Grade.".to_string(), criteria: levels },
    );
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::ScoreCriteriaOutOfRange { id: "s".to_string(), count: 11 })
    );
}

#[test]
fn accepts_max_question_count() {
    // The question cap is a local literal 64, not a protocol limit. The accept
    // side must admit exactly that many, so it pairs with
    // `rejects_too_many_questions` to pin the boundary from both sides.
    let mut request = all_kinds_request();
    request.questions.clear();
    for index in 0..64 {
        request.questions.insert(format!("q{index}"), noul_question());
    }
    assert_eq!(request.questions.len(), 64);
    assert_eq!(validate_request(&request), Ok(()));
}

#[test]
fn accepts_max_question_id_length() {
    let id = "x".repeat(64);
    let mut request = all_kinds_request();
    request.questions.clear();
    request.questions.insert(id, noul_question());
    assert_eq!(validate_request(&request), Ok(()));
}

#[test]
fn validates_request_question_in_key_order_across_kinds() {
    let mut request = all_kinds_request();
    request.questions.clear();
    request.questions.insert(
        "a".to_string(),
        SystemOneQuestion::Choice { instructions: "Pick.".to_string(), criteria: BTreeMap::new() },
    );
    request.questions.insert(
        "b".to_string(),
        SystemOneQuestion::Score { instructions: "Grade.".to_string(), criteria: vec![] },
    );
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::ChoiceCriteriaEmpty { id: "a".to_string() })
    );
    request.questions.remove("a");
    assert_eq!(
        validate_request(&request),
        Err(SystemOneRequestError::ScoreCriteriaOutOfRange { id: "b".to_string(), count: 0 })
    );
}

#[test]
fn accepts_max_choice_options() {
    // The protocol documents a maximum of 255 options per Choice; the accept
    // side must admit exactly that many, pairing with
    // `rejects_choice_256_options` to pin the boundary from both sides.
    let mut criteria = BTreeMap::new();
    for index in 0..255 {
        criteria.insert(format!("option{index}"), None);
    }
    let mut request = request_with_one_choice();
    request.questions.insert(
        "c".to_string(),
        SystemOneQuestion::Choice { instructions: "Pick.".to_string(), criteria },
    );
    assert_eq!(validate_request(&request), Ok(()));
}

#[test]
fn accepts_min_score_levels() {
    // The protocol requires at least two Score levels; two must be accepted,
    // pairing with `rejects_score_1_level`.
    assert_eq!(validate_request(&request_with_score_levels(2)), Ok(()));
}

#[test]
fn accepts_max_score_levels() {
    // The protocol accepts up to ten Score levels; ten must be accepted,
    // pairing with `rejects_score_11_level`.
    assert_eq!(validate_request(&request_with_score_levels(10)), Ok(()));
}

#[test]
fn rejects_non_string_state_on_wire() {
    let body = serde_json::json!({
        "state": {"nested": true},
        "model": "caller-model",
        "questions": {"n": {"type": "noul", "instructions": "Decide."}},
    });
    assert!(serde_json::from_value::<SystemOneDecisionRequest>(body).is_err());
}

#[test]
fn rejects_unknown_question_field_on_wire() {
    let body = serde_json::json!({
        "state": "state text",
        "model": "caller-model",
        "questions": {"n": {"type": "noul", "instructions": "Decide.", "extra": 1}},
    });
    assert!(serde_json::from_value::<SystemOneDecisionRequest>(body).is_err());
}

#[test]
fn rejects_unknown_top_level_request_field_on_wire() {
    // `deny_unknown_fields` is honoured at both the top-level request struct and
    // the internally tagged question enum, so an unknown caller field fails to
    // decode at either level rather than being silently dropped.
    let body = serde_json::json!({
        "state": "state text",
        "model": "caller-model",
        "questions": {"n": {"type": "noul", "instructions": "Decide."}},
        "unexpected": true,
    });
    assert!(serde_json::from_value::<SystemOneDecisionRequest>(body).is_err());
}

// ---------------------------------------------------------------------------
// Response validation
// ---------------------------------------------------------------------------

#[test]
fn accepts_valid_three_answer_response() {
    let request = all_kinds_request();
    let mut answers = BTreeMap::new();
    answers.insert("n".to_string(), noul_answer(0.75));
    answers.insert("c".to_string(), choice_answer("alpha", &[("alpha", 0.6), ("beta", 0.4)], 0.8));
    answers.insert(
        "s".to_string(),
        score_answer(
            1.5,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.1), ("1", 0.3), ("2", 0.6)],
            0.9,
        ),
    );
    let response =
        SystemOneDecisionResponse { model: "resolved-model".to_string(), answers, usage: usage() };
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn decodes_documented_response_json() {
    // Shapes and values copied verbatim from https://docs.typesafe.ai/api.md
    // (Noul) and https://docs.typesafe.ai/primitives/score.md (Score).
    let mut questions = BTreeMap::new();
    questions.insert(
        "is_urgent".to_string(),
        SystemOneQuestion::Noul {
            instructions: "Does this convey urgency?".to_string(),
            criteria: None,
        },
    );
    questions.insert(
        "severity".to_string(),
        SystemOneQuestion::Score {
            instructions: "How severe is the reported issue?".to_string(),
            criteria: vec![
                "Cosmetic; no impact to functionality".to_string(),
                "Broken or degraded feature, but workaround exists".to_string(),
                "Blocking issue; no workaround exists".to_string(),
            ],
        },
    );
    let request = SystemOneDecisionRequest {
        state: "The export button crashes the settings page in Safari.".to_string(),
        model: "jev-latest".to_string(),
        questions,
    };

    let body = r#"{
        "model": "jev-1.13.0",
        "answers": {
            "is_urgent": { "type": "noul", "noul": 0.95 },
            "severity": {
                "type": "score",
                "score": 1.43,
                "confidence": 0.35,
                "legend": {
                    "0": "Cosmetic; no impact to functionality",
                    "1": "Broken or degraded feature, but workaround exists",
                    "2": "Blocking issue; no workaround exists"
                },
                "probabilities": { "0": 0.0, "1": 0.57, "2": 0.43 }
            }
        },
        "usage": { "input_tokens": 296, "output_tokens": 20 }
    }"#;
    let decoded: SystemOneDecisionResponse =
        serde_json::from_str(body).expect("documented response must decode");
    assert_eq!(decoded.model, "jev-1.13.0");
    assert_eq!(decoded.usage, SystemOneUsage { input_tokens: 296, output_tokens: 20 });
    assert_eq!(validate_response(&request, &decoded), Ok(()));
}

#[test]
fn rejects_surplus_answer_id() {
    let request = single_noul_request();
    let response = single_answer_response(&request, "n", noul_answer(0.5));
    let mut response = response;
    response.answers.insert("extra".to_string(), noul_answer(0.5));
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::SurplusAnswer { id: "extra".to_string() })
    );
}

#[test]
fn rejects_missing_answer_id() {
    let request = single_noul_request();
    let response = SystemOneDecisionResponse {
        model: "server-model".to_string(),
        answers: BTreeMap::new(),
        usage: usage(),
    };
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::MissingAnswer { id: "n".to_string() })
    );
}

#[test]
fn rejects_wrong_answer_type() {
    let request = single_noul_request();
    let response = SystemOneDecisionResponse {
        model: "server-model".to_string(),
        answers: [("n".to_string(), choice_answer("alpha", &[("alpha", 1.0)], 1.0))]
            .into_iter()
            .collect(),
        usage: usage(),
    };
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::AnswerTypeMismatch {
            id: "n".to_string(),
            expected: "noul",
            actual: "choice",
        })
    );
}

#[test]
fn rejects_noul_above_one() {
    let request = single_noul_request();
    let response = single_answer_response(&request, "n", noul_answer(1.5));
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::NoulOutOfRange { id: "n".to_string(), value: 1.5 })
    );
}

#[test]
fn rejects_noul_below_zero() {
    let request = single_noul_request();
    let response = single_answer_response(&request, "n", noul_answer(-0.001));
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::NoulOutOfRange { id: "n".to_string(), value: -0.001 })
    );
}

#[test]
fn rejects_noul_non_finite() {
    let request = single_noul_request();
    let response = single_answer_response(&request, "n", noul_answer(f64::NAN));
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::NoulNotFinite { id: "n".to_string() })
    );
}

#[test]
fn rejects_confidence_out_of_range() {
    let request = request_with_one_choice();
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", 1.0), ("beta", 0.0)], 1.2),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ConfidenceOutOfRange { id: "c".to_string(), value: 1.2 })
    );

    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.5,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.1), ("1", 0.3), ("2", 0.6)],
            -0.2,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ConfidenceOutOfRange { id: "s".to_string(), value: -0.2 })
    );
}

#[test]
fn rejects_choice_probability_missing_option() {
    let request = request_with_one_choice();
    let response =
        single_answer_response(&request, "c", choice_answer("alpha", &[("alpha", 1.0)], 1.0));
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilityOptionMismatch {
            id: "c".to_string(),
            option: "beta".to_string(),
            surplus: false,
        })
    );
}

#[test]
fn rejects_choice_probability_surplus_option() {
    let request = request_with_one_choice();
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", 0.5), ("beta", 0.4), ("gamma", 0.1)], 1.0),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilityOptionMismatch {
            id: "c".to_string(),
            option: "gamma".to_string(),
            surplus: true,
        })
    );
}

#[test]
fn rejects_choice_reports_missing_option_before_surplus() {
    // The key-set equality check reports a declared option missing from the
    // probabilities before an extra probability option. Both defects are present
    // here, so the reported order is pinned: a check that scanned probabilities
    // first would report `gamma` (surplus) instead.
    let request = request_with_choice_options(&["alpha", "beta"]);
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", 1.0), ("gamma", 0.0)], 1.0),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilityOptionMismatch {
            id: "c".to_string(),
            option: "beta".to_string(),
            surplus: false,
        })
    );
}

#[test]
fn rejects_choice_probability_sum_drift() {
    let request = request_with_one_choice();
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", 0.5), ("beta", 0.4)], 1.0),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilitySumOutOfRange {
            id: "c".to_string(),
            sum: 0.9,
        })
    );
}

#[test]
fn accepts_choice_probability_sum_within_epsilon() {
    let request = request_with_one_choice();
    // Half the tolerance must be tolerated, and the fixture must actually drift:
    // reconstructing the second probability as `1 - p` made the float sum land
    // on exactly 1.0, so the test passed even with the tolerance removed.
    let probability = 0.5 + probability_sum_tolerance(2) / 2.0;
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", probability), ("beta", 0.5)], 1.0),
    );
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn rejects_choice_probability_not_finite() {
    let request = request_with_one_choice();
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", f64::NAN), ("beta", 0.0)], 1.0),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilityNotFinite {
            id: "c".to_string(),
            option: "alpha".to_string(),
        })
    );
}

#[test]
fn rejects_choice_probability_below_zero() {
    // A negative element still leaves the sum at one and the selected option is
    // a maximum-probability one, so neither the sum nor the argmax rule can see
    // it: -0.5 + 0.5 + 1.0 == 1.0.
    let request = request_with_choice_options(&["alpha", "beta", "gamma"]);
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("gamma", &[("alpha", -0.5), ("beta", 0.5), ("gamma", 1.0)], 1.0),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilityOutOfRange {
            id: "c".to_string(),
            option: "alpha".to_string(),
            value: -0.5,
        })
    );
}

#[test]
fn rejects_choice_probability_above_one() {
    // A value above one forces another element below zero for the sum to hold,
    // so this case necessarily carries both; the assertion pins the first
    // violation in key order to the 1.5 element that the sum rule accepts.
    let request = request_with_one_choice();
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("alpha", &[("alpha", 1.5), ("beta", -0.5)], 1.0),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceProbabilityOutOfRange {
            id: "c".to_string(),
            option: "alpha".to_string(),
            value: 1.5,
        })
    );
}

#[test]
fn rejects_choice_not_argmax() {
    let request = request_with_one_choice();
    let response = single_answer_response(
        &request,
        "c",
        choice_answer("beta", &[("alpha", 0.7), ("beta", 0.3)], 0.9),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ChoiceNotMaxProbability {
            id: "c".to_string(),
            choice: "beta".to_string(),
        })
    );
}

#[test]
fn rejects_score_probability_legend_mismatch() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.5,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.1), ("1", 0.9)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreLevelMismatch {
            id: "s".to_string(),
            level: "2".to_string(),
            surplus: false,
        })
    );

    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.5,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.1), ("1", 0.3), ("2", 0.6), ("extra", 0.0)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreLevelMismatch {
            id: "s".to_string(),
            level: "extra".to_string(),
            surplus: true,
        })
    );
}

#[test]
fn rejects_score_reports_missing_level_before_surplus() {
    // The legend key set must equal the probability key set. Both a missing
    // legend level and an extra probability level are present, pinning the
    // reported order to the legend-first scan: a probabilities-first scan would
    // report the surplus level `3` instead of the missing level `1`.
    let request = request_with_score_levels(3);
    let response = single_answer_response(
        &request,
        "s",
        score_answer(1.0, &[("0", "a"), ("1", "b"), ("2", "c")], &[("0", 1.0), ("3", 0.0)], 0.9),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreLevelMismatch {
            id: "s".to_string(),
            level: "1".to_string(),
            surplus: false,
        })
    );
}

#[test]
fn rejects_score_probability_sum_drift() {
    // The protocol requires the level probabilities to sum to one; three equal
    // halves (1.5) is the flagrant case the sum rule must catch.
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.0,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.5), ("1", 0.5), ("2", 0.5)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreProbabilitySumOutOfRange {
            id: "s".to_string(),
            sum: 1.5,
        })
    );
}

#[test]
fn accepts_score_probability_sum_within_epsilon() {
    let request = request_with_one_score();
    // Half the tolerance keeps the float-summed total inside it; the score is
    // the exact expectation so only the sum rule is exercised.
    let probability = 0.5 + probability_sum_tolerance(3) / 2.0;
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            0.5 + 2.0 * probability,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.0), ("1", 0.5), ("2", probability)],
            0.9,
        ),
    );
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn rejects_score_probability_not_finite() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.0,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", f64::NAN), ("1", 0.3), ("2", 0.7)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreProbabilityNotFinite {
            id: "s".to_string(),
            level: "0".to_string(),
        })
    );
}

#[test]
fn rejects_score_probability_below_zero() {
    // The sum (1.0), the expectation (2.0) and the score (2.0) are all
    // consistent; only the negative element itself is not a probability.
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            2.0,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", -0.5), ("1", 1.0), ("2", 0.5)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreProbabilityOutOfRange {
            id: "s".to_string(),
            level: "0".to_string(),
            value: -0.5,
        })
    );
}

#[test]
fn rejects_score_probability_above_one() {
    // Sum 1.0, expectation 0.5 and score 0.5 are consistent and the score lies
    // in [0, 2]; the 1.5 element is the first invalid one in key order. A value
    // above one forces the compensating element below zero for the sum to hold,
    // so this case necessarily carries both.
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            0.5,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.0), ("1", 1.5), ("2", -0.5)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreProbabilityOutOfRange {
            id: "s".to_string(),
            level: "1".to_string(),
            value: 1.5,
        })
    );
}

#[test]
fn rejects_empty_score_levels() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        SystemOneAnswer::Score {
            score: 0.0,
            legend: BTreeMap::new(),
            probabilities: BTreeMap::new(),
            confidence: 0.5,
        },
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::EmptyScoreLevels { id: "s".to_string() })
    );
}

#[test]
fn rejects_empty_legend_key() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(1.0, &[("", "blank"), ("1", "one")], &[("", 0.5), ("1", 0.5)], 0.9),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::EmptyLegendKey { id: "s".to_string() })
    );
}

#[test]
fn rejects_missing_ordinal_before_probability_or_score_errors() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            f64::NAN,
            &[("0", "worst"), ("1", "middle"), ("03", "noncanonical")],
            &[("0", f64::NAN)],
            1.2,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreLegendOrdinalMismatch {
            id: "s".to_string(),
            level: "2".to_string(),
            surplus: false,
        })
    );
}

#[test]
fn rejects_score_below_zero() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            -0.1,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.1), ("1", 0.3), ("2", 0.6)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreOutOfRange {
            id: "s".to_string(),
            value: -0.1,
            max_level: 2,
        })
    );
}

#[test]
fn rejects_score_above_top_level() {
    // The top level number of a three-level scale is 2; a fractional value
    // above it (2.9) and the level count itself (3.0) are both out of range.
    for value in [2.9, 3.0] {
        let request = request_with_one_score();
        let response = single_answer_response(
            &request,
            "s",
            score_answer(
                value,
                &[("0", "worst"), ("1", "middle"), ("2", "best")],
                &[("0", 0.1), ("1", 0.3), ("2", 0.6)],
                0.9,
            ),
        );
        assert_eq!(
            validate_response(&request, &response),
            Err(SystemOneResponseError::ScoreOutOfRange {
                id: "s".to_string(),
                value,
                max_level: 2,
            }),
            "score {value} must be rejected above the top level 2"
        );
    }
}

#[test]
fn accepts_score_at_top_level() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            2.0,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.0), ("1", 0.0), ("2", 1.0)],
            1.0,
        ),
    );
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn rejects_score_non_finite() {
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            f64::INFINITY,
            &[("0", "worst"), ("1", "middle"), ("2", "best")],
            &[("0", 0.1), ("1", 0.3), ("2", 0.6)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreNotFinite { id: "s".to_string() })
    );
}

/// Builds a `score` request whose question declares `count` ordered levels, so
/// the answer must supply exactly the ordinal legend keys `"0"`..`count - 1`.
fn request_with_score_levels(count: usize) -> SystemOneDecisionRequest {
    let mut questions = BTreeMap::new();
    questions.insert(
        "s".to_string(),
        SystemOneQuestion::Score {
            instructions: "Grade.".to_string(),
            criteria: (0..count).map(|index| format!("level {index}")).collect(),
        },
    );
    SystemOneDecisionRequest {
        state: "state text".to_string(),
        model: "caller-model".to_string(),
        questions,
    }
}

#[test]
fn rejects_score_not_probability_weighted_expectation() {
    // The score is the probability-weighted position on the level number line
    // (https://docs.typesafe.ai/primitives/score.md): all mass on level 0 means
    // score 0. A score of 3.0 is inside [0, 3] but contradicts its own
    // distribution, and the range check alone cannot see that.
    let request = request_with_score_levels(4);
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            3.0,
            &[("0", "none"), ("1", "some"), ("2", "much"), ("3", "all")],
            &[("0", 1.0), ("1", 0.0), ("2", 0.0), ("3", 0.0)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreExpectationMismatch {
            id: "s".to_string(),
            score: 3.0,
            expected: 0.0,
        })
    );
}

#[test]
fn rejects_score_legend_non_ordinal_keys() {
    // The documented legend keys are the ordinal strings "0".."N-1"; a
    // low/mid/high legend does not name the requested levels.
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.5,
            &[("low", "worst"), ("mid", "middle"), ("high", "best")],
            &[("low", 0.1), ("mid", 0.3), ("high", 0.6)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreLegendOrdinalMismatch {
            id: "s".to_string(),
            level: "0".to_string(),
            surplus: false,
        })
    );
}

#[test]
fn rejects_score_legend_wrong_level_count() {
    // A three-level question must not be answered with five legend entries.
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            2.0,
            &[("0", "a"), ("1", "b"), ("2", "c"), ("3", "d"), ("4", "e")],
            &[("0", 0.0), ("1", 0.0), ("2", 0.6), ("3", 0.3), ("4", 0.1)],
            0.9,
        ),
    );
    assert_eq!(
        validate_response(&request, &response),
        Err(SystemOneResponseError::ScoreLegendOrdinalMismatch {
            id: "s".to_string(),
            level: "3".to_string(),
            surplus: true,
        })
    );
}

#[test]
fn accepts_documented_score_1_43() {
    // The canonical Score example from https://docs.typesafe.ai/primitives/score.md:
    // score 1.43, probabilities {0: 0.0, 1: 0.57, 2: 0.43}, whose
    // probability-weighted expectation is exactly 1.43.
    let request = request_with_one_score();
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            1.43,
            &[
                ("0", "Cosmetic; no impact to functionality"),
                ("1", "Broken or degraded feature, but workaround exists"),
                ("2", "Blocking issue; no workaround exists"),
            ],
            &[("0", 0.0), ("1", 0.57), ("2", 0.43)],
            0.35,
        ),
    );
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn score_rounding_tolerance_matches_published_quantum() {
    // The score and every probability are published rounded to two decimals,
    // so the bound is half a quantum for the score plus half a quantum per unit
    // of index weight. The expectation of {0: 0, 1: 0.5, 2: 0.5} is 1.5.
    let request = request_with_one_score();
    let expectation = 1.5_f64;
    let probabilities = &[("0", 0.0), ("1", 0.5), ("2", 0.5)];

    let within = expectation + score_expectation_tolerance(3) - 1e-6;
    let at_boundary = single_answer_response(
        &request,
        "s",
        score_answer(within, &[("0", "a"), ("1", "b"), ("2", "c")], probabilities, 0.9),
    );
    assert_eq!(
        validate_response(&request, &at_boundary),
        Ok(()),
        "a score inside the published rounding bound of the expectation must pass"
    );

    let beyond = expectation + score_expectation_tolerance(3) + 1e-3;
    let outside = single_answer_response(
        &request,
        "s",
        score_answer(beyond, &[("0", "a"), ("1", "b"), ("2", "c")], probabilities, 0.9),
    );
    assert_eq!(
        validate_response(&request, &outside),
        Err(SystemOneResponseError::ScoreExpectationMismatch {
            id: "s".to_string(),
            score: beyond,
            expected: expectation,
        }),
        "a score beyond the published rounding bound of the expectation must fail"
    );
}

#[test]
fn accepts_live_score_whose_rounded_expectation_drifts_by_two_quanta() {
    // Captured from api.typesafe.ai (jev-latest): the probabilities recompute to
    // 0.03 while the published score is 0.05. Every value is rounded to two
    // decimals independently, so the drift is rounding, not a contract breach.
    let request = request_with_score_levels(5);
    let response = single_answer_response(
        &request,
        "s",
        score_answer(
            0.05,
            &[("0", "l0"), ("1", "l1"), ("2", "l2"), ("3", "l3"), ("4", "l4")],
            &[("0", 0.97), ("1", 0.03), ("2", 0.0), ("3", 0.0), ("4", 0.0)],
            0.96,
        ),
    );
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn accepts_live_choice_whose_rounded_distribution_sums_to_0_99() {
    // Captured from api.typesafe.ai (jev-latest) for a ten-type entity question:
    // ten independently rounded probabilities sum to 0.99.
    let options = [
        "person",
        "organization",
        "location",
        "event",
        "artifact",
        "natural",
        "process",
        "concept",
        "attribute",
        "entity",
    ];
    let request = request_with_choice_options(&options);
    let response = single_answer_response(
        &request,
        "c",
        choice_answer(
            "attribute",
            &[
                ("person", 0.0),
                ("organization", 0.0),
                ("location", 0.0),
                ("event", 0.0),
                ("artifact", 0.01),
                ("natural", 0.0),
                ("process", 0.03),
                ("concept", 0.4),
                ("attribute", 0.54),
                ("entity", 0.01),
            ],
            0.49,
        ),
    );
    assert_eq!(validate_response(&request, &response), Ok(()));
}

#[test]
fn rounding_tolerances_scale_with_the_published_quantum() {
    assert!((probability_sum_tolerance(10) - 0.05).abs() < 1e-6);
    assert!((score_expectation_tolerance(2) - 0.01).abs() < 1e-6);
    assert!((score_expectation_tolerance(5) - 0.055).abs() < 1e-6);
}

#[test]
fn rejects_malformed_body() {
    let error =
        serde_json::from_str::<SystemOneDecisionResponse>("not json").expect_err("must fail");
    assert!(matches!(SystemOneResponseError::from(error), SystemOneResponseError::Decode(_)));
}

#[test]
fn accepts_additive_unknown_response_fields() {
    // A server may add response metadata (a top-level id, extra answer fields,
    // extra usage counters) without breaking a deployed client; the validated
    // shape is enforced by `validate_response`, not by decode strictness.
    let request = single_noul_request();
    let body = serde_json::json!({
        "model": "server-model",
        "id": "req_123",
        "answers": {"n": {"type": "noul", "noul": 0.5, "note": "additive"}},
        "usage": {"input_tokens": 12, "output_tokens": 3, "total_tokens": 15},
    });
    let decoded: SystemOneDecisionResponse =
        serde_json::from_value(body).expect("additive response fields must decode");
    assert_eq!(decoded.usage, usage());
    assert_eq!(validate_response(&request, &decoded), Ok(()));
}

#[test]
fn rejects_negative_usage_on_wire() {
    let body = serde_json::json!({
        "model": "server-model",
        "answers": {"n": {"type": "noul", "noul": 0.5}},
        "usage": {"input_tokens": -1, "output_tokens": 1},
    });
    assert!(serde_json::from_value::<SystemOneDecisionResponse>(body).is_err());
}
