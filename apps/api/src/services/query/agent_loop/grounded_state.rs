use super::*;

#[derive(Debug, Clone)]
pub(super) struct ToolExecutionOutcome {
    pub(super) arguments_json: Option<String>,
    pub(super) requested_arguments_json: Option<String>,
    pub(super) message_content: String,
    pub(super) result_text: Option<String>,
    pub(super) result_json: Option<Value>,
    pub(super) grounding_text: Option<String>,
    /// Exact structured `answerBody` captured before debug/result compaction.
    /// This is the only text eligible for exact verbatim passthrough.
    pub(super) grounded_answer_body: Option<String>,
    /// Typed terminal outcome parsed from the unabridged MCP structured
    /// content. Debug compaction must never be used to reconstruct it.
    pub(super) grounded_answer_outcome: Option<AgentGroundedAnswerOutcome>,
    pub(super) grounded_answer_ready: bool,
    pub(super) grounded_answer_completed: bool,
    pub(super) grounded_answer_needs_follow_up: bool,
    pub(super) grounded_answer_repair_reason: Option<GroundedAnswerRepairReason>,
    pub(super) grounded_answer_language: QueryLanguage,
    pub(super) grounded_answer_clarification_required: bool,
    pub(super) is_error: bool,
    /// True when this outcome was replayed from a prior successful call's
    /// cached payload (effective-duplicate) rather than produced by a fresh
    /// execution. A replay is not a *new* successful tool result, so the
    /// no-progress guard treats it the same as an error/duplicate when
    /// deciding whether an iteration made progress.
    pub(super) is_replay: bool,
    /// Wall-clock the tool ran. Set centrally in `execute_tool_calls`;
    /// constructors default to 0.
    pub(super) duration_ms: u64,
    pub(super) child_query_execution_ids: Vec<Uuid>,
    pub(super) child_runtime_execution_ids: Vec<Uuid>,
}

#[derive(Debug, PartialEq)]
pub(super) struct TerminalGroundedAnswerPassthrough {
    pub(super) answer: String,
    pub(super) stop_reason: AgentStopReason,
    pub(super) grounded_answer_outcome: AgentGroundedAnswerOutcome,
}

impl TerminalGroundedAnswerPassthrough {
    pub(super) fn final_answer(
        answer: String,
        grounded_answer_outcome: AgentGroundedAnswerOutcome,
    ) -> Self {
        Self { answer, stop_reason: AgentStopReason::FinalAnswer, grounded_answer_outcome }
    }
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum GroundedAnswerRepairReason {
    ProcedureIncomplete,
    TroubleshootingIncomplete,
    AnswerStructureIncomplete,
    OrderedInventory { expected: usize, observed: usize },
    VerificationIncomplete,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum RuntimeGroundedRepairKind {
    ProcedureIncomplete,
    TroubleshootingIncomplete,
    AnswerStructureIncomplete,
    #[serde(rename = "ordered_inventory_incomplete")]
    OrderedInventory,
    VerificationIncomplete,
}

#[derive(Debug, Clone, Copy, serde::Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct RuntimeGroundedRepairMetadata {
    pub(super) reason: RuntimeGroundedRepairKind,
    pub(super) language: QueryLanguage,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) expected: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) observed: Option<usize>,
}

impl RuntimeGroundedRepairMetadata {
    pub(super) fn from_reason(reason: GroundedAnswerRepairReason, language: QueryLanguage) -> Self {
        match reason {
            GroundedAnswerRepairReason::ProcedureIncomplete => Self {
                reason: RuntimeGroundedRepairKind::ProcedureIncomplete,
                language,
                expected: None,
                observed: None,
            },
            GroundedAnswerRepairReason::TroubleshootingIncomplete => Self {
                reason: RuntimeGroundedRepairKind::TroubleshootingIncomplete,
                language,
                expected: None,
                observed: None,
            },
            GroundedAnswerRepairReason::AnswerStructureIncomplete => Self {
                reason: RuntimeGroundedRepairKind::AnswerStructureIncomplete,
                language,
                expected: None,
                observed: None,
            },
            GroundedAnswerRepairReason::OrderedInventory { expected, observed } => Self {
                reason: RuntimeGroundedRepairKind::OrderedInventory,
                language,
                expected: Some(expected),
                observed: Some(observed),
            },
            GroundedAnswerRepairReason::VerificationIncomplete => Self {
                reason: RuntimeGroundedRepairKind::VerificationIncomplete,
                language,
                expected: None,
                observed: None,
            },
        }
    }

