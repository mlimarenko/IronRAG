/// Detects line indices where new logical code units begin (functions, classes, etc.)
/// Returns 0-based line numbers that are good split points.
pub(super) fn detect_code_boundaries(text: &str, language: Option<&str>) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut boundaries = Vec::new();

    let lang = language.unwrap_or("");
    let effective_lang = if lang.is_empty() { guess_language(&lines) } else { lang };

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if is_code_boundary(trimmed, line, effective_lang, i, &lines) {
            boundaries.push(i);
        }
    }

    boundaries
}

/// A language detector: given a source sample it returns the language it
/// recognises, or `None` when the sample carries no signature for that language.
type LanguageDetector = fn(&str) -> Option<&'static str>;

pub(super) fn guess_language(lines: &[&str]) -> &'static str {
    let sample = lines.iter().take(50).copied().collect::<Vec<_>>().join("\n");
    const DETECTORS: &[LanguageDetector] = &[
        detect_rust,
        detect_python,
        detect_go,
        detect_javascript_family,
        detect_class_based_language,
        detect_php,
        detect_ruby,
        detect_swift,
        detect_kotlin,
        detect_elixir,
        detect_dockerfile,
        detect_yaml,
        detect_json,
        detect_terraform,
        detect_sql,
    ];
    DETECTORS.iter().find_map(|detect| detect(&sample)).unwrap_or("")
}

fn detect_rust(sample: &str) -> Option<&'static str> {
    (sample.contains("fn ")
        && (sample.contains("-> ") || sample.contains("pub ") || sample.contains("let ")))
    .then_some("rust")
}

fn detect_python(sample: &str) -> Option<&'static str> {
    (sample.contains("def ") && sample.contains(':') && !sample.contains('{')).then_some("python")
}

fn detect_go(sample: &str) -> Option<&'static str> {
    (sample.contains("func ") && sample.contains("package ")).then_some("go")
}

fn detect_javascript_family(sample: &str) -> Option<&'static str> {
    (sample.contains("function ") || sample.contains("const ") || sample.contains("=> {")).then(
        || {
            if sample.contains(": string")
                || sample.contains(": number")
                || sample.contains("interface ")
            {
                "typescript"
            } else {
                "javascript"
            }
        },
    )
}

fn detect_class_based_language(sample: &str) -> Option<&'static str> {
    if !sample.contains("class ")
        || !sample.contains('{')
        || (!sample.contains("public ") && !sample.contains("private "))
    {
        return None;
    }
    if sample.contains("package ") && sample.contains("import java") {
        return Some("java");
    }
    if sample.contains("#include") || sample.contains("std::") {
        return Some("cpp");
    }
    if sample.contains("using ") && sample.contains("namespace ") {
        return Some("csharp");
    }
    None
}

fn detect_php(sample: &str) -> Option<&'static str> {
    (sample.contains("<?php") || (sample.contains("function ") && sample.contains('$')))
        .then_some("php")
}

fn detect_ruby(sample: &str) -> Option<&'static str> {
    (sample.contains("require ") && sample.contains("end\n")).then_some("ruby")
}

fn detect_swift(sample: &str) -> Option<&'static str> {
    (sample.contains("import Swift")
        || (sample.contains("func ") && sample.contains("->") && !sample.contains("pub ")))
    .then_some("swift")
}

fn detect_kotlin(sample: &str) -> Option<&'static str> {
    (sample.contains("fun ") && sample.contains("val ")).then_some("kotlin")
}

fn detect_elixir(sample: &str) -> Option<&'static str> {
    (sample.contains("defmodule ") || (sample.contains("def ") && sample.contains("do\n")))
        .then_some("elixir")
}

fn detect_dockerfile(sample: &str) -> Option<&'static str> {
    (sample.contains("FROM ") && sample.contains("RUN ")).then_some("dockerfile")
}

fn detect_yaml(sample: &str) -> Option<&'static str> {
    (sample.contains("---") && sample.contains(": ") && !sample.contains('{')).then_some("yaml")
}

fn detect_json(sample: &str) -> Option<&'static str> {
    (sample.starts_with('{') || sample.contains("\":\n")).then_some("json")
}

