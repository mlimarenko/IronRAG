// Compliance scan prints findings via eprintln so they appear in cargo test
// output before the assertion fires.

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

#[allow(
    dead_code,
    reason = "each compliance integration target compiles this shared support module independently"
)]
pub(crate) type Finding = (PathBuf, usize, String);

#[allow(
    dead_code,
    reason = "only compliance targets that scan API sources need the API manifest root"
)]
pub(crate) fn api_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[allow(dead_code, reason = "only repository-wide compliance targets need the workspace root")]
pub(crate) fn workspace_root() -> PathBuf {
    let api_root = api_manifest_dir();
    match api_root.parent().and_then(Path::parent) {
        Some(root) => root.to_path_buf(),
        None => api_root,
    }
}

#[allow(
    dead_code,
    reason = "shared support exposes scan modes to independently compiled compliance targets"
)]
pub(crate) fn scan(root: &Path, exclusions: &[&str], pattern: &Regex) -> Vec<Finding> {
    scan_lines(root, exclusions, pattern, false)
}

#[allow(
    dead_code,
    reason = "shared support exposes scan modes to independently compiled compliance targets"
)]
pub(crate) fn scan_outside_cfg_test_blocks(
    root: &Path,
    exclusions: &[&str],
    pattern: &Regex,
) -> Vec<Finding> {
    scan_lines(root, exclusions, pattern, true)
}

#[allow(
    dead_code,
    reason = "shared support exposes scan modes to independently compiled compliance targets"
)]
pub(crate) fn scan_rust_string_literals(
    root: &Path,
    exclusions: &[&str],
    pattern: &Regex,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    for file_path in collect_files(root, exclusions) {
        let display_path = display_path(&file_path);
        let Ok(source) = fs::read_to_string(&file_path) else {
            continue;
        };
        let mut cfg_test_item_skipper = CfgTestItemSkipper::default();

        for (line_index, line) in source.lines().enumerate() {
            if cfg_test_item_skipper.should_skip_line(line) {
                continue;
            }
            for literal in rust_string_literals_on_line(line) {
                if pattern.is_match(&literal) {
                    findings.push((display_path.clone(), line_index + 1, literal));
                    break;
                }
            }
        }
    }

    findings
}

#[allow(
    dead_code,
    clippy::print_stderr,
    reason = "compliance failures print exact source findings before their aggregate assertion"
)]
pub(crate) fn print_findings(findings: &[Finding]) {
    for (path, line, source) in findings {
        eprintln!("{}:{}: {}", path.display(), line, source.trim());
    }
}

fn scan_lines(
    root: &Path,
    exclusions: &[&str],
    pattern: &Regex,
    skip_cfg_test_blocks: bool,
) -> Vec<Finding> {
    let mut findings = Vec::new();

    for file_path in collect_files(root, exclusions) {
        let display_path = display_path(&file_path);
        let Ok(source) = fs::read_to_string(&file_path) else {
            continue;
        };
        let mut cfg_test_item_skipper = CfgTestItemSkipper::default();

        for (line_index, line) in source.lines().enumerate() {
            if skip_cfg_test_blocks && cfg_test_item_skipper.should_skip_line(line) {
                continue;
            }

            if pattern.is_match(line) {
                findings.push((display_path.clone(), line_index + 1, line.to_string()));
            }
        }
    }

    findings
}

fn collect_files(root: &Path, exclusions: &[&str]) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];

    while let Some(path) = stack.pop() {
        if is_excluded(&path, exclusions) {
            continue;
        }

        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };

        if metadata.is_dir() {
            let Ok(entries) = fs::read_dir(&path) else {
                continue;
            };
            let mut child_paths = entries.flatten().map(|entry| entry.path()).collect::<Vec<_>>();
            child_paths.sort();
            stack.extend(child_paths.into_iter().rev());
            continue;
        }

        if metadata.is_file() && is_scannable_file(&path) {
            files.push(path);
        }
    }

    files.sort();
    files
}