    pub(super) fn is_consistent(self) -> bool {
        match self.reason {
            RuntimeGroundedRepairKind::OrderedInventory => matches!(
                (self.expected, self.observed),
                (Some(expected), Some(observed)) if expected > 0 && observed < expected
            ),
            RuntimeGroundedRepairKind::ProcedureIncomplete
            | RuntimeGroundedRepairKind::TroubleshootingIncomplete
            | RuntimeGroundedRepairKind::AnswerStructureIncomplete
            | RuntimeGroundedRepairKind::VerificationIncomplete => {
                self.expected.is_none() && self.observed.is_none()
            }
        }
    }
}

/// A grounded answer gets at most one deterministic repair probe per parent
/// turn. Keeping this budget in the runtime (rather than only in the prompt)
/// prevents chooser/retry loops while guaranteeing that the repair query is
/// not an effective duplicate of the original probe.
#[derive(Debug)]
pub(super) struct FocusedGroundedFollowUpState {
    pub(super) pending: Option<RuntimeGroundedRepairMetadata>,
    pub(super) attempted: bool,
    pub(super) unresolved: bool,
    pub(super) language: QueryLanguage,
}

impl Default for FocusedGroundedFollowUpState {
    fn default() -> Self {
        Self { pending: None, attempted: false, unresolved: false, language: QueryLanguage::Auto }
    }
}

impl FocusedGroundedFollowUpState {
    pub(super) fn schedule(
        &mut self,
        reason: GroundedAnswerRepairReason,
        language: QueryLanguage,
    ) -> bool {
        if self.attempted || self.pending.is_some() {
            return false;
        }
        self.pending = Some(RuntimeGroundedRepairMetadata::from_reason(reason, language));
        self.unresolved = true;
        self.language = language;
        true
    }

    pub(super) fn take_call(
        &mut self,
        user_question: &str,
        grounded_top_k: usize,
    ) -> Option<ChatToolCall> {
        let metadata = self.pending.take()?;
        self.attempted = true;
        Some(focused_grounded_answer_follow_up_call(user_question, grounded_top_k, metadata))
    }

    pub(super) fn observe_attempt_outcome(&mut self, outcome: &ToolExecutionOutcome) {
        if !self.attempted || !self.unresolved {
            return;
        }
        if !outcome.is_error
            && !outcome.is_replay
            && outcome.grounded_answer_ready
            && !outcome.grounded_answer_needs_follow_up
            && outcome.grounded_answer_repair_reason.is_none()
        {
            self.unresolved = false;
        }
    }

    pub(super) fn requires_resolution(&self) -> bool {
        self.unresolved
    }

    pub(super) fn is_unresolved_after_attempt(&self) -> bool {
        self.attempted && self.pending.is_none() && self.unresolved
    }

    pub(super) fn was_attempted(&self) -> bool {
        self.attempted
    }

    pub(super) fn language(&self) -> QueryLanguage {
        self.language
    }
}

#[derive(Debug, Default, Clone)]
pub(super) struct GroundedAnswerEvidenceLedger {
    pub(super) entries: Vec<GroundedAnswerEvidenceEntry>,
    pub(super) seen_keys: BTreeSet<String>,
}

#[derive(Debug, Clone)]
pub(super) struct GroundedAnswerEvidenceEntry {
    pub(super) index: usize,
    pub(super) execution_id: Option<String>,
    pub(super) final_answer_ready: bool,
    pub(super) finalizable: bool,
    pub(super) answer_disposition: ironrag_contracts::assistant::AssistantAnswerDisposition,
    pub(super) verification_state: Option<String>,
    pub(super) warning_codes: Vec<String>,
    pub(super) unsupported_literal_spans: BTreeSet<String>,
    pub(super) answer_body: Option<String>,
    pub(super) answer_excerpt: Option<String>,
    pub(super) must_preserve_spans: Vec<String>,
    pub(super) source_labels: Vec<String>,
}

