use super::{
    BTreeSet, RuntimeMatchedChunk, SetupConfigurationParameterRow,
    chunk_is_setup_focus_command_path_anchor, command_token_values, extract_explicit_path_literals,
    line_has_command_signal, normalized_alnum_token_sequence, normalized_alnum_tokens,
    shellish_token_has_executable_name_shape, shellish_token_is_path_command_start,
    split_dense_procedure_line, strip_leading_order_marker, structural_command_head_token,
    token_sequence_exact_or_contains_tokens, trim_command_boundary_token_decorations,
};

pub(super) fn setup_configuration_label_focus_score(
    subject_label_sequences: &[Vec<String>],
    chunk: &RuntimeMatchedChunk,
) -> usize {
    if subject_label_sequences.is_empty() {
        return 0;
    }
    let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 2);
    subject_label_sequences
        .iter()
        .filter(|subject_sequence| {
            token_sequence_exact_or_contains_tokens(&label_sequence, subject_sequence)
        })
        .map(Vec::len)
        .sum()
}

pub(super) fn extract_configuration_path_literals(text: &str, limit: usize) -> Vec<String> {
    extract_explicit_path_literals(text, limit.saturating_mul(2).max(4))
        .into_iter()
        .filter(|path| {
            let lowered = path.to_ascii_lowercase();
            [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
                .iter()
                .any(|extension| lowered.ends_with(extension))
        })
        .take(limit)
        .collect()
}

pub(super) fn extract_configuration_section_literals(text: &str, limit: usize) -> Vec<String> {
    let mut sections = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(start) = rest.find('[') {
            rest = &rest[start + 1..];
            let Some(end) = rest.find(']') else {
                break;
            };
            let section = rest[..end].trim();
            rest = &rest[end + 1..];
            if !configuration_section_literal_is_plausible(section) {
                continue;
            }
            let value = section.to_string();
            if seen.insert(value.to_lowercase()) {
                sections.push(value);
                if sections.len() >= limit {
                    return sections;
                }
            }
        }
    }
    sections
}

fn configuration_section_literal_is_plausible(section: &str) -> bool {
    let section = section.trim();
    if section.is_empty() {
        return false;
    }
    let mut has_alpha = false;
    let mut has_separator = false;
    let mut char_count = 0usize;
    for ch in section.chars() {
        char_count += 1;
        if ch.is_alphabetic() {
            has_alpha = true;
        } else if matches!(ch, '_' | '-' | '.') {
            has_separator = true;
        } else if !ch.is_alphanumeric() {
            return false;
        }
    }
    has_alpha && (has_separator || char_count >= 3)
}

pub(super) fn extract_setup_configuration_command_literals(
    text: &str,
    limit: usize,
) -> Vec<String> {
    let mut commands = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines().flat_map(split_dense_procedure_line) {
        for command in setup_configuration_command_literal_candidates(&line) {
            let key = setup_configuration_literal_key(&command);
            if seen.insert(key) {
                commands.push(command);
                if commands.len() >= limit {
                    return commands;
                }
            }
        }
    }
    commands
}

fn setup_configuration_command_literal_candidates(line: &str) -> Vec<String> {
    if setup_configuration_text_starts_with_artifact(line) {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let command = strip_leading_order_marker(line)
        .trim()
        .trim_end_matches(['.', ',', ';'])
        .trim()
        .to_string();
    if setup_configuration_command_literal_is_usable(&command) {
        candidates.push(command);
    }
    for embedded in setup_configuration_embedded_command_literals(line) {
        if setup_configuration_command_literal_is_usable(&embedded) {
            candidates.push(embedded);
        }
    }
    candidates
}

fn setup_configuration_text_starts_with_artifact(text: &str) -> bool {
    let Some(head) = strip_leading_order_marker(text)
        .split_whitespace()
        .next()
        .map(trim_command_boundary_token_decorations)
    else {
        return false;
    };
    head.contains("://") || shellish_token_is_path_command_start(head)
}

fn setup_configuration_embedded_command_literals(line: &str) -> Vec<String> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 3 {
        return Vec::new();
    }
    let mut commands = Vec::new();
    for start in 1..tokens.len() {
        let candidate = tokens[start..].join(" ");
        if candidate.split_whitespace().count() < 2 || !line_has_command_signal(&candidate) {
            continue;
        }
        for segment in split_dense_procedure_line(&candidate) {
            let segment = strip_leading_order_marker(&segment)
                .trim()
                .trim_end_matches(['.', ',', ';'])
                .trim()
                .to_string();
            if setup_configuration_command_literal_is_usable(&segment) {
                commands.push(segment);
            }
        }
        if !commands.is_empty() {
            break;
        }
    }
    commands
}