fn detect_terraform(sample: &str) -> Option<&'static str> {
    (sample.contains("resource ") && sample.contains("provider ")).then_some("terraform")
}

fn detect_sql(sample: &str) -> Option<&'static str> {
    (sample.contains("CREATE TABLE") || sample.contains("SELECT ")).then_some("sql")
}

fn is_code_boundary(
    trimmed: &str,
    raw_line: &str,
    lang: &str,
    line_idx: usize,
    lines: &[&str],
) -> bool {
    match lang {
        "rust" | "rs" => is_rust_boundary(trimmed, line_idx, lines),
        "python" | "py" => is_python_boundary(trimmed, line_idx, lines),
        "go" => is_go_boundary(trimmed),
        "typescript" | "ts" | "tsx" | "javascript" | "js" | "jsx" => {
            is_javascript_boundary(trimmed, raw_line)
        }
        "java" => is_java_boundary(trimmed),
        "csharp" | "cs" => is_csharp_boundary(trimmed),
        "c" | "cpp" | "cc" | "cxx" | "h" | "hpp" => is_c_family_boundary(trimmed, raw_line),
        "php" => is_php_boundary(trimmed),
        "ruby" | "rb" => is_ruby_boundary(trimmed),
        "swift" => is_swift_boundary(trimmed),
        "kotlin" | "kt" => is_kotlin_boundary(trimmed),
        "scala" => is_scala_boundary(trimmed),
        "elixir" | "ex" | "exs" => is_elixir_boundary(trimmed),
        "dart" => is_dart_boundary(trimmed),
        "lua" => starts_with_any(trimmed, &["function ", "local function "]),
        "r" => contains_any(trimmed, &["<- function(", "= function("]),
        "sh" | "bash" | "shell" | "zsh" => is_shell_boundary(trimmed),
        "sql" => is_sql_boundary(trimmed),
        "terraform" | "tf" | "hcl" => is_terraform_boundary(trimmed),
        "dockerfile" => is_dockerfile_boundary(trimmed),
        "yaml" | "yml" => is_yaml_boundary(trimmed, raw_line),
        "toml" => trimmed.starts_with('[') && trimmed.ends_with(']'),
        _ => false,
    }
}

fn starts_with_any(value: &str, prefixes: &[&str]) -> bool {
    prefixes.iter().any(|prefix| value.starts_with(prefix))
}

fn contains_any(value: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| value.contains(needle))
}

fn next_trimmed_line<'line>(line_idx: usize, lines: &'line [&str]) -> Option<&'line str> {
    lines.get(line_idx.saturating_add(1)).map(|line| line.trim())
}

fn is_rust_boundary(trimmed: &str, line_idx: usize, lines: &[&str]) -> bool {
    const PREFIXES: &[&str] = &[
        "pub fn ",
        "fn ",
        "pub async fn ",
        "async fn ",
        "pub struct ",
        "struct ",
        "pub enum ",
        "enum ",
        "pub trait ",
        "trait ",
        "impl ",
        "pub impl ",
        "mod ",
        "pub mod ",
        "pub const ",
        "pub static ",
        "#[test]",
        "#[cfg(test)]",
    ];
    starts_with_any(trimmed, PREFIXES)
        || (trimmed.starts_with("/// ")
            && next_trimmed_line(line_idx, lines)
                .is_some_and(|next| starts_with_any(next, &["pub fn ", "fn ", "pub struct "])))
}

fn is_python_boundary(trimmed: &str, line_idx: usize, lines: &[&str]) -> bool {
    starts_with_any(trimmed, &["def ", "async def ", "class "])
        || (trimmed.starts_with('@')
            && !trimmed.starts_with("@property")
            && next_trimmed_line(line_idx, lines)
                .is_some_and(|next| starts_with_any(next, &["def ", "class ", "async def "])))
}

fn is_go_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["func ", "func(", "package "])
        || (trimmed.starts_with("type ") && contains_any(trimmed, &[" struct ", " interface "]))
}

fn is_javascript_boundary(trimmed: &str, raw_line: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "function ",
        "async function ",
        "export function ",
        "export async function ",
        "export default function ",
        "class ",
        "export class ",
        "interface ",
        "export interface ",
        "export type ",
    ];
    starts_with_any(trimmed, PREFIXES)
        || (trimmed.starts_with("type ") && trimmed.contains(" = "))
        || (trimmed.starts_with("export const ") && contains_any(trimmed, &[" = (", " = async ("]))
        || (trimmed.starts_with("const ")
            && trimmed.contains(" = (")
            && raw_line.starts_with("const "))
}