impl GroundedAnswerEvidenceLedger {
    pub(super) fn remember(&mut self, tool_name: &str, outcome: &ToolExecutionOutcome) {
        if tool_name != GROUNDED_ANSWER_TOOL_NAME || outcome.is_error {
            return;
        }
        if self.entries.len() >= GROUNDED_EVIDENCE_LEDGER_ENTRY_LIMIT {
            return;
        }
        let Some(result_json) = outcome.result_json.as_ref() else {
            return;
        };
        let Some(entry) = GroundedAnswerEvidenceEntry::from_result(
            self.entries.len() + 1,
            result_json,
            outcome.result_text.as_deref(),
        ) else {
            return;
        };
        let key = entry
            .execution_id
            .clone()
            .unwrap_or_else(|| format!("entry:{}", self.entries.len() + 1));
        if self.seen_keys.insert(key) {
            self.entries.push(entry);
        }
    }

    pub(super) fn system_message(&self) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let mut message = String::from(
            "Same-turn grounded_answer evidence ledger. Treat these entries as accumulated tool evidence, not as a replacement final answer. Later focused repair entries add or clarify coverage; they do not erase earlier grounded facts, source labels, warnings, or action paths unless directly contradicted. Before finalizing, cover the relevant preserved spans and source labels from all entries, or mark unsupported/conflicting branches plainly.",
        );
        for entry in &self.entries {
            entry.push_to_message(&mut message);
            if message.chars().count() >= GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS {
                message = message.chars().take(GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS).collect();
                break;
            }
        }
        Some(message)
    }

    pub(super) fn guard_candidate_for_answer(&self, answer: &str) -> Option<String> {
        let high_signal_missing_count = self
            .guard_high_signal_anchor_set()
            .iter()
            .filter(|anchor| !answer_contains_guard_anchor(answer, anchor))
            .count();
        if high_signal_missing_count
            >= GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_MISSING_HIGH_SIGNAL_ANCHORS
        {
            return self.answer_guard_text();
        }

        let anchors = self.guard_anchor_set();
        if anchors.len() < GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_ANCHORS {
            return None;
        }
        let missing_count =
            anchors.iter().filter(|anchor| !answer_contains_guard_anchor(answer, anchor)).count();
        let missing_threshold = anchors.len().div_ceil(2).max(2);
        if missing_count < missing_threshold {
            return None;
        }
        self.answer_guard_text()
    }

    pub(super) fn guard_high_signal_anchor_set(&self) -> BTreeSet<String> {
        let mut anchors = BTreeSet::new();
        for entry in self.guardable_entries() {
            push_grounded_evidence_ledger_high_signal_anchors(
                &mut anchors,
                &entry.must_preserve_spans,
                &entry.unsupported_literal_spans,
            );
            if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                break;
            }
        }
        anchors
    }

    pub(super) fn guard_anchor_set(&self) -> BTreeSet<String> {
        let mut anchors = BTreeSet::new();
        for entry in self.guardable_entries() {
            push_grounded_evidence_ledger_anchors(&mut anchors, &entry.must_preserve_spans);
            if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                return anchors;
            }
        }
        if anchors.len() < GROUNDED_EVIDENCE_LEDGER_GUARD_MIN_ANCHORS {
            for entry in self.guardable_entries() {
                push_grounded_evidence_ledger_anchors(&mut anchors, &entry.source_labels);
                if anchors.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                    return anchors;
                }
            }
        }
        anchors
    }

    pub(super) fn guardable_entries(&self) -> impl Iterator<Item = &GroundedAnswerEvidenceEntry> {
        self.entries.iter().filter(|entry| entry.can_guard_final_answer())
    }

    pub(super) fn has_guardable_evidence(&self) -> bool {
        self.guardable_entries().next().is_some()
    }

    pub(super) fn answer_guard_text(&self) -> Option<String> {
        let mut lines = Vec::new();
        let mut seen = BTreeSet::new();
        for entry in self.guardable_entries() {
            if let Some(answer) = &entry.answer_body {
                push_guard_answer_line(&mut lines, &mut seen, answer);
            }
            if lines.len() >= GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }
        let text = lines.join("\n");
        Some(text.chars().take(GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS).collect())
    }
}

