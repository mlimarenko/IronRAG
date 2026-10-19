use super::{
    LazyLock, QueryIR, QuestionIntent, RuntimeMatchedChunk, classify_question_or_ir_intents,
    has_question_intent,
};

static ERROR_CODE_ASSIGNMENT_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(
        r"^\s*([A-Za-z][A-Za-z0-9_.-]{2,160})\s*=\s*((?:-?[0-9]+(?:[.][0-9]+)?\s*[,;]\s*)*-?[0-9]+(?:[.][0-9]+)?)\s*$",
    )
    .case_insensitive(true)
    .multi_line(true)
    .build()
    .expect("error-code assignment regex must compile")
});

static ERROR_CODE_MAPPING_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "the static regex literal is compile-time owned")]
    regex::RegexBuilder::new(r"^\s*(-?[0-9]+(?:[.][0-9]+)?)\s*=\s*(\S[^\r\n]{0,160})$")
        .multi_line(true)
        .build()
        .expect("error-code mapping regex must compile")
});

#[derive(Debug)]
struct ErrorCodeMappingCandidate {
    document_label: String,
    parameter: String,
    codes: Vec<(String, Option<String>)>,
    score: usize,
}

pub(super) fn build_error_code_mapping_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let intents = classify_question_or_ir_intents(question, query_ir);
    if !has_question_intent(&intents, QuestionIntent::ErrorCode) {
        return None;
    }
    let mut candidates =
        chunks.iter().flat_map(error_code_mapping_candidates_from_chunk).collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.parameter.cmp(&right.parameter))
    });
    let candidate = candidates.into_iter().next()?;
    let mut answer = format!("`{}` (`{}`)", candidate.parameter, candidate.document_label);
    for (code, message) in candidate.codes.into_iter().take(12) {
        match message {
            Some(message) if !message.trim().is_empty() => {
                answer.push_str(&format!("\n- `{code}` — {}.", clean_mapping_message(&message)));
            }
            _ => answer.push_str(&format!("\n- `{code}`.")),
        }
    }
    Some(answer)
}

fn error_code_mapping_candidates_from_chunk(
    chunk: &RuntimeMatchedChunk,
) -> Vec<ErrorCodeMappingCandidate> {
    let text = if chunk.source_text.trim().is_empty() {
        chunk.excerpt.as_str()
    } else {
        chunk.source_text.as_str()
    };
    let mappings = ERROR_CODE_MAPPING_REGEX
        .captures_iter(text)
        .filter_map(|capture| {
            let code = capture.get(1)?.as_str().trim().to_string();
            let message = capture.get(2)?.as_str().trim().to_string();
            Some((code, message))
        })
        .collect::<std::collections::HashMap<_, _>>();
    ERROR_CODE_ASSIGNMENT_REGEX
        .captures_iter(text)
        .filter_map(|capture| {
            let parameter = capture.get(1)?.as_str().trim().to_string();
            let codes = parse_error_code_list(capture.get(2)?.as_str());
            if codes.len() < 2 {
                return None;
            }
            let mapped_count = codes.iter().filter(|code| mappings.contains_key(*code)).count();
            if mapped_count == 0 && mappings.len() < 2 {
                return None;
            }
            let codes = codes
                .into_iter()
                .map(|code| {
                    let message = mappings.get(&code).cloned();
                    (code, message)
                })
                .collect::<Vec<_>>();
            let score = mapped_count * 100 + codes.len() * 10;
            Some(ErrorCodeMappingCandidate {
                document_label: chunk.document_label.clone(),
                parameter,
                codes,
                score,
            })
        })
        .collect()
}

fn parse_error_code_list(value: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    value
        .split([',', ';', ' ', '\t'])
        .filter_map(|part| {
            let code = part.trim();
            if code.is_empty() || !seen.insert(code.to_string()) {
                return None;
            }
            Some(code.to_string())
        })
        .collect()
}

fn clean_mapping_message(value: &str) -> String {
    value.trim().trim_end_matches(['.', ';', ',']).to_string()
}