fn is_java_boundary(trimmed: &str) -> bool {
    const MODIFIERS: &[&str] = &["public ", "private ", "protected "];
    const DECLARATIONS: &[&str] = &[" class ", " interface ", " enum "];
    (starts_with_any(trimmed, MODIFIERS)
        && (contains_any(trimmed, DECLARATIONS)
            || (trimmed.contains('(') && trimmed.contains(')') && !trimmed.contains('='))))
        || starts_with_any(trimmed, &["@Override", "@Test", "package ", "import "])
}

fn is_csharp_boundary(trimmed: &str) -> bool {
    const MODIFIERS: &[&str] = &["public ", "private ", "protected ", "internal "];
    const DECLARATIONS: &[&str] = &[" class ", " interface ", " struct ", " enum "];
    (starts_with_any(trimmed, MODIFIERS)
        && (contains_any(trimmed, DECLARATIONS)
            || (trimmed.contains('(') && !trimmed.contains('='))))
        || starts_with_any(trimmed, &["namespace ", "using "])
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
}

fn is_c_family_boundary(trimmed: &str, raw_line: &str) -> bool {
    let is_top_level_declaration = raw_line
        .starts_with(|character: char| character.is_alphabetic() || character == '_')
        && trimmed.contains('(')
        && !starts_with_any(trimmed, &["//", "#", "if ", "for ", "while ", "return ", "case "]);
    is_top_level_declaration
        || starts_with_any(
            trimmed,
            &["class ", "struct ", "namespace ", "template", "#include ", "#define "],
        )
}

fn is_php_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &[
            "function ",
            "public function ",
            "private function ",
            "protected function ",
            "class ",
            "interface ",
            "trait ",
            "namespace ",
        ],
    )
}

fn is_ruby_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["def ", "class ", "module ", "describe ", "it ", "context "])
}

fn is_swift_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["func ", "class ", "struct ", "enum ", "protocol ", "extension ", "import "],
    )
}

fn is_kotlin_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &[
            "fun ",
            "class ",
            "data class ",
            "object ",
            "interface ",
            "sealed class ",
            "suspend fun ",
            "override fun ",
            "package ",
            "import ",
        ],
    )
}

fn is_scala_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["def ", "class ", "object ", "trait ", "case class ", "val ", "package ", "import "],
    )
}

fn is_elixir_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["def ", "defp ", "defmodule ", "defmacro ", "test ", "describe "])
}

fn is_dart_boundary(trimmed: &str) -> bool {
    starts_with_any(trimmed, &["class ", "abstract class ", "void ", "Future<", "import ", "enum "])
}

fn is_shell_boundary(trimmed: &str) -> bool {
    trimmed.starts_with("function ")
        || (!trimmed.starts_with('#') && (trimmed.ends_with("()") || trimmed.ends_with("() {")))
}

fn is_sql_boundary(trimmed: &str) -> bool {
    let upper = trimmed.to_ascii_uppercase();
    starts_with_any(
        &upper,
        &[
            "CREATE TABLE",
            "CREATE INDEX",
            "CREATE VIEW",
            "CREATE FUNCTION",
            "ALTER TABLE",
            "INSERT INTO",
            "-- ===",
            "-- ---",
        ],
    )
}

fn is_terraform_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["resource ", "data ", "variable ", "output ", "module ", "provider ", "locals {"],
    )
}

fn is_dockerfile_boundary(trimmed: &str) -> bool {
    starts_with_any(
        trimmed,
        &["FROM ", "RUN ", "COPY ", "ENTRYPOINT ", "CMD ", "EXPOSE ", "ENV ", "WORKDIR "],
    )
}

fn is_yaml_boundary(trimmed: &str, raw_line: &str) -> bool {
    !raw_line.starts_with(' ')
        && !raw_line.starts_with('\t')
        && trimmed.ends_with(':')
        && !trimmed.starts_with('#')
        && !trimmed.starts_with('-')
}