fn is_scannable_file(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
        return true;
    };

    matches!(
        extension,
        "css"
            | "html"
            | "js"
            | "json"
            | "jsx"
            | "md"
            | "mdx"
            | "py"
            | "rs"
            | "scss"
            | "sh"
            | "sql"
            | "svg"
            | "toml"
            | "ts"
            | "tsx"
            | "txt"
            | "yaml"
            | "yml"
    )
}

#[derive(Default)]
struct CfgTestItemSkipper {
    lexical_state: RustLexicalState,
    skipped_item: Option<SkippedCfgTestItem>,
}

impl CfgTestItemSkipper {
    fn should_skip_line(&mut self, line: &str) -> bool {
        let structural_source = self.lexical_state.structural_source(line);

        if self.skipped_item.is_some() {
            self.advance_skipped_item(&structural_source);
            return true;
        }

        let Some(attribute_end) = cfg_test_attribute_end(&structural_source) else {
            return false;
        };
        self.skipped_item = Some(SkippedCfgTestItem::AwaitingItem);
        self.advance_skipped_item(&structural_source[attribute_end..]);
        true
    }

    fn advance_skipped_item(&mut self, structural_source: &str) {
        let Some(state) = self.skipped_item.take() else {
            return;
        };

        self.skipped_item = match state {
            SkippedCfgTestItem::AwaitingItem => {
                let Some(item_source) = source_after_leading_attributes(structural_source) else {
                    self.skipped_item = Some(SkippedCfgTestItem::AwaitingItem);
                    return;
                };
                let mut boundary = CfgTestItemBoundary::default();
                (!boundary.consume(item_source)).then_some(SkippedCfgTestItem::InItem(boundary))
            }
            SkippedCfgTestItem::InItem(mut boundary) => (!boundary.consume(structural_source))
                .then_some(SkippedCfgTestItem::InItem(boundary)),
        };
    }
}

enum SkippedCfgTestItem {
    AwaitingItem,
    InItem(CfgTestItemBoundary),
}

#[derive(Default)]
struct CfgTestItemBoundary {
    parenthesis_depth: usize,
    bracket_depth: usize,
    brace_depth: usize,
    top_level_assignment_seen: bool,
    braced_item_can_terminate: Option<bool>,
}

impl CfgTestItemBoundary {
    fn consume(&mut self, source: &str) -> bool {
        let bytes = source.as_bytes();
        for (index, byte) in bytes.iter().copied().enumerate() {
            match byte {
                b'(' => self.parenthesis_depth = self.parenthesis_depth.saturating_add(1),
                b')' => self.parenthesis_depth = self.parenthesis_depth.saturating_sub(1),
                b'[' => self.bracket_depth = self.bracket_depth.saturating_add(1),
                b']' => self.bracket_depth = self.bracket_depth.saturating_sub(1),
                b'{' => {
                    if self.brace_depth == 0
                        && self.parenthesis_depth == 0
                        && self.bracket_depth == 0
                    {
                        self.braced_item_can_terminate = Some(!self.top_level_assignment_seen);
                    }
                    self.brace_depth = self.brace_depth.saturating_add(1);
                }
                b'}' => {
                    self.brace_depth = self.brace_depth.saturating_sub(1);
                    if self.brace_depth == 0
                        && self.parenthesis_depth == 0
                        && self.bracket_depth == 0
                        && self.braced_item_can_terminate == Some(true)
                    {
                        return true;
                    }
                }
                b'=' if self.at_top_level()
                    && bytes.get(index.wrapping_sub(1)) != Some(&b'=')
                    && bytes.get(index + 1) != Some(&b'=')
                    && bytes.get(index + 1) != Some(&b'>') =>
                {
                    self.top_level_assignment_seen = true;
                }
                b';' if self.at_top_level() => return true,
                _ => {}
            }
        }
        false
    }

    fn at_top_level(&self) -> bool {
        self.parenthesis_depth == 0 && self.bracket_depth == 0 && self.brace_depth == 0
    }
}

