//! AST-based identifier extraction for code blocks via tree-sitter.
//!
//! Supports the subset of programming languages whose tree-sitter
//! grammars are ABI-compatible with the backend's canonical
//! `tree-sitter` crate. When a structured block carries
//! a `code_language` tag that matches one of the supported grammars,
//! this module parses the block's text into an AST and walks it for
//! named declarations. The extracted identifiers are strictly more
//! precise than substring heuristics — they cannot produce false
//! positives on comments, string literals, or prose.

use tree_sitter::{Node, Parser, Tree};

/// An identifier extracted from a code AST.
#[derive(Debug, Clone)]
pub struct AstIdentifier {
    pub name: String,
    pub kind: AstIdentifierKind,
    pub start_byte: usize,
    pub end_byte: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AstIdentifierKind {
    Function,
    Class,
    Variable,
    Constant,
    EnvVar,
    Struct,
    Enum,
    Interface,
    Module,
    Type,
}

/// Parse `source` as `language` and extract named identifiers.
#[must_use]
pub fn extract_ast_identifiers(source: &str, language: &str) -> Option<Vec<AstIdentifier>> {
    let tree = parse_source(source, language)?;
    let root = tree.root_node();
    let mut identifiers = Vec::new();
    walk_generic(root, source, language, &mut identifiers);
    Some(identifiers)
}

/// Returns true if the language is supported by tree-sitter.
#[must_use]
pub fn is_supported_language(language: &str) -> bool {
    resolve_language(language).is_some()
}

/// Auto-detect the most likely language of a code block by trying
/// to parse it with each supported grammar and picking the one that
/// produces the cleanest AST (fewest ERROR nodes) and the most
/// extracted identifiers. Returns `None` if no grammar parses cleanly.
///
/// This is the fallback for fenced code blocks without a language tag
/// (` ``` ` with no annotation). A keyword pre-filter narrows the
/// candidate set before the full parse to keep latency bounded — on a
/// typical 10-line snippet, no more than six grammars are tried.
#[must_use]
pub fn detect_language(source: &str) -> Option<&'static str> {
    let candidates = prefilter_language_candidates(source);
    let mut best: Option<(&str, usize, usize)> = None; // (lang, identifiers, errors)

    for lang in candidates {
        let Some(tree) = parse_source(source, lang) else {
            continue;
        };
        let error_count = count_errors(tree.root_node());
        let mut ids = Vec::new();
        walk_generic(tree.root_node(), source, lang, &mut ids);

        let is_better = match best {
            None => true,
            Some((_, _, prev_errors)) if error_count < prev_errors => true,
            Some((_, prev_ids, prev_errors))
                if error_count == prev_errors && ids.len() > prev_ids =>
            {
                true
            }
            _ => false,
        };
        if is_better {
            best = Some((lang, ids.len(), error_count));
        }
    }

    // Accept if the best candidate has either:
    // - At least 1 identifier and reasonable error rate (code languages)
    // - Zero errors on a config format (yaml/toml/json — no identifiers expected)
    best.and_then(|(lang, id_count, error_count)| {
        let line_count = source.lines().count().max(1);
        let is_config_format = matches!(lang, "yaml" | "toml" | "json");
        if is_config_format && error_count == 0 && line_count >= 2 {
            return Some(lang);
        }
        if id_count > 0 && error_count * 3 <= line_count { Some(lang) } else { None }
    })
}

fn count_errors(node: Node<'_>) -> usize {
    let mut errors = usize::from(node.is_error() || node.is_missing());
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        errors += count_errors(child);
    }
    errors
}

