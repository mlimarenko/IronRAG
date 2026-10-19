use super::{
    command_token_values, extract_semver_like_version, procedure_line_has_list_marker,
    shellish_line_has_source_syntax, shellish_token_file_artifact_name,
    shellish_token_has_artifact_preparation_signal, shellish_token_has_executable_name_shape,
    shellish_token_is_command_argument_signal, shellish_token_is_invocable_head,
    shellish_token_is_local_artifact, shellish_token_is_path_command_start,
    shellish_tokens_start_command, split_concatenated_local_artifact_token,
    strip_leading_numeric_order_marker, token_has_local_command_artifact,
    trim_command_boundary_token_decorations, update_procedure_block_has_setup_script_signature,
    update_procedure_normalized_step,
};

#[derive(Debug, Clone)]
pub(super) struct UpdateProcedureLine {
    pub(super) text: String,
    pub(super) has_order_marker: bool,
    pub(super) has_version: bool,
    pub(super) has_command: bool,
}

pub(super) fn update_procedure_step_from_line(line: &UpdateProcedureLine) -> String {
    if !line.has_command || line_has_command_signal(&line.text) {
        return line.text.clone();
    }
    let command = update_procedure_normalized_step(&line.text);
    format!("`{command}`")
}

pub(super) fn update_procedure_line_blocks(text: &str) -> Vec<Vec<UpdateProcedureLine>> {
    let mut blocks = Vec::new();
    let mut state = UpdateProcedureBlockBuilder::default();
    for raw_line in text.lines() {
        let segments = split_dense_procedure_segments(raw_line);
        if segments.is_empty() {
            state.push_segment(
                DenseProcedureSegment {
                    text: raw_line.trim().to_string(),
                    explicit_command_context: false,
                },
                &mut blocks,
            );
            continue;
        }
        for segment in segments {
            state.push_segment(segment, &mut blocks);
        }
    }
    state.finish(&mut blocks);
    blocks
}

#[derive(Default)]
struct UpdateProcedureBlockBuilder {
    current: Vec<UpdateProcedureLine>,
    preceding_line_opens_command_context: bool,
}

impl UpdateProcedureBlockBuilder {
    fn push_segment(
        &mut self,
        segment: DenseProcedureSegment,
        blocks: &mut Vec<Vec<UpdateProcedureLine>>,
    ) {
        let trimmed = segment.text.trim();
        if trimmed.is_empty() || line_is_markdown_atx_heading(trimmed) {
            self.finish(blocks);
            return;
        }
        let has_order_marker = line_has_order_marker(trimmed);
        let line = trimmed.trim_matches(['-', '*', '•', ' ']).trim();
        let has_contextual_command = !has_order_marker
            && (segment.explicit_command_context || self.preceding_line_opens_command_context)
            && contextual_procedure_command_shape(line);
        if line.chars().count() < 8 && !line_has_command_signal(line) && !has_contextual_command {
            return;
        }
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let has_version =
            !line_looks_data_record(&normalized) && update_procedure_line_has_version(&normalized);
        let has_command = line_has_command_signal(&normalized) || has_contextual_command;
        if update_procedure_line_is_section_heading(
            &normalized,
            has_order_marker,
            has_version,
            has_command,
        ) && update_procedure_block_has_setup_script_signature(&self.current)
        {
            self.finish(blocks);
        }
        self.current.push(UpdateProcedureLine {
            has_version,
            has_order_marker,
            has_command,
            text: normalized,
        });
        self.preceding_line_opens_command_context = !has_command
            && self.current.last().is_some_and(|line| {
                line.text.ends_with(':') && line.text.split_whitespace().count() <= 16
            });
    }

