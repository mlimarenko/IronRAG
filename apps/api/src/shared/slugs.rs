#[must_use]
pub fn slugify(value: &str) -> String {
    let slug = value
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect::<String>();
    let slug = slug.split('-').filter(|segment| !segment.is_empty()).collect::<Vec<_>>().join("-");
    if slug.is_empty() { "new-item".to_string() } else { slug }
}

#[cfg(test)]
mod tests {
    use super::slugify;

    #[test]
    fn slugify_normalizes_mixed_input() {
        assert_eq!(slugify("  Agent Workspace 2026!  "), "agent-workspace-2026");
    }

    #[test]
    fn slugify_falls_back_for_non_alphanumeric_input() {
        assert_eq!(slugify(" !!! "), "new-item");
    }
}