type LanguageMarker = (&'static str, fn(&str) -> bool);

fn has_python_marker(source: &str) -> bool {
    source.contains("def ")
        || source.contains("import ")
        || source.contains("class ") && source.contains(":\n")
}

fn has_rust_marker(source: &str) -> bool {
    source.contains("fn ")
        || source.contains("let ")
        || source.contains("pub ")
        || source.contains("use ")
}

fn has_go_marker(source: &str) -> bool {
    source.contains("func ") || source.contains("package ")
}

fn has_javascript_marker(source: &str) -> bool {
    source.contains("const ") || source.contains("function ") || source.contains("=>")
}

fn has_typescript_marker(source: &str) -> bool {
    source.contains("interface ") || source.contains(": string") || source.contains(": number")
}

fn has_bash_marker(source: &str) -> bool {
    source.contains("#!/") || source.contains("export ") || source.contains("echo ")
}

fn has_c_marker(source: &str) -> bool {
    source.contains("#include") || source.contains("int ") && source.contains('(')
}

fn has_ruby_marker(source: &str) -> bool {
    source.contains("end\n") || source.contains("do |") || source.contains("require ")
}

fn has_php_marker(source: &str) -> bool {
    source.contains("<?php") || source.contains("$this->")
}

fn has_swift_marker(source: &str) -> bool {
    source.contains("let ") && source.contains("var ") || source.contains("guard ")
}

fn has_yaml_marker(source: &str) -> bool {
    source.lines().any(|line| {
        let trimmed = line.trim();
        trimmed.contains(": ") && !trimmed.starts_with('#') && !trimmed.starts_with("//")
    })
}

fn has_toml_marker(source: &str) -> bool {
    source.contains('[') && source.lines().any(|line| line.trim().contains(" = "))
}

fn has_json_marker(source: &str) -> bool {
    source.trim_start().starts_with('{')
}

const LANGUAGE_MARKERS: [LanguageMarker; 13] = [
    ("python", has_python_marker),
    ("rust", has_rust_marker),
    ("go", has_go_marker),
    ("javascript", has_javascript_marker),
    ("typescript", has_typescript_marker),
    ("bash", has_bash_marker),
    ("c", has_c_marker),
    ("ruby", has_ruby_marker),
    ("php", has_php_marker),
    ("swift", has_swift_marker),
    ("yaml", has_yaml_marker),
    ("toml", has_toml_marker),
    ("json", has_json_marker),
];

fn push_java_family_candidate(source: &str, candidates: &mut Vec<&'static str>) {
    if !(source.contains("public ") || source.contains("private ") || source.contains("class ")) {
        return;
    }
    candidates.push(if source.contains("System.") || source.contains("namespace ") {
        "csharp"
    } else {
        "java"
    });
}

const JAVA_FAMILY_PRECEDING_MARKERS: usize = 5;

fn append_matching_markers(
    source: &str,
    markers: &[LanguageMarker],
    candidates: &mut Vec<&'static str>,
) {
    candidates.extend(
        markers.iter().filter_map(|(language, matches)| matches(source).then_some(*language)),
    );
}

/// Quick keyword scan to narrow the grammar search space. Returns
/// ≤6 candidate language names based on syntax markers.
fn prefilter_language_candidates(source: &str) -> Vec<&'static str> {
    let mut candidates = Vec::new();
    append_matching_markers(
        source,
        &LANGUAGE_MARKERS[..JAVA_FAMILY_PRECEDING_MARKERS],
        &mut candidates,
    );
    push_java_family_candidate(source, &mut candidates);
    append_matching_markers(
        source,
        &LANGUAGE_MARKERS[JAVA_FAMILY_PRECEDING_MARKERS..],
        &mut candidates,
    );

    if candidates.is_empty() {
        candidates.extend_from_slice(&["python", "javascript", "bash"]);
    }
    candidates.truncate(6);
    candidates
}

fn resolve_language(language: &str) -> Option<tree_sitter::Language> {
    Some(match language {
        "python" | "py" => tree_sitter_python::LANGUAGE.into(),
        "javascript" | "js" | "jsx" => tree_sitter_javascript::LANGUAGE.into(),
        "typescript" | "ts" | "tsx" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "bash" | "sh" | "shell" | "zsh" => tree_sitter_bash::LANGUAGE.into(),
        "rust" | "rs" => tree_sitter_rust::LANGUAGE.into(),
        "go" | "golang" => tree_sitter_go::LANGUAGE.into(),
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" | "h" => tree_sitter_c::LANGUAGE.into(),
        "csharp" | "cs" | "c#" => tree_sitter_c_sharp::LANGUAGE.into(),
        "ruby" | "rb" => tree_sitter_ruby::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "swift" => tree_sitter_swift::LANGUAGE.into(),
        "scala" => tree_sitter_scala::LANGUAGE.into(),
        "yaml" | "yml" => tree_sitter_yaml::LANGUAGE.into(),
        "proto" | "protobuf" => tree_sitter_proto::LANGUAGE.into(),
        _ => return None,
    })
}

fn parse_source(source: &str, language: &str) -> Option<Tree> {
    let ts_language = resolve_language(language)?;
    let mut parser = Parser::new();
    if parser.set_language(&ts_language).is_err() {
        #[cfg(test)]
        tracing::warn!("[tree-sitter] set_language failed for {language}");
        return None;
    }
    let tree = parser.parse(source, None);
    if tree.is_none() {
        #[cfg(test)]
        tracing::warn!("[tree-sitter] parse returned None for {language}");
    }
    tree
}

fn node_name_text<'a>(node: Node<'_>, source: &'a str) -> Option<&'a str> {
    let name = node.child_by_field_name("name")?;
    Some(&source[name.start_byte()..name.end_byte()])
}

fn push_named(node: Node<'_>, source: &str, kind: AstIdentifierKind, out: &mut Vec<AstIdentifier>) {
    if let Some(name) = node_name_text(node, source)
        && !name.is_empty()
        && name.len() <= 120
    {
        out.push(AstIdentifier {
            name: name.to_string(),
            kind,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        });
    }
}