    fn finish(&mut self, blocks: &mut Vec<Vec<UpdateProcedureLine>>) {
        if !self.current.is_empty() {
            blocks.push(std::mem::take(&mut self.current));
        }
        self.preceding_line_opens_command_context = false;
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DenseProcedureSegment {
    text: String,
    explicit_command_context: bool,
}

fn line_is_markdown_atx_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    let marker_len = trimmed.chars().take_while(|ch| *ch == '#').count();
    (1..=6).contains(&marker_len) && trimmed.chars().nth(marker_len).is_none_or(char::is_whitespace)
}

fn update_procedure_line_is_section_heading(
    line: &str,
    has_order_marker: bool,
    has_version: bool,
    has_command: bool,
) -> bool {
    !has_order_marker
        && !has_version
        && !has_command
        && line.ends_with(':')
        && line.chars().count() <= 120
        && line.split_whitespace().count() <= 14
}

pub(super) fn split_dense_procedure_line(line: &str) -> Vec<String> {
    split_dense_procedure_segments(line).into_iter().map(|segment| segment.text).collect()
}

fn split_dense_procedure_segments(line: &str) -> Vec<DenseProcedureSegment> {
    let mut segments = Vec::<DenseProcedureSegment>::new();
    let mut current = Vec::<String>::new();
    let mut current_has_explicit_command_context = false;
    let tokens = line
        .split_whitespace()
        .flat_map(|token| {
            split_concatenated_local_artifact_token(token)
                .into_iter()
                .enumerate()
                .map(|(segment_index, token)| (token, segment_index > 0))
        })
        .collect::<Vec<_>>();
    for (token_index, (token, follows_concatenated_artifact_boundary)) in
        tokens.iter().copied().enumerate()
    {
        let next_token = tokens.get(token_index + 1).map(|(token, _)| *token);
        if current_has_explicit_command_context
            && contextual_command_segment_is_complete(&current)
            && !contextual_command_token_continues(&current, token)
        {
            push_dense_procedure_segment(
                &mut segments,
                &mut current,
                current_has_explicit_command_context,
            );
            current_has_explicit_command_context = false;
        }
        let inside_square_delimiter = tokens_have_unclosed_square_delimiter(&current);
        let follows_inline_command_delimiter =
            !inside_square_delimiter && current_ends_with_inline_command_delimiter(&current);
        let is_command_start = !inside_square_delimiter
            && (follows_concatenated_artifact_boundary
                || token_is_inline_command_boundary_start(token, next_token, &current));
        let is_order_start = !inside_square_delimiter && token_is_inline_order_marker(token);
        if (is_command_start || is_order_start) && !current.is_empty() {
            push_dense_procedure_segment(
                &mut segments,
                &mut current,
                current_has_explicit_command_context,
            );
            current_has_explicit_command_context = is_command_start
                && (follows_inline_command_delimiter || follows_concatenated_artifact_boundary);
        }
        current.push(token.to_string());
    }
    if !current.is_empty() {
        push_dense_procedure_segment(
            &mut segments,
            &mut current,
            current_has_explicit_command_context,
        );
    }
    segments
        .into_iter()
        .map(|segment| {
            let text = segment.text.trim_matches(['-', '*', '•', ' ']).trim();
            DenseProcedureSegment {
                text: compact_bracketed_identifier_spacing(text),
                explicit_command_context: segment.explicit_command_context,
            }
        })
        .filter(|segment| !segment.text.is_empty())
        .collect()
}

fn push_dense_procedure_segment(
    segments: &mut Vec<DenseProcedureSegment>,
    current: &mut Vec<String>,
    explicit_command_context: bool,
) {
    if current.is_empty() {
        return;
    }
    segments.push(DenseProcedureSegment { text: current.join(" "), explicit_command_context });
    current.clear();
}

fn tokens_have_unclosed_square_delimiter(tokens: &[String]) -> bool {
    tokens.iter().flat_map(|token| token.chars()).fold(0usize, |depth, ch| match ch {
        '[' => depth.saturating_add(1),
        ']' => depth.saturating_sub(1),
        _ => depth,
    }) > 0
}

fn compact_bracketed_identifier_spacing(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '[' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        let Some(relative_end) = chars[index + 1..].iter().position(|ch| *ch == ']') else {
            output.push(chars[index]);
            index += 1;
            continue;
        };
        let end = index + 1 + relative_end;
        let inner = &chars[index + 1..end];
        let inner_text = inner.iter().collect::<String>();
        let identifier = inner_text.trim();
        let identifier_shaped = identifier.chars().any(char::is_alphanumeric)
            && !identifier.chars().any(char::is_whitespace)
            && identifier.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-'));
        if !identifier_shaped {
            output.extend(chars[index..=end].iter());
            index = end + 1;
            continue;
        }
        output.push('[');
        output.push_str(identifier);
        output.push(']');
        index = end + 1;
    }
    output
}