fn setup_configuration_command_literal_is_usable(command: &str) -> bool {
    !command.is_empty()
        && !setup_configuration_command_is_table_row(command)
        && !setup_configuration_command_is_commented_fragment(command)
        && !setup_configuration_command_is_standalone_path(command)
        && !setup_configuration_command_is_standalone_assignment(command)
        && setup_configuration_command_has_invocable_structural_head(command)
        && line_has_command_signal(command)
}

fn setup_configuration_command_has_invocable_structural_head(command: &str) -> bool {
    let tokens = command_token_values(strip_leading_order_marker(command));
    let Some(head) = tokens.first().map(String::as_str) else {
        return false;
    };
    shellish_token_has_executable_name_shape(head)
        && !shellish_token_is_path_command_start(head)
        && tokens.iter().skip(1).any(|token| {
            token.starts_with('-')
                || token.contains('=')
                || token.contains("://")
                || token.contains('|')
        })
}

fn setup_configuration_command_is_table_row(command: &str) -> bool {
    command.matches('|').count() >= 2
}

fn setup_configuration_command_is_commented_fragment(command: &str) -> bool {
    let trimmed = command.trim_start();
    matches!(trimmed.chars().next(), Some(';' | '#')) && trimmed.contains(['=', ':', '['])
}

fn setup_configuration_command_is_standalone_path(command: &str) -> bool {
    let trimmed = command.trim().trim_matches('`').trim();
    if trimmed.split_whitespace().count() != 1 {
        return false;
    }
    extract_configuration_path_literals(trimmed, 1).into_iter().any(|path| path == trimmed)
}