fn push_node_slice(
    node: Node<'_>,
    source: &str,
    kind: AstIdentifierKind,
    out: &mut Vec<AstIdentifier>,
) {
    let name = &source[node.start_byte()..node.end_byte()];
    if !name.is_empty() && name.len() <= 120 {
        out.push(AstIdentifier {
            name: name.to_string(),
            kind,
            start_byte: node.start_byte(),
            end_byte: node.end_byte(),
        });
    }
}

fn push_function_declarator(node: Node<'_>, source: &str, out: &mut Vec<AstIdentifier>) {
    if let Some(declarator) = node.child_by_field_name("declarator") {
        push_node_slice(declarator, source, AstIdentifierKind::Function, out);
    }
}

fn push_variable_declarator(node: Node<'_>, source: &str, out: &mut Vec<AstIdentifier>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    if name_node.kind() != "identifier" {
        return;
    }
    let is_const = node.parent().is_some_and(|parent| {
        parent.kind() == "lexical_declaration"
            && source[parent.start_byte()..node.start_byte()].contains("const")
    });
    let kind = if is_const { AstIdentifierKind::Constant } else { AstIdentifierKind::Variable };
    push_node_slice(name_node, source, kind, out);
}

fn push_assignment(node: Node<'_>, source: &str, out: &mut Vec<AstIdentifier>) {
    let Some(left) = node.child_by_field_name("left") else {
        return;
    };
    if left.kind() != "identifier" {
        return;
    }
    let name = &source[left.start_byte()..left.end_byte()];
    let kind = if name.chars().all(|character| character.is_uppercase() || character == '_') {
        AstIdentifierKind::Constant
    } else {
        AstIdentifierKind::Variable
    };
    push_node_slice(left, source, kind, out);
}

fn push_variable_assignment(node: Node<'_>, source: &str, out: &mut Vec<AstIdentifier>) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let name = &source[name_node.start_byte()..name_node.end_byte()];
    let kind = if name.chars().all(|character| character.is_uppercase() || character == '_') {
        AstIdentifierKind::EnvVar
    } else {
        AstIdentifierKind::Variable
    };
    push_node_slice(name_node, source, kind, out);
}

