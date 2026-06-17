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
    pub(crate) evidence: &'static str,
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
        evidence: "Evidence fragments",
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
        evidence: "Подтверждающие фрагменты",
    };

pub(crate) fn deterministic_answer_labels(language: QueryLanguage) -> DeterministicAnswerLabelSet {
    match language {
        QueryLanguage::Ru => RU_DETERMINISTIC_ANSWER_LABELS,
        QueryLanguage::En | QueryLanguage::Auto => EN_DETERMINISTIC_ANSWER_LABELS,
    }
}
