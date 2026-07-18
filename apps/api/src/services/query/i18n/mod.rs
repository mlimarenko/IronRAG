use crate::domains::query_ir::QueryLanguage;

#[derive(Debug, Clone, Copy)]
pub(crate) struct DeterministicAnswerLabelSet {
    pub(crate) variants: &'static str,
    pub(crate) source: &'static str,
    pub(crate) package: &'static str,
    pub(crate) reconfigure: &'static str,
    pub(crate) path: &'static str,
    pub(crate) section: &'static str,
    pub(crate) parameter: &'static str,
    pub(crate) parameter_details: &'static str,
    pub(crate) update_sequence: &'static str,
}

/// Localized copy used by deterministic, provider-independent query paths.
///
/// Keeping these messages in the dedicated catalog prevents execution and
/// verification policy from growing language-specific branches or embedding
/// user-facing prose alongside control flow.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DeterministicQueryMessageSet {
    pub(crate) options_heading: &'static str,
    pub(crate) clarify_source: &'static str,
    pub(crate) strict_verification_failure: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct GroundedRepairMessageSet {
    pub(crate) partial_answer_notice: &'static str,
    pub(crate) unverified_answer_notice: &'static str,
}

pub(crate) const EN_DETERMINISTIC_ANSWER_LABELS: DeterministicAnswerLabelSet =
    DeterministicAnswerLabelSet {
        variants: "Setup variants",
        source: "Source",
        package: "Item",
        reconfigure: "Command",
        path: "File",
        section: "Section",
        parameter: "Parameter",
        parameter_details: "Parameter details",
        update_sequence: "Steps",
    };

pub(crate) const RU_DETERMINISTIC_ANSWER_LABELS: DeterministicAnswerLabelSet =
    DeterministicAnswerLabelSet {
        variants: "Варианты настройки",
        source: "Источник",
        package: "Элемент",
        reconfigure: "Команда",
        path: "Файл",
        section: "Секция",
        parameter: "Параметр",
        parameter_details: "Детали параметров",
        update_sequence: "Шаги",
    };

pub(crate) const EN_DETERMINISTIC_QUERY_MESSAGES: DeterministicQueryMessageSet =
    DeterministicQueryMessageSet {
        options_heading: "Options:",
        clarify_source: "Please choose one of the available sources.",
        strict_verification_failure: "I couldn't produce an answer fully supported by the selected sources.",
    };

pub(crate) const RU_DETERMINISTIC_QUERY_MESSAGES: DeterministicQueryMessageSet =
    DeterministicQueryMessageSet {
        options_heading: "Варианты:",
        clarify_source: "Выберите один из доступных источников.",
        strict_verification_failure: "Не удалось сформировать ответ, полностью подтверждённый выбранными источниками.",
    };

pub(crate) const EN_GROUNDED_REPAIR_MESSAGES: GroundedRepairMessageSet = GroundedRepairMessageSet {
    partial_answer_notice: "The answer below contains only the source-verified portion. One additional focused retrieval attempt did not fully cover the request.",
    unverified_answer_notice: "Below is the last completed system answer. One additional retrieval attempt did not close the gap, so completeness and verification are not guaranteed.",
};

pub(crate) const RU_GROUNDED_REPAIR_MESSAGES: GroundedRepairMessageSet = GroundedRepairMessageSet {
    partial_answer_notice: "Ниже приведена только подтверждённая источниками часть ответа. Полностью покрыть запрос после одной дополнительной попытки поиска не удалось.",
    unverified_answer_notice: "Ниже приведён последний завершённый системой ответ. Дополнительная попытка поиска не устранила пробел, поэтому полнота и проверка ответа не гарантированы.",
};

pub(crate) fn deterministic_answer_labels(language: QueryLanguage) -> DeterministicAnswerLabelSet {
    match language {
        QueryLanguage::Ru => RU_DETERMINISTIC_ANSWER_LABELS,
        QueryLanguage::En | QueryLanguage::Auto => EN_DETERMINISTIC_ANSWER_LABELS,
    }
}

pub(crate) fn deterministic_query_messages(
    language: QueryLanguage,
) -> DeterministicQueryMessageSet {
    match language {
        QueryLanguage::Ru => RU_DETERMINISTIC_QUERY_MESSAGES,
        QueryLanguage::En | QueryLanguage::Auto => EN_DETERMINISTIC_QUERY_MESSAGES,
    }
}

pub(crate) fn grounded_repair_messages(language: QueryLanguage) -> GroundedRepairMessageSet {
    match language {
        QueryLanguage::Ru => RU_GROUNDED_REPAIR_MESSAGES,
        QueryLanguage::En | QueryLanguage::Auto => EN_GROUNDED_REPAIR_MESSAGES,
    }
}

#[cfg(test)]
mod tests {
    use super::grounded_repair_messages;
    use crate::domains::query_ir::QueryLanguage;

    #[test]
    fn repair_catalog_uses_only_the_explicit_typed_language() {
        assert_eq!(
            grounded_repair_messages(QueryLanguage::Auto).partial_answer_notice,
            grounded_repair_messages(QueryLanguage::En).partial_answer_notice,
        );
        assert_ne!(
            grounded_repair_messages(QueryLanguage::Ru).partial_answer_notice,
            grounded_repair_messages(QueryLanguage::En).partial_answer_notice,
        );
    }
}
