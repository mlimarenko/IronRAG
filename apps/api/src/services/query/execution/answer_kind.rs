#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnswerKind {
    DeterministicGroundedAnswer,
    ExactVersionChangeSummary,
    MissingExplicitDocument,
    OrderedSourceSlice,
    OrderedSourceSliceIdentityFallback,
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
            Self::OrderedSourceSliceIdentityFallback => "ordered_source_slice_identity_fallback",
            Self::SetupConfigurationAnchor => "setup_configuration_anchor",
            Self::TargetedTableAnswer => "targeted_table_answer",
            Self::UpdateProcedureSequence => "update_procedure_sequence",
        }
    }

    pub(super) fn from_usage_json(usage_json: &serde_json::Value) -> Option<Self> {
        match usage_json.get("answer_kind").and_then(serde_json::Value::as_str)? {
            "deterministic_grounded_answer" => Some(Self::DeterministicGroundedAnswer),
            "exact_version_change_summary" => Some(Self::ExactVersionChangeSummary),
            "missing_explicit_document" => Some(Self::MissingExplicitDocument),
            "ordered_source_slice" => Some(Self::OrderedSourceSlice),
            "ordered_source_slice_identity_fallback" => {
                Some(Self::OrderedSourceSliceIdentityFallback)
            }
            "setup_configuration_anchor" => Some(Self::SetupConfigurationAnchor),
            "targeted_table_answer" => Some(Self::TargetedTableAnswer),
            "update_procedure_sequence" => Some(Self::UpdateProcedureSequence),
            _ => None,
        }
    }
}