fn cfg_test_attribute_end(source: &str) -> Option<usize> {
    const CFG_TEST_ATTRIBUTE: &str = "#[cfg(test)]";
    let leading_whitespace = source.len().saturating_sub(source.trim_start().len());
    source[leading_whitespace..]
        .starts_with(CFG_TEST_ATTRIBUTE)
        .then_some(leading_whitespace + CFG_TEST_ATTRIBUTE.len())
}

fn source_after_leading_attributes(mut source: &str) -> Option<&str> {
    loop {
        source = source.trim_start();
        if source.is_empty() {
            return None;
        }
        if !source.starts_with("#[") {
            return Some(source);
        }

        let mut bracket_depth = 1usize;
        let mut attribute_end = None;
        for (index, byte) in source.bytes().enumerate().skip(2) {
            match byte {
                b'[' => bracket_depth = bracket_depth.saturating_add(1),
                b']' if bracket_depth == 1 => {
                    attribute_end = Some(index + 1);
                    break;
                }
                b']' => bracket_depth = bracket_depth.saturating_sub(1),
                _ => {}
            }
        }
        source = &source[attribute_end?..];
    }
}

#[derive(Default)]
struct RustLexicalState {
    block_comment_depth: usize,
    raw_string_hashes: Option<usize>,
    quoted_literal: Option<u8>,
    quoted_literal_escaped: bool,
}

impl RustLexicalState {
    fn structural_source(&mut self, line: &str) -> String {
        let bytes = line.as_bytes();
        let mut structural = vec![b' '; bytes.len()];
        let mut index = 0usize;

        while index < bytes.len() {
            if let Some(next_index) = self.advance_raw_string(bytes, index) {
                index = next_index;
                continue;
            }
            if let Some(next_index) = self.advance_block_comment(bytes, index) {
                index = next_index;
                continue;
            }
            if let Some(next_index) = self.advance_quoted_literal(bytes, index) {
                index = next_index;
                continue;
            }
            match self.advance_structural_source(line, bytes, &mut structural, index) {
                StructuralSourceStep::Advance(next_index) => index = next_index,
                StructuralSourceStep::Finished => break,
            }
        }

        String::from_utf8_lossy(&structural).into_owned()
    }

    fn advance_raw_string(&mut self, bytes: &[u8], index: usize) -> Option<usize> {
        let hash_count = self.raw_string_hashes?;
        if bytes[index] == b'"' && raw_hashes_match(bytes, index + 1, hash_count) {
            self.raw_string_hashes = None;
            return Some(index.saturating_add(1 + hash_count));
        }
        Some(index + 1)
    }

    fn advance_block_comment(&mut self, bytes: &[u8], index: usize) -> Option<usize> {
        if self.block_comment_depth == 0 {
            return None;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            self.block_comment_depth = self.block_comment_depth.saturating_add(1);
            return Some(index + 2);
        }
        if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
            self.block_comment_depth = self.block_comment_depth.saturating_sub(1);
            return Some(index + 2);
        }
        Some(index + 1)
    }

    fn advance_quoted_literal(&mut self, bytes: &[u8], index: usize) -> Option<usize> {
        let delimiter = self.quoted_literal?;
        if self.quoted_literal_escaped {
            self.quoted_literal_escaped = false;
        } else if bytes[index] == b'\\' {
            self.quoted_literal_escaped = true;
        } else if bytes[index] == delimiter {
            self.quoted_literal = None;
        }
        Some(index + 1)
    }

    fn advance_structural_source(
        &mut self,
        line: &str,
        bytes: &[u8],
        structural: &mut [u8],
        index: usize,
    ) -> StructuralSourceStep {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            return StructuralSourceStep::Finished;
        }
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'*') {
            self.block_comment_depth = 1;
            return StructuralSourceStep::Advance(index + 2);
        }
        if let Some((content_start, hash_count)) = raw_string_start(bytes, index) {
            self.raw_string_hashes = Some(hash_count);
            return StructuralSourceStep::Advance(content_start);
        }
        if bytes[index] == b'"' {
            self.quoted_literal = Some(b'"');
            return StructuralSourceStep::Advance(index + 1);
        }
        if bytes[index] == b'\'' && character_literal_starts_at(line, index) {
            self.quoted_literal = Some(b'\'');
            return StructuralSourceStep::Advance(index + 1);
        }

        structural[index] = bytes[index];
        StructuralSourceStep::Advance(index + 1)
    }
}