fn token_is_inline_command_boundary_start(
    token: &str,
    next_token: Option<&str>,
    current: &[String],
) -> bool {
    let cleaned = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';'));
    let normalized = trim_command_boundary_token_decorations(cleaned).to_ascii_lowercase();
    if !current.is_empty()
        && !current_ends_with_inline_command_delimiter(current)
        && !shellish_token_is_path_command_start(cleaned)
        && current_is_plain_invocation_with_structural_argument(current, &normalized)
    {
        return false;
    }
    if current_command_prepares_local_artifact(current)
        && token_has_local_command_artifact(&normalized)
    {
        return true;
    }
    if shellish_token_is_path_command_start(cleaned) {
        return path_token_starts_inline_command(current, cleaned);
    }
    if invocable_token_starts_inline_command(current, &normalized, next_token) {
        return true;
    }
    false
}

fn path_token_starts_inline_command(current: &[String], token: &str) -> bool {
    current.is_empty()
        || current_ends_with_formal_shell_delimiter(current)
        || current_ends_with_directory_shaped_path_before_executable_artifact(current, token)
}

fn invocable_token_starts_inline_command(
    current: &[String],
    normalized: &str,
    next_token: Option<&str>,
) -> bool {
    if current_ends_with_inline_command_delimiter(current)
        && shellish_token_is_invocable_head(normalized)
    {
        return true;
    }
    if current_starts_with_command(current)
        && current_command_has_external_materialization(current)
        && shellish_token_is_invocable_head(normalized)
    {
        return true;
    }
    if current_command_expects_structural_value(current)
        && token_has_local_command_artifact(normalized)
    {
        return false;
    }
    !current.is_empty()
        && shellish_token_is_invocable_head(normalized)
        && shellish_token_has_executable_name_shape(normalized)
        && next_token.is_some_and(|token| {
            let next = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
            shellish_token_is_command_argument_signal(&next)
        })
}

fn contextual_command_segment_is_complete(current: &[String]) -> bool {
    let tokens = current
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    shellish_tokens_start_command(&tokens)
        || tokens.iter().any(|token| {
            shellish_token_has_executable_name_shape(token)
                || shellish_token_is_path_command_start(token)
        })
}

fn contextual_command_token_continues(current: &[String], token: &str) -> bool {
    let token = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
    if shellish_token_is_command_argument_signal(&token)
        || shellish_token_is_local_artifact(&token)
        || current_command_expects_structural_value(current)
    {
        return true;
    }
    let normalized = current
        .iter()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if normalized.len() < 2 {
        return true;
    }
    let Some(last) = normalized.last() else {
        return false;
    };
    !shellish_token_is_path_command_start(last)
        && shellish_token_has_executable_name_shape(last)
        && normalized.iter().take(normalized.len().saturating_sub(1)).all(|value| {
            shellish_token_is_invocable_head(value)
                && !shellish_token_has_executable_name_shape(value)
        })
}

fn current_ends_with_directory_shaped_path_before_executable_artifact(
    current: &[String],
    token: &str,
) -> bool {
    if shellish_token_file_artifact_name(token).is_none() {
        return false;
    }
    let Some(previous) = current
        .last()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
    else {
        return false;
    };
    shellish_token_is_path_command_start(&previous)
        && shellish_token_file_artifact_name(&previous).is_none()
        && current_starts_with_command(current)
}

fn contextual_procedure_command_shape(line: &str) -> bool {
    let tokens = command_token_values(strip_leading_order_marker(line));
    let Some(head) = tokens.first() else {
        return false;
    };
    tokens.len() >= 2
        && tokens.len() <= 12
        && shellish_token_is_invocable_head(head)
        && (shellish_tokens_start_command(&tokens)
            || tokens.iter().all(|token| shellish_token_is_invocable_head(token)))
}

fn current_is_plain_invocation_with_structural_argument(current: &[String], token: &str) -> bool {
    let Some(head) = current
        .first()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
    else {
        return false;
    };
    !shellish_token_is_path_command_start(&head)
        && !shellish_token_has_executable_name_shape(&head)
        && shellish_token_is_invocable_head(&head)
        && shellish_token_is_command_argument_signal(token)
}

fn current_ends_with_inline_command_delimiter(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        token
            .trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ','))
            .ends_with(':')
    })
}

fn current_ends_with_formal_shell_delimiter(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        let token = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')'));
        ["&&", "||", ";", "|", "&"].iter().any(|delimiter| token.ends_with(delimiter))
    })
}

fn current_starts_with_command(tokens: &[String]) -> bool {
    let normalized = tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    shellish_tokens_start_command(&normalized)
}

