use super::*;

#[test]
fn grounded_answer_evidence_ledger_retains_partial_and_repair_entries() {
    let mut ledger = GroundedAnswerEvidenceLedger::default();
    let partial = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000001",
            "answerBody": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded.",
            "finalAnswerReady": false,
            "finalizable": false,
            "mustPreserveSpans": ["Alpha path", "Beta recovery path", "Beta recovery summary"],
            "executionDetail": {
                "verificationState": "conflicting",
                "verificationWarnings": [{"code": "partial_coverage"}],
                "preparedSegmentReferences": [{"documentTitle": "Alpha operator guide"}],
                "entityReferences": [{"label": "Beta recovery path", "summary": "Beta recovery summary"}],
                "relationReferences": [{"normalizedAssertion": "Alpha supports beta recovery"}]
            }
        },
            "content": [{"type": "text", "text": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded."}],
        "isError": false
    }));
    let repair = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000002",
            "answerBody": "Gamma repair path is supported.",
            "finalAnswerReady": true,
            "finalizable": true,
            "mustPreserveSpans": ["Gamma repair path"],
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": [],
                "preparedSegmentReferences": [{"documentTitle": "Gamma repair guide"}],
                "entityReferences": [],
                "relationReferences": []
            }
        },
        "content": [{"type": "text", "text": "Gamma repair path is supported."}],
        "isError": false
    }));

    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);
    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &repair);

    let message = ledger.system_message().expect("ledger message");
    assert!(message.contains("Same-turn grounded_answer evidence ledger"));
    assert!(message.contains("finalizable=false"));
    assert!(message.contains("finalizable=true"));
    assert!(message.contains("Alpha path"));
    assert!(message.contains("Beta recovery path"));
    assert!(message.contains("Beta recovery summary"));
    assert!(message.contains("Gamma repair path"));
    assert!(message.contains("partial_coverage"));
}

#[test]
fn grounded_answer_evidence_ledger_guards_answer_that_drops_anchors() {
    let mut ledger = GroundedAnswerEvidenceLedger::default();
    let partial = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000011",
            "answerBody": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded.",
            "finalAnswerReady": false,
            "finalizable": false,
            "mustPreserveSpans": ["Alpha path", "Beta recovery path", "Beta recovery summary"],
            "executionDetail": {
                "verificationState": "conflicting",
                "verificationWarnings": [{"code": "partial_coverage"}],
                "preparedSegmentReferences": [{"documentTitle": "Alpha operator guide"}],
                "entityReferences": [{"label": "Beta recovery path", "summary": "Beta recovery summary"}],
                "relationReferences": [{"normalizedAssertion": "Alpha supports beta recovery"}]
            }
        },
            "content": [{"type": "text", "text": "Alpha path is supported. Beta recovery path still needs repair. Beta recovery summary is grounded."}],
        "isError": false
    }));
    let repair = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000012",
            "answerBody": "Gamma repair path is supported. Gamma repair guide names Gamma repair action.",
            "finalAnswerReady": true,
            "finalizable": true,
            "mustPreserveSpans": [
                "Gamma repair path",
                "Gamma repair guide",
                "Gamma repair action"
            ],
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": [],
                "preparedSegmentReferences": [{"documentTitle": "Gamma repair guide"}],
                "entityReferences": [],
                "relationReferences": []
            }
        },
        "content": [{"type": "text", "text": "Gamma repair path is supported."}],
        "isError": false
    }));

    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);
    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &repair);

    let dropped_answer = "No matching operational branch is described.";
    let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

    assert!(!guard.contains("Alpha path"));
    assert!(!guard.contains("Beta recovery path"));
    assert!(!guard.contains("Beta recovery summary"));
    assert!(guard.contains("Gamma repair path"));
    assert!(guard.contains("Gamma repair guide"));
    assert!(guard.contains("Gamma repair action"));

    assert!(ledger.guard_candidate_for_answer(&guard).is_none());
}

#[test]
fn grounded_answer_evidence_ledger_rejects_insufficient_high_signal_inventory() {
    let mut ledger = GroundedAnswerEvidenceLedger::default();
    let answer_body = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `currency = USD`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`\n\
- `fillDetails = true`\n\
- `printSlip = false`\n\
- `visible = true`";
    let partial = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000021",
            "answerBody": answer_body,
            "finalAnswerReady": false,
            "finalizable": false,
            "mustPreserveSpans": [
                "Provider Alpha Guide",
                "/opt/alpha/alpha.conf",
                "[Main]",
                "url = http://localhost",
                "timeout = 10",
                "merchantId",
                "secretKey",
                "currency = USD",
                "retryWindow = 60",
                "pollInterval = 1",
                "fillDetails = true",
                "printSlip = false",
                "visible = true"
            ],
            "executionDetail": {
                "verificationState": "insufficient_evidence",
                "verificationWarnings": [{
                    "code": "unsupported_literal",
                    "message": "Literal `currency = USD` is not grounded in selected evidence."
                }],
                "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                "entityReferences": [],
                "relationReferences": []
            }
        },
        "content": [{"type": "text", "text": answer_body}],
        "isError": false
    }));

    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);

    let dropped_answer = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`";
    assert!(
        ledger.guard_candidate_for_answer(dropped_answer).is_none(),
        "a non-terminal typed answer must not be promoted back to factual evidence"
    );
}

