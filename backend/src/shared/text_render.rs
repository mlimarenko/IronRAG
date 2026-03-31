fn first_token(text: &str) -> Option<&str> {
    text.split_whitespace().next()
}

fn last_token(text: &str) -> Option<&str> {
    text.split_whitespace().last()
}

fn is_protocol_split(left: &str, right: &str) -> bool {
    matches!(left, "http" | "https") && right.starts_with("://")
}

fn is_path_continuation(left: &str, right: &str) -> bool {
    (left.starts_with('/') || left.contains("://")) && right.starts_with('/')
}

fn is_ascii_fragment_split(left: &str, right: &str) -> bool {
    if !left.is_ascii() || !right.is_ascii() {
        return false;
    }
    if left.len() > 32 || right.len() > 32 {
        return false;
    }

    let Some(left_last) = left.chars().last() else {
        return false;
    };
    let Some(right_first) = right.chars().next() else {
        return false;
    };

    let left_joinable =
        left_last.is_ascii_lowercase() || left_last.is_ascii_digit() || matches!(left_last, '_' | '/' | ':' | '.');
    let right_joinable =
        right_first.is_ascii_lowercase() || right_first.is_ascii_digit() || right_first == '_';
    if !left_joinable || !right_joinable {
        return false;
    }

    if left.chars().all(|ch| !ch.is_ascii_lowercase()) {
        return false;
    }

    let left_tail_since_uppercase = left
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_ascii_uppercase())
        .map_or(left.len(), |(index, _)| left.len().saturating_sub(index));
    let right_starts_underscore = right.starts_with('_');
    let left_all_lower_or_digits =
        left.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit());
    let left_fragment_like = (right_starts_underscore && left_all_lower_or_digits)
        || (left_all_lower_or_digits && left.len() <= 4)
        || left_tail_since_uppercase <= 3;
    let right_fragment_like = right.starts_with('_')
        || (right.len() <= 5 && right.chars().all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit()))
        || (right.len() <= 8
            && right.chars().next().is_some_and(|ch| ch.is_ascii_lowercase())
            && right.chars().skip(1).any(|ch| ch.is_ascii_uppercase()));

    left_fragment_like && right_fragment_like
}

fn should_join_without_separator(previous: &str, current: &str) -> bool {
    let Some(left) = last_token(previous) else {
        return false;
    };
    let Some(right) = first_token(current) else {
        return false;
    };

    is_protocol_split(left, right)
        || is_path_continuation(left, right)
        || is_ascii_fragment_split(left, right)
}

#[must_use]
pub fn repair_technical_layout_noise(content: &str) -> String {
    let mut repaired_lines = Vec::<String>::new();

    for raw_line in content.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        if let Some(previous) = repaired_lines.last_mut() {
            if should_join_without_separator(previous, line) {
                previous.push_str(line);
                continue;
            }
        }

        repaired_lines.push(line.to_string());
    }

    repaired_lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::repair_technical_layout_noise;

    #[test]
    fn repair_technical_layout_noise_joins_ascii_identifier_fragments() {
        let repaired = repair_technical_layout_noise(
            "pageNu\nmber\nwithCar\nds\nnumber\n_starting\ninte\nger\nboo\nlean",
        );

        assert!(repaired.contains("pageNumber"));
        assert!(repaired.contains("withCards"));
        assert!(repaired.contains("number_starting"));
        assert!(repaired.contains("integer"));
        assert!(repaired.contains("boolean"));
    }

    #[test]
    fn repair_technical_layout_noise_joins_protocol_and_paths() {
        let repaired = repair_technical_layout_noise(
            "http\n://localhost:8080/ACC/rest/v1/accounts\n/bypage\n/system/info",
        );

        assert!(repaired.contains("http://localhost:8080/ACC/rest/v1/accounts/bypage"));
        assert!(repaired.contains("/system/info"));
    }

    #[test]
    fn repair_technical_layout_noise_does_not_join_uppercase_headings() {
        let repaired = repair_technical_layout_noise("REST\nAPI\nGET");

        assert_eq!(repaired, "REST\nAPI\nGET");
    }
}
