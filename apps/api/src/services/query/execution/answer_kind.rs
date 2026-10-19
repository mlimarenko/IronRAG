#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AnswerKind {
    DeterministicGroundedAnswer,
    ExactVersionChangeSummary,
    MissingExplicitDocument,
    OrderedSourceSlice,
    SetupConfigurationAnchor,
    TargetedTableAnswer,
    UpdateProcedureSequence,
}

impl AnswerKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::DeterministicGroundedAnswer => "deterministic_grounded_answer",
            Self::ExactVersionChangeSummary => "exact_version_change_summary",
            Self::MissingExplicitDocument => "missing_explicit_document",
            Self::OrderedSourceSlice => "ordered_source_slice",
            Self::SetupConfigurationAnchor => "setup_configuration_anchor",
            Self::TargetedTableAnswer => "targeted_table_answer",
            Self::UpdateProcedureSequence => "update_procedure_sequence",
        }
    }

    /// Reads diagnostic metadata produced by current or historical binaries.
    ///
    /// This is persisted wire data, so an unrecognized value must remain
    /// non-authoritative. Retired and future values are ignored instead of
    /// making an older conversation unreadable.
    pub(super) fn from_usage_json(usage_json: &serde_json::Value) -> Option<Self> {
        match usage_json.get("answer_kind").and_then(serde_json::Value::as_str)? {
            "deterministic_grounded_answer" => Some(Self::DeterministicGroundedAnswer),
            "exact_version_change_summary" => Some(Self::ExactVersionChangeSummary),
            "missing_explicit_document" => Some(Self::MissingExplicitDocument),
            "ordered_source_slice" => Some(Self::OrderedSourceSlice),
            "setup_configuration_anchor" => Some(Self::SetupConfigurationAnchor),
            "targeted_table_answer" => Some(Self::TargetedTableAnswer),
            "update_procedure_sequence" => Some(Self::UpdateProcedureSequence),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AnswerKind;

    #[test]
    fn retired_persisted_answer_kind_is_ignored() {
        let usage_json = serde_json::json!({
            "deterministic": true,
            "answer_kind": "ordered_source_slice_identity_fallback",
        });

        assert_eq!(AnswerKind::from_usage_json(&usage_json), None);
    }

    #[test]
    fn missing_or_malformed_persisted_answer_kind_is_ignored() {
        assert_eq!(AnswerKind::from_usage_json(&serde_json::json!({})), None);
        assert_eq!(AnswerKind::from_usage_json(&serde_json::json!({"answer_kind": 7})), None,);
    }
}