impl GroundedAnswerEvidenceEntry {
    pub(super) fn from_result(
        index: usize,
        result_json: &Value,
        fallback_text: Option<&str>,
    ) -> Option<Self> {
        let structured = result_json.get("structuredContent")?;
        let completion_envelope = try_parse_grounded_answer_completion_envelope(structured);
        let answer_body = structured
            .get("answerBody")
            .and_then(Value::as_str)
            .or(fallback_text)
            .map(str::trim)
            .map(|text| text.chars().take(GROUNDED_EVIDENCE_LEDGER_TEXT_CHARS).collect::<String>())
            .filter(|text| !text.trim().is_empty());
        let answer_excerpt =
            answer_body.as_deref().map(compact_ledger_text).filter(|text| !text.trim().is_empty());
        let mut must_preserve_spans = Vec::new();
        let mut seen_spans = BTreeSet::new();
        push_json_string_array(
            &mut must_preserve_spans,
            &mut seen_spans,
            structured.get("mustPreserveSpans"),
            GROUNDED_EVIDENCE_LEDGER_SPAN_LIMIT,
        );

        let execution_detail = structured.get("executionDetail");
        let mut source_labels = Vec::new();
        let mut seen_labels = BTreeSet::new();
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/relationReferences",
            "normalizedAssertion",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );
        for field_name in ["documentTitle", "displayValue", "label", "assertion", "predicate"] {
            push_reference_field_values(
                &mut source_labels,
                &mut seen_labels,
                Some(structured),
                "/referenceSummary/references",
                field_name,
                GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
            );
        }
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/entityReferences",
            "label",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/entityReferences",
            "summary",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );
        push_reference_field_values(
            &mut source_labels,
            &mut seen_labels,
            execution_detail,
            "/preparedSegmentReferences",
            "documentTitle",
            GROUNDED_EVIDENCE_LEDGER_SOURCE_LABEL_LIMIT,
        );

        if answer_excerpt.is_none() && must_preserve_spans.is_empty() && source_labels.is_empty() {
            return None;
        }

        Some(Self {
            index,
            execution_id: structured
                .get("executionId")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            final_answer_ready: completion_envelope
                .as_ref()
                .is_some_and(|envelope| envelope.final_answer_ready),
            finalizable: completion_envelope.as_ref().is_some_and(|envelope| envelope.finalizable),
            answer_disposition: completion_envelope.as_ref().map_or(
                ironrag_contracts::assistant::AssistantAnswerDisposition::NonTerminal,
                |envelope| envelope.readiness.answer_disposition,
            ),
            verification_state: execution_detail
                .and_then(|detail| detail.get("verificationState"))
                .or_else(|| structured.pointer("/verifier/state"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            warning_codes: collect_verification_warning_codes(execution_detail, Some(structured)),
            unsupported_literal_spans: collect_unsupported_literal_spans(
                execution_detail,
                Some(structured),
            ),
            answer_body,
            answer_excerpt,
            must_preserve_spans,
            source_labels,
        })
    }

    pub(super) fn push_to_message(&self, message: &mut String) {
        message.push_str("\n\n[grounded_answer ");
        message.push_str(&self.index.to_string());
        message.push_str("] finalAnswerReady=");
        message.push_str(if self.final_answer_ready { "true" } else { "false" });
        message.push_str(" finalizable=");
        message.push_str(if self.finalizable { "true" } else { "false" });
        if let Some(state) = &self.verification_state {
            message.push_str(" verificationState=");
            message.push_str(state);
        }
        if !self.warning_codes.is_empty() {
            message.push_str(" warnings=");
            message.push_str(&self.warning_codes.join(", "));
        }
        if let Some(excerpt) = &self.answer_excerpt {
            message.push_str("\nanswerExcerpt: ");
            message.push_str(excerpt);
        }
        if !self.must_preserve_spans.is_empty() {
            message.push_str("\nmustPreserveSpans: ");
            message.push_str(&self.must_preserve_spans.join(" | "));
        }
        if !self.source_labels.is_empty() {
            message.push_str("\nsourceLabelsAndEvidenceSummaries: ");
            message.push_str(&self.source_labels.join(" | "));
        }
    }

    pub(super) fn can_guard_final_answer(&self) -> bool {
        matches!(
            self.answer_disposition,
            ironrag_contracts::assistant::AssistantAnswerDisposition::FactualReady
        ) && self.answer_body.as_deref().is_some_and(|answer| !answer.trim().is_empty())
    }
}