enum StructuralSourceStep {
    Advance(usize),
    Finished,
}

fn raw_string_start(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    let raw_index = if bytes.get(index) == Some(&b'r') {
        index
    } else if matches!(bytes.get(index), Some(b'b' | b'c')) && bytes.get(index + 1) == Some(&b'r') {
        index + 1
    } else {
        return None;
    };
    if index > 0
        && bytes.get(index - 1).is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
    {
        return None;
    }

    let mut quote_index = raw_index + 1;
    while bytes.get(quote_index) == Some(&b'#') {
        quote_index += 1;
    }
    (bytes.get(quote_index) == Some(&b'"'))
        .then_some((quote_index + 1, quote_index.saturating_sub(raw_index + 1)))
}

fn character_literal_starts_at(line: &str, index: usize) -> bool {
    let remainder = &line[index + 1..];
    let mut characters = remainder.char_indices();
    let Some((_, first)) = characters.next() else {
        return false;
    };
    if first == '\\' {
        let mut escaped = false;
        return remainder.as_bytes().iter().skip(1).any(|byte| {
            if escaped {
                escaped = false;
                return false;
            }
            if *byte == b'\\' {
                escaped = true;
                return false;
            }
            *byte == b'\''
        });
    }
    let character_end = first.len_utf8();
    remainder.as_bytes().get(character_end) == Some(&b'\'')
}

fn rust_string_literals_on_line(line: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let bytes = line.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        if bytes[index] == b'/' && bytes.get(index + 1) == Some(&b'/') {
            break;
        }

        if let Some((literal, next_index)) = raw_string_at(line, index) {
            literals.push(literal);
            index = next_index;
            continue;
        }

        if bytes[index] == b'"' {
            if let Some((literal, next_index)) = normal_string_at(line, index) {
                literals.push(literal);
                index = next_index;
                continue;
            }
            break;
        }

        index += 1;
    }

    literals
}

fn raw_string_at(line: &str, index: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let raw_start = if bytes.get(index) == Some(&b'b') && bytes.get(index + 1) == Some(&b'r') {
        index + 1
    } else {
        index
    };

    if bytes.get(raw_start) != Some(&b'r') {
        return None;
    }

    let mut quote_index = raw_start + 1;
    while bytes.get(quote_index) == Some(&b'#') {
        quote_index += 1;
    }
    if bytes.get(quote_index) != Some(&b'"') {
        return None;
    }

    let hash_count = quote_index.saturating_sub(raw_start + 1);
    let content_start = quote_index + 1;
    let mut content_end = content_start;
    while content_end < bytes.len() {
        if bytes[content_end] == b'"' && raw_hashes_match(bytes, content_end + 1, hash_count) {
            let close_end = content_end + 1 + hash_count;
            return Some((line[content_start..content_end].to_string(), close_end));
        }
        content_end += 1;
    }

    None
}

fn raw_hashes_match(bytes: &[u8], start: usize, hash_count: usize) -> bool {
    bytes.len() >= start + hash_count
        && bytes[start..start + hash_count].iter().all(|byte| *byte == b'#')
}

fn normal_string_at(line: &str, index: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let content_start = index + 1;
    let mut cursor = content_start;
    let mut escaped = false;

    while cursor < bytes.len() {
        if escaped {
            escaped = false;
            cursor += 1;
            continue;
        }

        match bytes[cursor] {
            b'\\' => {
                escaped = true;
                cursor += 1;
            }
            b'"' => return Some((line[content_start..cursor].to_string(), cursor + 1)),
            _ => cursor += 1,
        }
    }

    None
}

fn is_excluded(path: &Path, exclusions: &[&str]) -> bool {
    let relative_path = normalized_relative_path(path);
    exclusions.iter().any(|pattern| path_matches_pattern(&relative_path, pattern))
}

fn display_path(path: &Path) -> PathBuf {
    let root = workspace_root();
    match path.strip_prefix(&root) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => path.to_path_buf(),
    }
}

