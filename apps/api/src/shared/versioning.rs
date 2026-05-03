use std::collections::BTreeSet;

pub fn dotted_version_terms(text: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    for candidate in dotted_numeric_candidates(text) {
        let parts =
            candidate.split('.').filter_map(|part| part.parse::<u32>().ok()).collect::<Vec<_>>();
        if parts.len() < 2 || parts.len() > 4 {
            continue;
        }
        if looks_like_calendar_date(&parts) {
            continue;
        }
        if seen.insert(candidate.clone()) {
            terms.push(candidate);
        }
    }
    terms
}

pub fn dotted_version_key(text: &str) -> Option<[u32; 4]> {
    if let Some(candidate) = dotted_version_terms(text).into_iter().next() {
        let parts =
            candidate.split('.').filter_map(|part| part.parse::<u32>().ok()).collect::<Vec<_>>();
        let mut key = [0; 4];
        for (index, part) in parts.into_iter().enumerate() {
            key[index] = part;
        }
        return Some(key);
    }
    None
}

fn dotted_numeric_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let mut current = String::new();
    let mut dot_count = 0usize;
    for character in text.chars() {
        if character.is_ascii_digit() {
            current.push(character);
            continue;
        }
        if character == '.' && !current.is_empty() && !current.ends_with('.') {
            current.push(character);
            dot_count += 1;
            continue;
        }
        push_candidate(&mut candidates, &mut current, &mut dot_count);
    }
    push_candidate(&mut candidates, &mut current, &mut dot_count);
    candidates
}

fn push_candidate(candidates: &mut Vec<String>, current: &mut String, dot_count: &mut usize) {
    if *dot_count > 0 {
        let candidate = current.trim_end_matches('.');
        if candidate.contains('.') {
            candidates.push(candidate.to_string());
        }
    }
    current.clear();
    *dot_count = 0;
}

fn looks_like_calendar_date(parts: &[u32]) -> bool {
    parts.len() >= 3
        && (1..=31).contains(&parts[0])
        && (1..=12).contains(&parts[1])
        && parts[2] >= 1900
}

#[cfg(test)]
mod tests {
    use super::{dotted_version_key, dotted_version_terms};

    #[test]
    fn dotted_version_terms_extract_prefix_with_trailing_dot() {
        assert_eq!(dotted_version_terms("Alpha Suite 4.6."), vec!["4.6"]);
    }

    #[test]
    fn dotted_version_key_extracts_dotted_versions() {
        assert_eq!(dotted_version_key("Alpha Suite Version 2.10.3 Notes"), Some([2, 10, 3, 0]));
    }

    #[test]
    fn dotted_version_key_ignores_calendar_dates() {
        assert_eq!(dotted_version_key("Compliance notice 29.08.2019"), None);
    }
}