fn token_is_inline_order_marker(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';'));
    let mut chars = token.chars().peekable();
    let mut digit_count = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        digit_count = digit_count.saturating_add(1);
        chars.next();
    }
    digit_count > 0 && chars.peek().is_some_and(|ch| matches!(ch, '.' | ')')) && {
        chars.next();
        chars.next().is_none()
    }
}

fn current_command_has_external_materialization(tokens: &[String]) -> bool {
    let normalized = tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    if !normalized.iter().any(|token| token.contains("://")) {
        return false;
    }
    normalized.iter().skip(1).any(|token| token_has_local_command_artifact(token))
}

fn current_command_prepares_local_artifact(tokens: &[String]) -> bool {
    let command_tokens = if tokens.first().is_some_and(|token| token_is_inline_order_marker(token))
    {
        &tokens[1..]
    } else {
        tokens
    };
    let normalized = command_tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized.iter().any(|token| shellish_token_has_artifact_preparation_signal(token))
        && normalized.iter().skip(1).any(|token| token_has_local_command_artifact(token))
}

fn current_command_expects_structural_value(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        let token = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
        if let Some((_, value)) = token.split_once('=') {
            return value.is_empty();
        }
        token.starts_with('-') || token.starts_with('+') || token.contains("+")
    })
}

pub(super) fn line_has_command_signal(line: &str) -> bool {
    let trimmed = strip_leading_order_marker(line).trim();
    if compact_configuration_section_header(trimmed) || shellish_line_has_source_syntax(trimmed) {
        return false;
    }
    let code_delimited = trimmed.len() >= 2
        && trimmed.starts_with('`')
        && trimmed.ends_with('`')
        && !trimmed.trim_matches('`').trim().is_empty();
    let command = trimmed.trim_matches('`').trim();
    let tokens = command_token_values(command);
    let Some(head) = tokens.first().map(String::as_str) else {
        return false;
    };
    shellish_tokens_start_command(&tokens)
        || (code_delimited && tokens.len() >= 2 && shellish_token_is_invocable_head(head))
        || (procedure_line_has_list_marker(line)
            && tokens.len() >= 2
            && shellish_token_has_executable_name_shape(head))
}

fn compact_configuration_section_header(line: &str) -> bool {
    let Some(inner) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) else {
        return false;
    };
    !inner.is_empty()
        && !inner.chars().any(char::is_whitespace)
        && inner.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

pub(super) fn line_has_order_marker(line: &str) -> bool {
    procedure_line_has_list_marker(line)
}

pub(super) fn update_procedure_line_has_version(line: &str) -> bool {
    extract_semver_like_version(line).is_some()
        || line.split_whitespace().any(version_token_has_structured_separator)
}

fn version_token_has_structured_separator(token: &str) -> bool {
    let cleaned = token
        .trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')));
    if cleaned.is_empty()
        || !cleaned.chars().any(|ch| ch.is_ascii_digit())
        || !cleaned.chars().any(|ch| matches!(ch, '-' | '_'))
    {
        return false;
    }
    let numeric_groups = cleaned
        .split(['.', '-', '_'])
        .filter(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
        .count();
    numeric_groups >= 2
}

fn line_looks_data_record(line: &str) -> bool {
    let trimmed = strip_leading_order_marker(line).trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('|') || trimmed.starts_with('{') {
        return true;
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.contains(',') {
        return true;
    }
    let colon_index = trimmed.find(':');
    let equals_index = trimmed.find('=');
    let separator_index = match (colon_index, equals_index) {
        (Some(colon), Some(equals)) => Some(colon.min(equals)),
        (Some(colon), None) => Some(colon),
        (None, Some(equals)) => Some(equals),
        (None, None) => None,
    };
    let Some(separator_index) = separator_index else {
        return false;
    };
    let (left, right_with_separator) = trimmed.split_at(separator_index);
    let right = right_with_separator[1..].trim();
    let left = left.trim().trim_matches('"');
    if left.is_empty() || right.is_empty() || left.split_whitespace().count() > 3 {
        return false;
    }
    let right_value = right.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | ')' | ']')
    });
    !right_value.is_empty()
        && right_value.chars().any(|ch| ch.is_ascii_digit())
        && right_value.chars().any(|ch| matches!(ch, '.' | '-' | '_'))
        && right_value.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_' | '+'))
}

pub(super) fn strip_leading_order_marker(line: &str) -> &str {
    strip_leading_numeric_order_marker(line)
}