fn normalized_relative_path(path: &Path) -> String {
    let root = workspace_root();
    let relative = match path.strip_prefix(&root) {
        Ok(value) => value,
        Err(_) => path,
    };
    relative.to_string_lossy().replace('\\', "/")
}

fn path_matches_pattern(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_start_matches("./");
    if pattern.ends_with('/') {
        return path.starts_with(pattern);
    }

    if !pattern.contains('*') {
        return path == pattern || path.starts_with(&format!("{pattern}/"));
    }

    let mut regex_pattern = String::from("^");
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '*' {
            if chars.peek() == Some(&'*') {
                chars.next();
                regex_pattern.push_str(".*");
            } else {
                regex_pattern.push_str("[^/]*");
            }
            continue;
        }
        push_regex_escaped_char(&mut regex_pattern, ch);
    }
    regex_pattern.push('$');

    Regex::new(&regex_pattern).map(|regex| regex.is_match(path)).unwrap_or(false)
}

fn push_regex_escaped_char(regex_pattern: &mut String, ch: char) {
    if matches!(ch, '.' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|' | '\\') {
        regex_pattern.push('\\');
    }
    regex_pattern.push(ch);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_fixture(source: &str) -> Result<Vec<Finding>, Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("fixture.rs"), source)?;
        let pattern = Regex::new(r"\p{Cyrillic}")?;

        Ok(scan_rust_string_literals(directory.path(), &[], &pattern))
    }

    #[test]
    fn path_patterns_preserve_scanner_matching_semantics() {
        let cases = [
            ("exact path", "src/lib.rs", "src/lib.rs", true),
            ("exact mismatch", "src/main.rs", "src/lib.rs", false),
            ("directory descendant", "src/lib/mod.rs", "src", true),
            ("directory sibling prefix", "src-old/lib.rs", "src", false),
            ("single star", "src/lib.rs", "src/*.rs", true),
            ("single star directory boundary", "src/lib/mod.rs", "src/*.rs", false),
            ("double star", "src/a/b/mod.rs", "src/**/mod.rs", true),
            ("double star slash boundary", "src/mod.rs", "src/**/mod.rs", false),
            ("leading current directory", "src/lib.rs", "./src/lib.rs", true),
            (
                "regex metacharacters",
                "docs/[v1]+(draft)?/guide.md",
                "docs/[v1]+(draft)?/*.md",
                true,
            ),
            (
                "regex metacharacter mismatch",
                "docs/v1draft/guide.md",
                "docs/[v1]+(draft)?/*.md",
                false,
            ),
        ];

        for (case, path, pattern, expected) in cases {
            assert_eq!(path_matches_pattern(path, pattern), expected, "{case}");
        }
    }

    #[test]
    fn cfg_test_item_boundaries_ignore_braces_in_strings_and_comments()
    -> Result<(), Box<dyn std::error::Error>> {
        let findings = scan_fixture(
            r#"
#[cfg(test)]
mod tests {
    const CLOSE: &str = "}";
    const TEST_ONLY_AFTER_STRING: &str = "Тестовая строка";
}

#[cfg(test)]
mod more_tests {
    // An opening brace in a comment must not extend the skipped item: {
}

const PRODUCTION_COPY: &str = "Рабочая строка";
"#,
        )?;

        assert_eq!(
            findings.iter().map(|(_, _, literal)| literal.as_str()).collect::<Vec<_>>(),
            vec!["Рабочая строка"],
        );
        Ok(())
    }

    #[test]
    fn cfg_test_skips_braceless_multiline_items_until_their_semicolon()
    -> Result<(), Box<dyn std::error::Error>> {
        let findings = scan_fixture(
            r#"
#[cfg(test)]
const TEST_ONLY: &str =
    "Тестовая строка";

const PRODUCTION_COPY: &str = "Рабочая строка";
"#,
        )?;

        assert_eq!(
            findings.iter().map(|(_, _, literal)| literal.as_str()).collect::<Vec<_>>(),
            vec!["Рабочая строка"],
        );
        Ok(())
    }
}
