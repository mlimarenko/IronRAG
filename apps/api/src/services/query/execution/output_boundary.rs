pub(super) fn strip_trailing_media_source_token(answer: &str) -> Option<String> {
    let mut trimmed = answer.trim_end();
    let mut removed = false;
    loop {
        let token_end = if trimmed.ends_with('`') {
            trimmed
        } else {
            trimmed.trim_end_matches(sentence_terminal_char)
        };
        let Some(without_closing_tick) = token_end.strip_suffix('`') else {
            break;
        };
        let Some((prefix, token)) = without_closing_tick.rsplit_once('`') else {
            break;
        };
        let substantive_prefix = prefix.trim_end();
        if !token_is_media_filename(token.trim())
            || !substantive_prefix_ends_sentence(substantive_prefix)
        {
            break;
        }
        trimmed = substantive_prefix;
        removed = true;
    }
    removed.then(|| trimmed.to_string())
}

pub(super) fn token_is_media_filename(token: &str) -> bool {
    token_is_filename_shaped(token) && media_filename_extension(token)
}

fn token_is_filename_shaped(token: &str) -> bool {
    if token.is_empty()
        || token.contains('/')
        || token.contains('\\')
        || token.chars().any(char::is_whitespace)
    {
        return false;
    }
    let Some((stem, extension)) = token.rsplit_once('.') else {
        return false;
    };
    !stem.is_empty()
        && (2..=5).contains(&extension.chars().count())
        && extension.chars().all(|ch| ch.is_ascii_alphanumeric())
}

fn media_filename_extension(token: &str) -> bool {
    matches!(
        token.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "svg")
    )
}

fn substantive_prefix_ends_sentence(value: &str) -> bool {
    value
        .chars()
        .rev()
        .find(|ch| !matches!(ch, '"' | '\'' | '`' | ')' | ']' | '}' | '»' | '”' | '’'))
        .is_some_and(sentence_terminal_char)
}

fn sentence_terminal_char(ch: char) -> bool {
    matches!(ch, '.' | '!' | '?' | '。' | '！' | '？')
}

#[cfg(test)]
mod tests {
    use super::{strip_trailing_media_source_token, token_is_media_filename};

    #[test]
    fn strips_bare_trailing_media_source_token_after_complete_sentence() {
        assert_eq!(
            strip_trailing_media_source_token("Use the documented remediation. `example.png`")
                .as_deref(),
            Some("Use the documented remediation.")
        );
        assert!(strip_trailing_media_source_token("Upload `example.png`").is_none());
        assert!(token_is_media_filename("example.PNG"));
    }

    #[test]
    fn strips_multiple_trailing_media_source_tokens() {
        assert_eq!(
            strip_trailing_media_source_token(
                "Use the documented remediation. `first.png`. `second.png`"
            )
            .as_deref(),
            Some("Use the documented remediation.")
        );
    }
}
