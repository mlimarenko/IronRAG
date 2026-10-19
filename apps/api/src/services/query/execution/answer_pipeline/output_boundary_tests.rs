use super::*;

#[test]
fn strips_trailing_interrogative_meta_paragraph_after_numbered_inventory() {
    let answer = "1. Alpha item\n2. Beta item\n\nMore detail?";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
        Some("1. Alpha item\n2. Beta item")
    );
}

#[test]
fn strips_meta_paragraph_after_multiline_inventory_item() {
    let answer =
        "1. Alpha item\n   Source: alpha.md\n2. Beta item\n   Source: beta.md\n\nMore detail?";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
        Some("1. Alpha item\n   Source: alpha.md\n2. Beta item\n   Source: beta.md")
    );
}

#[test]
fn strips_short_meta_paragraph_when_requested_inventory_count_is_satisfied() {
    let answer = "1. Alpha item\n2. Beta item\n\nOverall theme: Alpha and Beta changes.";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, Some(2)).as_deref(),
        Some("1. Alpha item\n2. Beta item")
    );
}

#[test]
fn strips_trailing_interrogative_meta_paragraph_after_bulleted_inventory() {
    let answer = "- Alpha item\n- Beta item\n\nContinue？";

    assert_eq!(
        strip_trailing_inventory_meta_paragraph(answer, None).as_deref(),
        Some("- Alpha item\n- Beta item")
    );
}

#[test]
fn preserves_untyped_trailing_prose_in_non_latin_script() {
    let answer = "Сначала проверьте журнал обработки и исходную запись.\n\nЕсли хотите, я могу помочь найти исходную операцию.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_untyped_trailing_prose_in_latin_script() {
    let answer = "Validate the duplicate record before changing it.\n\nLet me know if you'd like a step-by-step checklist.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_substantive_russian_conditional_remediation() {
    let answer = "Сначала проверьте журнал обработки.\n\nЕсли ошибка повторяется, перезапустите сервисный модуль и снова проверьте журнал.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_substantive_russian_capability_statement() {
    let answer = "Проверка завершена.\n\nЯ могу подтвердить два изменения: новый формат и проверку входных данных.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_substantive_english_conditional_remediation() {
    let answer = "Validate the duplicate record.\n\nIf the error persists, restart the service and check the log again.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_trailing_coverage_limit_statement() {
    let answer = "1. Alpha item\n2. Beta item\n\nOnly the first two matching items were grounded.";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_non_inventory_question_answer() {
    let answer = "The selected evidence contains two matching items. Which one is relevant?";

    assert!(strip_trailing_inventory_meta_paragraph(answer, None).is_none());
}

#[test]
fn preserves_trailing_technical_paragraph() {
    let answer = "1. Alpha item\n2. Beta item\n\nCheck `alpha --status`?";

    assert!(strip_trailing_inventory_meta_paragraph(answer, Some(2)).is_none());
}
