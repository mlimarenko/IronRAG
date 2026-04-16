use std::collections::BTreeSet;

use super::super::{
    FactCandidate, StructuredBlockData, TechnicalFactKind, build_candidate, matches_any_substring,
    technical_tokens, trim_technical_token,
};

pub(crate) fn extract_environment_variable_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut env_vars = BTreeSet::<String>::new();

    let tokens = technical_tokens(line);
    let lower = line.to_ascii_lowercase();
    let has_env_context = matches_any_substring(
        &lower,
        &["environment", "env", "variable", "export", "getenv", "environ"],
    );

    for token in &tokens {
        if token.starts_with('$') {
            let name = token.trim_start_matches('$').trim_start_matches('{').trim_end_matches('}');
            if is_env_var_name(name) {
                env_vars.insert(name.to_string());
            }
        }

        if let Some(rest) = token.strip_prefix("process.env.") {
            let name = trim_technical_token(rest);
            if is_env_var_name(name) {
                env_vars.insert(name.to_string());
            }
        }
    }

    for pattern in &["os.getenv(", "os.environ["] {
        if let Some(pos) = lower.find(pattern) {
            let after = &line[pos + pattern.len()..];
            if let Some(name) = extract_quoted_argument(after)
                && is_env_var_name(&name)
            {
                env_vars.insert(name);
            }
        }
    }

    for pattern in &["env::var(", "std::env::var("] {
        if let Some(pos) = line.find(pattern) {
            let after = &line[pos + pattern.len()..];
            if let Some(name) = extract_quoted_argument(after)
                && is_env_var_name(&name)
            {
                env_vars.insert(name);
            }
        }
    }

    if let Some(pos) = line.find("ENV[") {
        let after = &line[pos + 4..];
        if let Some(name) = extract_quoted_argument(after)
            && is_env_var_name(&name)
        {
            env_vars.insert(name);
        }
    }

    if has_env_context {
        for token in &tokens {
            let candidate = trim_technical_token(token);
            if is_env_var_name(candidate) {
                env_vars.insert(candidate.to_string());
            }
        }
    }

    env_vars
        .into_iter()
        .filter_map(|var| {
            build_candidate(
                block,
                TechnicalFactKind::EnvironmentVariable,
                &var,
                Vec::new(),
                line,
                "environment_variable",
            )
        })
        .collect()
}

fn is_env_var_name(candidate: &str) -> bool {
    if candidate.len() < 3 {
        return false;
    }
    let mut chars = candidate.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn extract_quoted_argument(after: &str) -> Option<String> {
    let trimmed = after.trim_start();
    let quote = trimmed.chars().next()?;
    if !matches!(quote, '"' | '\'') {
        return None;
    }
    let rest = &trimmed[1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}