fn setup_configuration_command_is_standalone_assignment(command: &str) -> bool {
    let Some((left, right)) = command.split_once('=').or_else(|| command.split_once(':')) else {
        return false;
    };
    let left = left.trim().trim_matches(|ch| matches!(ch, '"' | '\'' | '`'));
    let right = right.trim();
    if left.is_empty() || right.is_empty() || left.split_whitespace().count() != 1 {
        return false;
    }
    left.chars().any(char::is_alphabetic)
        && left.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

pub(super) fn filter_setup_configuration_parameters(
    parameters: Vec<String>,
    packages: &[String],
    reconfigure_packages: &[String],
    paths: &[String],
    sections: &[String],
) -> Vec<String> {
    let blocked = packages
        .iter()
        .chain(reconfigure_packages)
        .chain(paths)
        .chain(sections)
        .map(|value| setup_configuration_literal_key(value))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let command_heads = reconfigure_packages
        .iter()
        .filter_map(|value| setup_configuration_command_head_key(value))
        .collect::<BTreeSet<_>>();
    parameters
        .into_iter()
        .filter(|parameter| {
            let key = setup_configuration_literal_key(parameter);
            !key.is_empty() && !blocked.contains(&key) && !command_heads.contains(&key)
        })
        .collect()
}

fn setup_configuration_command_head_key(command: &str) -> Option<String> {
    let tokens = command_token_values(strip_leading_order_marker(command));
    let head = structural_command_head_token(&tokens)?;
    let key = setup_configuration_literal_key(head);
    (!key.is_empty()).then_some(key)
}

pub(super) fn extract_setup_configuration_parameter_rows(
    text: &str,
    limit: usize,
) -> Vec<SetupConfigurationParameterRow> {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let Some(row) = parse_setup_configuration_parameter_row(line)
            .or_else(|| parse_setup_configuration_plain_parameter_row(line))
        else {
            continue;
        };
        if seen.insert(setup_configuration_literal_key(&row.name)) {
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
    }
    rows
}

fn parse_setup_configuration_parameter_row(line: &str) -> Option<SetupConfigurationParameterRow> {
    if !line.contains('|') || !line.contains(':') {
        return None;
    }
    let cells = line
        .split('|')
        .filter_map(|cell| {
            let (raw_key, raw_value) = cell.split_once(':')?;
            let key = setup_configuration_table_key(raw_key);
            let value = clean_setup_configuration_row_value(raw_value);
            (!key.is_empty() && !value.is_empty()).then_some((key, value))
        })
        .collect::<Vec<_>>();
    if cells.len() < 2 {
        return None;
    }
    let name_index = cells
        .iter()
        .position(|(_, value)| setup_configuration_row_value_is_parameter_identifier(value))
        .or_else(|| {
            cells
                .iter()
                .position(|(_, value)| setup_configuration_row_value_can_name_parameter(value))
        })?;
    let name = cells[name_index].1.clone();
    let positional_assignment_value =
        cells.iter().enumerate().rev().find_map(|(index, (_, value))| {
            (index != name_index && setup_configuration_default_value_is_assignment_scalar(value))
                .then(|| value.clone())
        });
    let details = cells
        .into_iter()
        .enumerate()
        .filter_map(|(index, (key, value))| {
            (index != name_index && !setup_configuration_literal_key(&value).is_empty())
                .then_some((key, value))
        })
        .take(4)
        .collect::<Vec<_>>();
    let key = setup_configuration_literal_key(&name);
    if key.is_empty() || details.is_empty() {
        return None;
    }
    Some(SetupConfigurationParameterRow { name, details, positional_assignment_value })
}

fn parse_setup_configuration_plain_parameter_row(
    line: &str,
) -> Option<SetupConfigurationParameterRow> {
    if line.matches('|').count() < 3 || line.contains(':') {
        return None;
    }
    let cells = line
        .split('|')
        .map(clean_setup_configuration_row_value)
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    if cells.len() < 4 || cells.iter().all(|cell| cell.chars().all(|ch| matches!(ch, '-'))) {
        return None;
    }
    let name = cells.first()?.clone();
    if !setup_configuration_row_value_is_parameter_identifier(&name) {
        return None;
    }
    let default = cells.last()?.clone();
    if !setup_configuration_default_value_is_assignment_scalar(&default) {
        return None;
    }
    Some(SetupConfigurationParameterRow {
        name,
        details: Vec::new(),
        positional_assignment_value: Some(default),
    })
}

pub(super) fn setup_configuration_default_value_is_assignment_scalar(value: &str) -> bool {
    let trimmed = value.trim().trim_matches('`').trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 || trimmed.contains("://") {
        return false;
    }
    if matches!(trimmed, "true" | "false") {
        return true;
    }
    let numeric = trimmed.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_'));
    numeric && trimmed.chars().any(|ch| ch.is_ascii_digit())
}

fn setup_configuration_row_value_can_name_parameter(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.chars().count() < 2 || trimmed.chars().count() > 120 {
        return false;
    }
    if trimmed.split_whitespace().count() > 8 {
        return false;
    }
    trimmed.chars().any(|ch| ch.is_alphabetic() || ch == '_' || ch == '.' || ch == '-')
}

fn setup_configuration_row_value_is_parameter_identifier(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.chars().count() < 2
        || trimmed.chars().count() > 80
        || trimmed.chars().any(char::is_whitespace)
        || !trimmed.chars().any(char::is_alphabetic)
    {
        return false;
    }
    trimmed.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn setup_configuration_table_key(value: &str) -> String {
    value.trim().trim_matches(|ch: char| !ch.is_alphanumeric() && ch != ' ').to_string()
}

fn clean_setup_configuration_row_value(value: &str) -> String {
    value.trim().trim_matches('|').trim().trim_matches('`').trim().to_string()
}

pub(super) fn setup_configuration_literal_key(value: &str) -> String {
    normalized_alnum_tokens(value, 1).into_iter().collect::<Vec<_>>().join(" ")
}

/// Bounded literal sets extracted from a setup-configuration chunk.
pub(super) struct SetupConfigurationLiterals<'a> {
    pub(super) packages: &'a [String],
    pub(super) reconfigure_packages: &'a [String],
    pub(super) paths: &'a [String],
    pub(super) sections: &'a [String],
    pub(super) parameters: &'a [String],
    pub(super) parameter_rows: &'a [SetupConfigurationParameterRow],
}

pub(super) fn setup_configuration_variant_score(
    literals: SetupConfigurationLiterals<'_>,
    chunk: &RuntimeMatchedChunk,
) -> usize {
    literals
        .packages
        .len()
        .saturating_mul(24)
        .saturating_add(literals.reconfigure_packages.len().saturating_mul(18))
        .saturating_add(literals.paths.len().saturating_mul(24))
        .saturating_add(literals.sections.len().saturating_mul(16))
        .saturating_add(literals.parameters.len().saturating_mul(3))
        .saturating_add(literals.parameter_rows.len().saturating_mul(8))
        .saturating_add((chunk_is_setup_focus_command_path_anchor(chunk) as usize) * 32)
}

pub(super) fn push_unique_values(target: &mut Vec<String>, values: Vec<String>, limit: usize) {
    let mut seen = target.iter().map(|value| value.to_lowercase()).collect::<BTreeSet<_>>();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            target.push(trimmed.to_string());
            if target.len() >= limit {
                break;
            }
        }
    }
}

pub(super) fn push_unique_parameter_rows(
    target: &mut Vec<SetupConfigurationParameterRow>,
    values: Vec<SetupConfigurationParameterRow>,
    limit: usize,
) {
    let mut seen = target
        .iter()
        .map(|row| setup_configuration_literal_key(&row.name))
        .collect::<BTreeSet<_>>();
    for row in values {
        let key = setup_configuration_literal_key(&row.name);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        target.push(row);
        if target.len() >= limit {
            break;
        }
    }
}