/// Generic AST walker that dispatches by node kind string. This
/// covers the common patterns across all C-family and scripting
/// languages without requiring per-language walkers.
fn walk_generic(node: Node<'_>, source: &str, language: &str, out: &mut Vec<AstIdentifier>) {
    match node.kind() {
        "function_definition"
        | "function_declaration"
        | "method_declaration"
        | "method_definition"
        | "method"
        | "function_item"
        | "constructor_declaration"
        | "singleton_method" => {
            push_named(node, source, AstIdentifierKind::Function, out);
        }
        "class_definition" | "class_declaration" | "class_specifier" | "class" => {
            push_named(node, source, AstIdentifierKind::Class, out);
        }
        "function_declarator" => push_function_declarator(node, source, out),
        "struct_item" | "struct_specifier" => {
            push_named(node, source, AstIdentifierKind::Struct, out);
        }
        "type_spec" if matches!(language, "go" | "golang") => {
            push_named(node, source, AstIdentifierKind::Struct, out);
        }
        "enum_item" | "enum_declaration" | "enum_specifier" => {
            push_named(node, source, AstIdentifierKind::Enum, out);
        }
        "trait_item" | "interface_declaration" | "protocol_declaration" => {
            push_named(node, source, AstIdentifierKind::Interface, out);
        }
        "mod_item" | "module_declaration" | "package_declaration" => {
            push_named(node, source, AstIdentifierKind::Module, out);
        }
        "type_alias_declaration" | "type_item" => {
            push_named(node, source, AstIdentifierKind::Type, out);
        }
        "variable_declarator" => push_variable_declarator(node, source, out),
        "assignment" => push_assignment(node, source, out),
        "variable_assignment" => push_variable_assignment(node, source, out),
        "const_item" | "static_item" => {
            push_named(node, source, AstIdentifierKind::Constant, out);
        }
        "create_table_statement" | "create_view_statement" | "message" | "service" | "rpc" => {
            push_named(node, source, AstIdentifierKind::Type, out);
        }
        _ => {}
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_generic(child, source, language, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_function_and_class() {
        let ids =
            extract_ast_identifiers("def hello():\n    pass\n\nclass Foo:\n    pass", "python")
                .unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"hello"), "{names:?}");
        assert!(names.contains(&"Foo"), "{names:?}");
    }

    #[test]
    fn javascript_const_and_function() {
        let ids =
            extract_ast_identifiers("const API_KEY = 'x';\nfunction fetch() {}", "javascript")
                .unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"API_KEY"), "{names:?}");
        assert!(names.contains(&"fetch"), "{names:?}");
    }

    #[test]
    fn typescript_interface_and_class() {
        let ids =
            extract_ast_identifiers("interface Config { url: string }\nclass App {}", "typescript")
                .unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"App"), "{names:?}");
    }

    #[test]
    fn bash_env_vars() {
        let ids = extract_ast_identifiers(
            "DATABASE_URL=postgres://localhost\nexport API_KEY=secret",
            "bash",
        )
        .unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"DATABASE_URL"), "{names:?}");
        assert!(names.contains(&"API_KEY"), "{names:?}");
    }

    #[test]
    fn go_func_and_struct() {
        let code = "func main() {}\ntype Config struct {}";
        let ids = extract_ast_identifiers(code, "go").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"main"), "{names:?}");
    }

    #[test]
    fn java_class_and_method() {
        let code = "public class UserService {\n  public void createUser() {}\n}";
        let ids = extract_ast_identifiers(code, "java").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "{names:?}");
        assert!(names.contains(&"createUser"), "{names:?}");
    }

    #[test]
    fn java_family_candidates_keep_priority_before_language_cap() {
        let source = "public class Sample {\n  public void run() {\n    const int count = 1;\n    echo(count);\n  }\n}";

        let candidates = prefilter_language_candidates(source);

        assert!(candidates.contains(&"java"), "{candidates:?}");
        assert!(candidates.len() <= 6);
    }

    #[test]
    fn php_function_and_class() {
        let code = "<?php\nclass Invoice {}\nfunction calculateTotal() {}";
        let ids = extract_ast_identifiers(code, "php").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Invoice"), "{names:?}");
        assert!(names.contains(&"calculateTotal"), "{names:?}");
    }

    #[test]
    fn unsupported_grammar_returns_none() {
        // These grammars ARE now supported after upgrading tree-sitter to 0.25
        assert!(extract_ast_identifiers("pub fn build_router() {}", "rust").is_some());
        assert!(extract_ast_identifiers("class Payment\nend", "ruby").is_some());
        assert!(extract_ast_identifiers("public class OrderController {}", "csharp").is_some());
        assert!(extract_ast_identifiers("class ViewModel {}", "swift").is_some());
        assert!(extract_ast_identifiers("int main() { return 0; }", "c").is_some());
        // Actually unsupported:
        assert!(extract_ast_identifiers("hello", "cobol").is_none());
    }

    #[test]
    fn scala_class_and_def() {
        let code = "class UserService {\n  def findUser(id: Int): User = ???\n}";
        let ids = extract_ast_identifiers(code, "scala").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "{names:?}");
    }

    #[test]
    fn unsupported_returns_none() {
        assert!(extract_ast_identifiers("hello", "cobol").is_none());
    }

    #[test]
    fn rust_fn_struct_enum() {
        let code = "pub fn build_router() {}\npub struct AppState {}\nenum Status { Ok, Err }";
        let ids = extract_ast_identifiers(code, "rust").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"build_router"), "rust names: {names:?}");
        assert!(names.contains(&"AppState"), "rust names: {names:?}");
        assert!(names.contains(&"Status"), "rust names: {names:?}");
    }

    #[test]
    fn c_function() {
        let code = "int main(int argc, char** argv) { return 0; }";
        let ids = extract_ast_identifiers(code, "c").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"main"), "c names: {names:?}");
    }

    #[test]
    fn csharp_class_and_method() {
        let code = "public class OrderController {\n  public void GetOrders() {}\n}";
        let ids = extract_ast_identifiers(code, "csharp").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"OrderController"), "csharp names: {names:?}");
        assert!(names.contains(&"GetOrders"), "csharp names: {names:?}");
    }

    #[test]
    fn ruby_class() {
        let code = "class Payment\n  def process\n  end\nend";
        let ids = extract_ast_identifiers(code, "ruby").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"Payment"), "ruby names: {names:?}");
    }

    #[test]
    fn swift_class_and_func() {
        let code = "class ViewModel {}\nfunc loadData() {}";
        let ids = extract_ast_identifiers(code, "swift").unwrap();
        let names: Vec<&str> = ids.iter().map(|i| i.name.as_str()).collect();
        assert!(names.contains(&"ViewModel"), "swift names: {names:?}");
        assert!(names.contains(&"loadData"), "swift names: {names:?}");
    }

    #[test]
    fn supported_language_check() {
        for lang in &[
            "python",
            "javascript",
            "typescript",
            "bash",
            "rust",
            "go",
            "java",
            "c",
            "csharp",
            "ruby",
            "php",
            "swift",
            "scala",
            "yaml",
            "proto",
        ] {
            assert!(is_supported_language(lang), "{lang} should be supported");
        }
        assert!(!is_supported_language("cobol"));
        assert!(!is_supported_language("fortran"));
    }
}
