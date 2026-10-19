use std::collections::BTreeSet;

use super::rendering::sanitize_field_path_segment;

/// A large header and many operator notes fit without letting comments make a
/// single config document grow without bound.
pub(super) const MAX_PRESERVED_CONFIG_COMMENT_CHARS: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfigSyntax {
    Yaml,
    Toml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConfigCommentRun {
    start_line: usize,
    end_line: usize,
    text: String,
}

/// Collects consecutive full-line comments before the structured value parser
/// gets a chance to discard them. Identical rendered runs are kept once, and
/// the document-wide character budget is applied after deduplication.
pub(super) fn extract_comment_runs(raw_text: &str) -> Vec<ConfigCommentRun> {
    let lines = raw_text.lines().collect::<Vec<_>>();
    let mut runs = Vec::<ConfigCommentRun>::new();
    let mut seen = BTreeSet::<String>::new();
    let mut preserved_chars = 0_usize;
    let mut line_index = 0_usize;

    while line_index < lines.len() && preserved_chars < MAX_PRESERVED_CONFIG_COMMENT_CHARS {
        if !is_full_line_comment(lines[line_index]) {
            line_index += 1;
            continue;
        }

        let start_line = line_index;
        let mut comment_lines = Vec::<&str>::new();
        while line_index < lines.len() && is_full_line_comment(lines[line_index]) {
            comment_lines.push(lines[line_index].trim());
            line_index += 1;
        }
        let text = comment_lines.join("\n");
        if text.is_empty() || !seen.insert(text.clone()) {
            continue;
        }

        let remaining = MAX_PRESERVED_CONFIG_COMMENT_CHARS.saturating_sub(preserved_chars);
        let preserved_text = text.chars().take(remaining).collect::<String>();
        let preserved_count = preserved_text.chars().count();
        if preserved_count == 0 {
            break;
        }
        preserved_chars = preserved_chars.saturating_add(preserved_count);
        runs.push(ConfigCommentRun {
            start_line,
            end_line: line_index.saturating_sub(1),
            text: preserved_text,
        });
    }

    runs
}

/// Returns comment text grouped by the rendered record field it annotates.
/// The nearest non-comment source line supplies only lexical/config-structure
/// evidence; comment wording never affects placement.
pub(super) fn anchor_comment_runs(
    raw_text: &str,
    syntax: ConfigSyntax,
    runs: &[ConfigCommentRun],
    fields_by_record: &[Vec<String>],
) -> Vec<Vec<Vec<String>>> {
    let mut anchored = fields_by_record
        .iter()
        .map(|fields| vec![Vec::<String>::new(); fields.len()])
        .collect::<Vec<_>>();
    let field_count = fields_by_record.iter().map(Vec::len).sum::<usize>();
    if runs.is_empty() || field_count == 0 {
        return anchored;
    }

    let lines = raw_text.lines().collect::<Vec<_>>();
    let first_content_line = first_content_line(&lines, syntax);
    for run in runs {
        if run.end_line < first_content_line {
            if let Some((record_index, field_index)) = first_field_anchor(fields_by_record) {
                anchored[record_index][field_index].push(run.text.clone());
            }
            continue;
        }
        let anchor_line = nearest_content_line(&lines, run, syntax).unwrap_or(run.start_line);
        let evidence = anchor_evidence(&lines, anchor_line, syntax);
        if let Some((record_index, field_index)) =
            best_field_anchor(&evidence, anchor_line, lines.len(), fields_by_record)
        {
            anchored[record_index][field_index].push(run.text.clone());
        }
    }
    anchored
}

/// Renders leading comment runs as ordinary document prose. The comment marker
/// is syntax rather than content, so removing it lets structured preparation
/// reuse the existing paragraph chunk shape.
pub(super) fn extract_document_header_text(
    raw_text: &str,
    syntax: ConfigSyntax,
    runs: &[ConfigCommentRun],
) -> Option<String> {
    let lines = raw_text.lines().collect::<Vec<_>>();
    let first_content_line = first_content_line(&lines, syntax);
    let header_runs = runs
        .iter()
        .filter(|run| run.end_line < first_content_line)
        .filter_map(|run| comment_run_prose(&run.text))
        .collect::<Vec<_>>();
    (!header_runs.is_empty()).then(|| header_runs.join("\n\n"))
}

fn first_content_line(lines: &[&str], syntax: ConfigSyntax) -> usize {
    lines.iter().position(|line| is_anchor_line(line, syntax)).unwrap_or(lines.len())
}

fn first_field_anchor(fields_by_record: &[Vec<String>]) -> Option<(usize, usize)> {
    fields_by_record
        .iter()
        .enumerate()
        .find_map(|(record_index, fields)| (!fields.is_empty()).then_some((record_index, 0)))
}

fn comment_run_prose(text: &str) -> Option<String> {
    let prose = text
        .lines()
        .map(str::trim_start)
        .map(|line| line.strip_prefix('#').unwrap_or(line))
        .map(|line| line.strip_prefix(' ').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n");
    let prose = prose.trim_matches('\n');
    (!prose.trim().is_empty()).then(|| prose.to_string())
}

fn is_full_line_comment(line: &str) -> bool {
    line.trim_start().starts_with('#')
}

fn nearest_content_line(
    lines: &[&str],
    run: &ConfigCommentRun,
    syntax: ConfigSyntax,
) -> Option<usize> {
    let following = (run.end_line.saturating_add(1)..lines.len())
        .find(|index| is_anchor_line(lines[*index], syntax));
    let preceding = (0..run.start_line).rev().find(|index| is_anchor_line(lines[*index], syntax));

    match (following, preceding) {
        (Some(after), Some(before)) => {
            let following_distance = after.saturating_sub(run.end_line);
            let preceding_distance = run.start_line.saturating_sub(before);
            Some(if following_distance <= preceding_distance { after } else { before })
        }
        (Some(after), None) => Some(after),
        (None, Some(before)) => Some(before),
        (None, None) => None,
    }
}

fn is_anchor_line(line: &str, syntax: ConfigSyntax) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() || is_full_line_comment(line) {
        return false;
    }
    !matches!(syntax, ConfigSyntax::Yaml) || !matches!(trimmed, "---" | "...")
}

#[derive(Debug, Default)]
struct AnchorEvidence {
    lexemes: BTreeSet<String>,
    paths: BTreeSet<String>,
}

fn anchor_evidence(lines: &[&str], anchor_line: usize, syntax: ConfigSyntax) -> AnchorEvidence {
    let mut evidence = AnchorEvidence::default();
    if let Some(line) = lines.get(anchor_line) {
        collect_lexemes(line, &mut evidence.lexemes);
    }

    let path = match syntax {
        ConfigSyntax::Yaml => yaml_path_at(lines, anchor_line),
        ConfigSyntax::Toml => toml_path_at(lines, anchor_line),
    };
    if !path.is_empty() {
        for segment in &path {
            evidence.lexemes.insert(segment.clone());
        }
        evidence.paths.insert(path.join("."));
    }
    evidence
}

fn collect_lexemes(line: &str, out: &mut BTreeSet<String>) {
    for candidate in line.split(|ch: char| {
        !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
    }) {
        let lexeme = candidate.trim_matches(['-', '.', ':', '/']);
        if !lexeme.is_empty() {
            out.insert(lexeme.to_string());
        }
    }
}

fn yaml_path_at(lines: &[&str], anchor_line: usize) -> Vec<String> {
    let mut parents = Vec::<(usize, String)>::new();
    let mut current_path = Vec::<String>::new();

    for (line_index, line) in lines.iter().enumerate().take(anchor_line.saturating_add(1)) {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_full_line_comment(line) || matches!(trimmed, "---" | "...") {
            continue;
        }
        let indentation = line.chars().take_while(|ch| ch.is_whitespace()).count();
        let content = line.trim_start().strip_prefix("- ").unwrap_or_else(|| line.trim_start());
        let Some((raw_key, remainder)) = content.split_once(':') else {
            if line_index == anchor_line {
                current_path = parents.iter().map(|(_, key)| key.clone()).collect();
            }
            continue;
        };
        let key = raw_key.trim().trim_matches(['\'', '"']);
        if key.is_empty() {
            continue;
        }
        parents.retain(|(parent_indent, _)| *parent_indent < indentation);
        current_path = parents.iter().map(|(_, key)| key.clone()).collect();
        current_path.push(sanitize_field_path_segment(key));
        if remainder.trim().is_empty() {
            parents.push((indentation, sanitize_field_path_segment(key)));
        }
        if line_index == anchor_line {
            break;
        }
    }

    current_path
}

fn toml_path_at(lines: &[&str], anchor_line: usize) -> Vec<String> {
    let mut table_path = Vec::<String>::new();
    let mut current_path = Vec::<String>::new();

    for (line_index, line) in lines.iter().enumerate().take(anchor_line.saturating_add(1)) {
        let trimmed = line.trim();
        if trimmed.is_empty() || is_full_line_comment(line) {
            continue;
        }
        if let Some(table) = toml_table_path(trimmed) {
            table_path = table;
            if line_index == anchor_line {
                current_path.clone_from(&table_path);
            }
            continue;
        }
        let Some((raw_key, _)) = trimmed.split_once('=') else {
            continue;
        };
        current_path.clone_from(&table_path);
        current_path.extend(split_dotted_path(raw_key));
        if line_index == anchor_line {
            break;
        }
    }

    current_path
}

fn toml_table_path(line: &str) -> Option<Vec<String>> {
    let table = line
        .strip_prefix("[[")
        .and_then(|value| value.strip_suffix("]]"))
        .or_else(|| line.strip_prefix('[').and_then(|value| value.strip_suffix(']')))?;
    Some(split_dotted_path(table))
}

fn split_dotted_path(path: &str) -> Vec<String> {
    path.split('.')
        .map(str::trim)
        .map(|segment| segment.trim_matches(['\'', '"']))
        .filter(|segment| !segment.is_empty())
        .map(sanitize_field_path_segment)
        .collect()
}

fn best_field_anchor(
    evidence: &AnchorEvidence,
    anchor_line: usize,
    line_count: usize,
    fields_by_record: &[Vec<String>],
) -> Option<(usize, usize)> {
    let total_fields = fields_by_record.iter().map(Vec::len).sum::<usize>();
    let mut best: Option<(usize, usize, usize, usize)> = None;
    let mut global_field_index = 0_usize;

    for (record_index, fields) in fields_by_record.iter().enumerate() {
        for (field_index, field) in fields.iter().enumerate() {
            let score = field_anchor_score(field, evidence);
            let source_distance = global_field_index
                .saturating_mul(line_count.max(1))
                .abs_diff(anchor_line.saturating_mul(total_fields.max(1)));
            let candidate = (score, source_distance, record_index, field_index);
            let replace = best.as_ref().is_none_or(|current| {
                candidate.0 > current.0 || (candidate.0 == current.0 && candidate.1 < current.1)
            });
            if replace {
                best = Some(candidate);
            }
            global_field_index = global_field_index.saturating_add(1);
        }
    }

    best.map(|(_, _, record_index, field_index)| (record_index, field_index))
}

fn field_anchor_score(field: &str, evidence: &AnchorEvidence) -> usize {
    let Some((path, value)) = field.split_once('=') else {
        return 0;
    };
    let path_segments = path.split('.').collect::<BTreeSet<_>>();
    let value_parts = value.split(", ").collect::<BTreeSet<_>>();
    let mut score = 0_usize;

    for candidate_path in &evidence.paths {
        if candidate_path == path {
            score = score.saturating_add(128);
        } else {
            let candidate_segments = candidate_path.split('.').collect::<Vec<_>>();
            if candidate_segments.iter().all(|segment| path_segments.contains(segment)) {
                score = score.saturating_add(48);
            }
        }
    }
    for lexeme in &evidence.lexemes {
        let sanitized = sanitize_field_path_segment(lexeme);
        if lexeme == value {
            score = score.saturating_add(64);
        } else if value_parts.contains(lexeme.as_str()) {
            score = score.saturating_add(40);
        } else if value.contains(lexeme) {
            score = score.saturating_add(8);
        }
        if sanitized == path {
            score = score.saturating_add(32);
        } else if path_segments.contains(sanitized.as_str()) {
            score = score.saturating_add(16);
        }
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comment_run_extraction_deduplicates_and_caps_document_text() {
        let oversized = "x".repeat(MAX_PRESERVED_CONFIG_COMMENT_CHARS);
        let input = format!("# repeated\na: 1\n# repeated\nb: 2\n# {oversized}\nc: 3\n");

        let runs = extract_comment_runs(&input);
        let preserved = runs.iter().map(|run| run.text.chars().count()).sum::<usize>();

        assert_eq!(runs.iter().filter(|run| run.text == "# repeated").count(), 1);
        assert_eq!(preserved, MAX_PRESERVED_CONFIG_COMMENT_CHARS);
    }

    #[test]
    fn yaml_sequence_comment_anchors_to_its_parent_field() {
        let input = "services:\n  broker:\n    ports:\n      # first port\n      - 5672:5672\n";
        let runs = extract_comment_runs(input);
        let fields = vec![vec![
            "services.broker.image=sample".to_string(),
            "services.broker.ports=5672:5672".to_string(),
        ]];

        let anchored = anchor_comment_runs(input, ConfigSyntax::Yaml, &runs, &fields);

        assert!(anchored[0][0].is_empty());
        assert_eq!(anchored[0][1], vec!["# first port"]);
    }
}