#[test]
fn grounded_answer_evidence_ledger_does_not_guard_from_conflicting_partial_inventory() {
    let mut ledger = GroundedAnswerEvidenceLedger::default();
    let answer_body = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `fillDetails = true`\n\
- `printSlip = false`\n\
- `visible = true`";
    let partial = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000024",
            "answerBody": answer_body,
            "finalAnswerReady": false,
            "finalizable": false,
            "mustPreserveSpans": [
                "Provider Alpha Guide",
                "/opt/alpha/alpha.conf",
                "[Main]",
                "url = http://localhost",
                "timeout = 10",
                "merchantId",
                "secretKey",
                "fillDetails = true",
                "printSlip = false",
                "visible = true"
            ],
            "executionDetail": {
                "verificationState": "conflicting",
                "verificationWarnings": [{"code": "partial_coverage"}],
                "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                "entityReferences": [],
                "relationReferences": []
            }
        },
        "content": [{"type": "text", "text": answer_body}],
        "isError": false
    }));

    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &partial);

    let dropped_answer = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`";
    assert!(ledger.guard_candidate_for_answer(dropped_answer).is_none());
}

#[test]
fn grounded_answer_evidence_ledger_guards_missing_verified_high_signal_spans() {
    let mut ledger = GroundedAnswerEvidenceLedger::default();
    let answer_body = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `currency = USD`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`\n\
- `fillDetails = true`\n\
- `printSlip = false`\n\
- `visible = true`";
    let verified = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000022",
            "answerBody": answer_body,
            "finalAnswerReady": true,
            "finalizable": true,
            "mustPreserveSpans": [
                "Provider Alpha Guide",
                "/opt/alpha/alpha.conf",
                "[Main]",
                "url = http://localhost",
                "timeout = 10",
                "merchantId",
                "secretKey",
                "currency = USD",
                "retryWindow = 60",
                "pollInterval = 1",
                "fillDetails = true",
                "printSlip = false",
                "visible = true"
            ],
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": [{
                    "code": "unsupported_literal",
                    "message": "Literal `currency = USD` is not grounded in selected evidence."
                }],
                "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                "entityReferences": [],
                "relationReferences": []
            }
        },
        "content": [{"type": "text", "text": answer_body}],
        "isError": false
    }));

    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &verified);

    let dropped_answer = "`Provider Alpha Guide`\n\
- `/opt/alpha/alpha.conf`\n\
- `[Main]`\n\
- `url = http://localhost`\n\
- `timeout = 10`\n\
- `merchantId`\n\
- `secretKey`\n\
- `retryWindow = 60`\n\
- `pollInterval = 1`";
    let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

    assert!(guard.contains("fillDetails = true"));
    assert!(guard.contains("printSlip = false"));
    assert!(guard.contains("visible = true"));
    assert!(guard.contains("\n- `fillDetails = true`"));
    assert!(
        !ledger.guard_high_signal_anchor_set().contains("currency = USD"),
        "unsupported warning literals must not become high-signal guard anchors"
    );
}

#[test]
fn grounded_answer_evidence_ledger_guard_preserves_body_beyond_excerpt_limit() {
    let mut ledger = GroundedAnswerEvidenceLedger::default();
    let mut answer_body = String::from("Provider Alpha Guide\n\n");
    for index in 0..90 {
        answer_body.push_str(&format!(
            "- field{index:02} = value{index:02}; keep this operational detail intact.\n"
        ));
    }
    answer_body.push_str("- finalSetting = enabled\n");
    answer_body.push_str("- lateIdentifier = preserve_me\n");
    assert!(answer_body.chars().count() > GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS);

    let verified = grounded_answer_ledger_outcome(serde_json::json!({
        "structuredContent": {
            "executionId": "00000000-0000-0000-0000-000000000023",
            "answerBody": answer_body,
            "finalAnswerReady": true,
            "finalizable": true,
            "mustPreserveSpans": [
                "Provider Alpha Guide",
                "field00 = value00",
                "field01 = value01",
                "finalSetting = enabled",
                "lateIdentifier = preserve_me"
            ],
            "executionDetail": {
                "verificationState": "verified",
                "verificationWarnings": [],
                "preparedSegmentReferences": [{"documentTitle": "Provider Alpha Guide"}],
                "entityReferences": [],
                "relationReferences": []
            }
        },
        "content": [{"type": "text", "text": answer_body}],
        "isError": false
    }));

    ledger.remember(GROUNDED_ANSWER_TOOL_NAME, &verified);

    let dropped_answer = "Provider Alpha Guide\n- field00 = value00\n- field01 = value01";
    let guard = ledger.guard_candidate_for_answer(dropped_answer).expect("guard candidate");

    assert!(guard.chars().count() > GROUNDED_EVIDENCE_LEDGER_ANSWER_CHARS);
    assert!(guard.contains("finalSetting = enabled"));
    assert!(guard.contains("lateIdentifier = preserve_me"));
    assert!(guard.contains("\n- lateIdentifier = preserve_me"));
}
