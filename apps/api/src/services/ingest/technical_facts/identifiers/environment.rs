use std::collections::BTreeSet;

use super::super::{
    FactCandidate, StructuredBlockData, TechnicalFactKind, build_candidate, technical_tokens,
    trim_technical_token,
};

const QUOTED_ENVIRONMENT_PATTERNS: [&str; 5] =
    ["os.getenv(", "os.environ[", "env::var(", "std::env::var(", "ENV["];

pub(crate) fn extract_environment_variable_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut env_vars = BTreeSet::<String>::new();
    collect_token_environment_variables(line, &mut env_vars);
    collect_quoted_environment_variables(line, &mut env_vars);

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

fn collect_token_environment_variables(line: &str, env_vars: &mut BTreeSet<String>) {
    for token in technical_tokens(line) {
        let shell_name =
            token.trim_start_matches('$').trim_start_matches('{').trim_end_matches('}');
        if token.starts_with('$') && is_env_var_name(shell_name) {
            env_vars.insert(shell_name.to_string());
        }

        if let Some(rest) = token.strip_prefix("process.env.") {
            let name = trim_technical_token(rest);
            if is_env_var_name(name) {
                env_vars.insert(name.to_string());
            }
        }
    }
}

fn collect_quoted_environment_variables(line: &str, env_vars: &mut BTreeSet<String>) {
    for pattern in QUOTED_ENVIRONMENT_PATTERNS {
        for (position, _) in line.match_indices(pattern) {
            let after_pattern = &line[position + pattern.len()..];
            if let Some(name) =
                extract_quoted_argument(after_pattern).filter(|name| is_env_var_name(name))
            {
                env_vars.insert(name);
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::{collect_quoted_environment_variables, collect_token_environment_variables};
    use std::collections::BTreeSet;

    #[test]
    fn collects_supported_environment_variable_forms_once() {
        let line = concat!(
            "$ALPHA_MODE ${BETA_MODE} process.env.GAMMA_MODE ",
            "os.getenv('DELTA_MODE') os.environ[\"EPSILON_MODE\"] ",
            "env::var(\"ZETA_MODE\") std::env::var('ETA_MODE') ENV[\"THETA_MODE\"]"
        );
        let mut values = BTreeSet::new();

        collect_token_environment_variables(line, &mut values);
        collect_quoted_environment_variables(line, &mut values);

        assert_eq!(
            values.into_iter().collect::<Vec<_>>(),
            vec![
                "ALPHA_MODE",
                "BETA_MODE",
                "DELTA_MODE",
                "EPSILON_MODE",
                "ETA_MODE",
                "GAMMA_MODE",
                "THETA_MODE",
                "ZETA_MODE",
            ]
        );
    }
}
