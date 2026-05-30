pub(crate) const EFFECTIVE_QUERY_SCOPE_PREFIX: &str = "scope:";
pub(crate) const EFFECTIVE_QUERY_QUESTION_PREFIX: &str = "question:";

pub(crate) fn current_question_segment(query_text: &str) -> &str {
    structured_current_question_segment(query_text).unwrap_or_else(|| query_text.trim())
}

pub(crate) fn structured_current_question_segment(query_text: &str) -> Option<&str> {
    let trimmed = query_text.trim();
    if !trimmed
        .lines()
        .find(|line| !line.trim().is_empty())
        .is_some_and(|line| line.starts_with(EFFECTIVE_QUERY_SCOPE_PREFIX))
    {
        return None;
    }

    question_line_start(trimmed)
        .map(|question_start| &trimmed[question_start + EFFECTIVE_QUERY_QUESTION_PREFIX.len()..])
        .map(str::trim)
        .filter(|current_question| !current_question.is_empty())
}

fn question_line_start(query_text: &str) -> Option<usize> {
    query_text
        .match_indices(EFFECTIVE_QUERY_QUESTION_PREFIX)
        .filter_map(|(index, _)| {
            (index == 0 || query_text.as_bytes().get(index.wrapping_sub(1)) == Some(&b'\n'))
                .then_some(index)
        })
        .last()
}

#[cfg(test)]
mod tests {
    use super::{current_question_segment, structured_current_question_segment};

    #[test]
    fn extracts_current_question_from_effective_query_text() {
        let query = "scope: prior answer\nentities: Alpha, Beta\nquestion: Alpha setup";

        assert_eq!(structured_current_question_segment(query), Some("Alpha setup"));
        assert_eq!(current_question_segment(query), "Alpha setup");
    }

    #[test]
    fn extracts_current_question_from_crlf_effective_query_text() {
        let query = "scope: prior answer\r\nentities: Alpha, Beta\r\nquestion: Alpha setup";

        assert_eq!(structured_current_question_segment(query), Some("Alpha setup"));
        assert_eq!(current_question_segment(query), "Alpha setup");
    }

    #[test]
    fn ignores_nested_scope_markers_after_non_scope_first_line() {
        let query = "How do I configure Alpha?\nscope: quoted prior block\nquestion: Beta";

        assert_eq!(structured_current_question_segment(query), None);
        assert_eq!(current_question_segment(query), query);
    }

    #[test]
    fn leaves_plain_query_text_intact() {
        let query = "How do I configure Alpha?";

        assert_eq!(structured_current_question_segment(query), None);
        assert_eq!(current_question_segment(query), query);
    }
}
