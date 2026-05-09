#![allow(dead_code)]
// Compliance scan prints findings via eprintln so they appear in cargo test
// output before the assertion fires. Suppress workspace-wide print_stderr lint.
#![allow(clippy::print_stderr)]

use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;

pub(crate) type Finding = (PathBuf, usize, String);

pub(crate) fn api_manifest_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub(crate) fn workspace_root() -> PathBuf {
    let api_root = api_manifest_dir();
    match api_root.parent().and_then(Path::parent) {
        Some(root) => root.to_path_buf(),
        None => api_root,
    }
}

pub(crate) fn scan(root: &Path, exclusions: &[&str], pattern: &Regex) -> Vec<Finding> {
    scan_lines(root, exclusions, pattern, false)
}

pub(crate) fn scan_outside_cfg_test_blocks(
    root: &Path,
    exclusions: &[&str],
    pattern: &Regex,
) -> Vec<Finding> {
    scan_lines(root, exclusions, pattern, true)
}

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

        for (line_index, line) in source.lines().enumerate() {
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
        let mut pending_cfg_test = false;
        let mut cfg_test_depth = 0usize;

        for (line_index, line) in source.lines().enumerate() {
            if skip_cfg_test_blocks
                && should_skip_line_inside_cfg_test_block(
                    line,
                    &mut pending_cfg_test,
                    &mut cfg_test_depth,
                )
            {
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

fn should_skip_line_inside_cfg_test_block(
    line: &str,
    pending_cfg_test: &mut bool,
    cfg_test_depth: &mut usize,
) -> bool {
    let trimmed = line.trim_start();

    if *cfg_test_depth > 0 {
        *cfg_test_depth = update_brace_depth(*cfg_test_depth, line);
        return true;
    }

    if *pending_cfg_test {
        if trimmed.is_empty() || trimmed.starts_with("#[") {
            return true;
        }

        *pending_cfg_test = false;
        *cfg_test_depth = update_brace_depth(0, line);
        return true;
    }

    if trimmed.starts_with("#[cfg(test)]") {
        *pending_cfg_test = true;
        return true;
    }

    false
}

fn update_brace_depth(current: usize, line: &str) -> usize {
    let opens = line.bytes().filter(|byte| *byte == b'{').count();
    let closes = line.bytes().filter(|byte| *byte == b'}').count();
    current.saturating_add(opens).saturating_sub(closes)
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
