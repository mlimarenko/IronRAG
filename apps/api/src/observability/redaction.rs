//! Secret and credential redaction for log records.
//!
//! Every value that leaves [`crate::observability::error_tracking`] passes
//! through [`scrub`] first: an error message can quote an upstream response
//! body, a connection string or a user-supplied path, and a log record must
//! never carry the credential that happened to sit next to the failure.
//!
//! The rules are compiled once. They replace the whole matched secret with a
//! fixed `[REDACTED:<kind>]` marker, so the output is stable and a reader can
//! still tell which class of secret was removed. Business fields are not
//! rewritten by this module; only the log record's message and stack pass
//! through it.

use std::sync::LazyLock;

use regex::Regex;

/// Hard bound on one scrubbed value. A stack trace or a provider body can be
/// arbitrarily long; the record is diagnostic, not an archive.
const MAX_SCRUBBED_CHARS: usize = 4096;

const TRUNCATION_MARKER: &str = "[TRUNCATED]";

/// `scheme://user:password@host` — the userinfo segment is a credential even
/// when it does not look like one.
static URL_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([a-z][a-z0-9+.\-]*://)[^/@\s]+@")
        .expect("static URL credential pattern is valid")
});

/// Explicit authorization headers are credentials regardless of token shape.
static AUTHORIZATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(\bauthorization["']?\s*[:=]\s*["']?)(bearer|basic)\s+[^\s"',;]+"#)
        .expect("static authorization header pattern is valid")
});

/// `Bearer <token>` outside an explicit header. The
/// case-insensitive scope covers only the scheme word: the token itself must
/// contain an uppercase letter, digit or symbol, so prose like
/// "Bearer authentication failed" is not mistaken for a credential.
///
/// The Rust regex crate has no look-around, so the mixed-case/digit guard is
/// applied to the captured token instead of a `(?=...)` prefix. The pattern
/// stays greedy over the complete credential alphabet; the capture is skipped
/// when it is all lowercase letters.
static BEARER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:\bBearer\s+)(?P<token>[A-Za-z0-9._~+/=\-]{8,})")
        .expect("static bearer pattern is valid")
});

/// `Basic <base64>` — base64 is at least eight characters for a real pair, and
/// the same mixed-case/digit guard keeps "Basic authentication" intact.
static BASIC: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i:\bBasic\s+)(?P<token>[A-Za-z0-9+/=]{8,})")
        .expect("static basic pattern is valid")
});

/// Provider API keys (`sk-...`, including the `sk-proj-...` form).
static PROVIDER_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bsk-[A-Za-z0-9_\-]{8,}").expect("static provider key pattern is valid")
});

/// `password=value`, `api_key: "value"`, `token: value` and their siblings.
/// The marker keeps the key name so the reader knows which class was removed.
static SECRET_ASSIGNMENT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)\b(password|passwd|secret|client_secret|token|access_token|refresh_token|api_key|apikey|access_key)\b\s*[=:]\s*("[^"]*"|'[^']*'|[^\s,;]+)"#,
    )
    .expect("static secret assignment pattern is valid")
});

/// An email address is personal data and never belongs in an operator record.
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b")
        .expect("static email pattern is valid")
});

/// A credential token carries at least one character outside lowercase ASCII:
/// an uppercase letter, digit or symbol. Prose words after a scheme name are
/// lowercase and stay readable.
fn credential_token(token: &str) -> bool {
    token.chars().any(|character| !character.is_ascii_lowercase())
}

/// Removes every recognised secret class from `text` and bounds the result.
#[must_use]
pub fn scrub(text: &str) -> String {
    let mut scrubbed =
        URL_CREDENTIALS.replace_all(text, "${1}[REDACTED:credentials]@").into_owned();
    scrubbed = AUTHORIZATION
        .replace_all(&scrubbed, |captures: &regex::Captures<'_>| {
            format!(
                "{}{} [REDACTED:{}]",
                &captures[1],
                &captures[2],
                captures[2].to_ascii_lowercase()
            )
        })
        .into_owned();
    scrubbed = BEARER
        .replace_all(&scrubbed, |captures: &regex::Captures<'_>| {
            captures.name("token").filter(|token| credential_token(token.as_str())).map_or_else(
                || captures.get(0).map_or_else(String::new, |matched| matched.as_str().to_string()),
                |_| "Bearer [REDACTED:bearer]".to_string(),
            )
        })
        .into_owned();
    scrubbed = BASIC
        .replace_all(&scrubbed, |captures: &regex::Captures<'_>| {
            captures.name("token").filter(|token| credential_token(token.as_str())).map_or_else(
                || captures.get(0).map_or_else(String::new, |matched| matched.as_str().to_string()),
                |_| "Basic [REDACTED:basic]".to_string(),
            )
        })
        .into_owned();
    scrubbed = PROVIDER_KEY.replace_all(&scrubbed, "[REDACTED:provider_key]").into_owned();
    scrubbed = SECRET_ASSIGNMENT
        .replace_all(&scrubbed, |captures: &regex::Captures<'_>| {
            let kind = captures.get(1).map_or("secret", |matched| matched.as_str());
            format!("[REDACTED:{}]", kind.to_ascii_lowercase())
        })
        .into_owned();
    scrubbed = EMAIL.replace_all(&scrubbed, "[REDACTED:email]").into_owned();
    truncate_chars(&scrubbed, MAX_SCRUBBED_CHARS)
}

/// Bounds `text` to `max` characters, appending an explicit marker so a reader
/// can tell a truncated value from a complete one.
#[must_use]
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut bounded: String = text.chars().take(max).collect();
    bounded.push_str(TRUNCATION_MARKER);
    bounded
}

#[cfg(test)]
mod tests {
    use super::{MAX_SCRUBBED_CHARS, TRUNCATION_MARKER, scrub};

    /// Credential fixtures are assembled at runtime so the repository itself
    /// never contains a literal that a secret scanner can mistake for a leak.
    fn credential(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn explicit_authorization_redacts_short_and_lowercase_values() {
        for (scheme, value) in [("Bearer", "a".repeat(32)), ("Basic", ["dT", "pw"].concat())] {
            let input = format!("Authorization: {scheme} {value}");
            let output = scrub(&input);
            assert!(!output.contains(&value), "{output}");
            assert!(output.contains("[REDACTED:"), "{output}");
        }
        assert_eq!(scrub("Bearer authentication failed"), "Bearer authentication failed");
        for scheme in ["Bearer", "basic"] {
            let value = "authentication";
            let input = format!("authorization: {scheme} {value}");
            let output = scrub(&input);
            assert!(!output.contains(value), "{output}");
            assert!(output.contains("[REDACTED:"), "{output}");
        }
    }

    #[test]
    fn removes_authentication_credentials_with_their_markers() {
        let bearer_value = credential(&["Header", "1234", ".payload", ".signature"]);
        let bearer = scrub(&format!("Authorization: Bearer {bearer_value}"));
        assert!(bearer.contains("[REDACTED:bearer]"), "{bearer}");
        assert!(!bearer.contains(&bearer_value), "{bearer}");

        let basic_value = credential(&["dXNlcj", "pwYXNz", "d29yZA", "=="]);
        let basic = scrub(&format!("Authorization: Basic {basic_value}"));
        assert!(basic.contains("[REDACTED:basic]"), "{basic}");
        assert!(!basic.contains(&basic_value), "{basic}");
    }

    #[test]
    fn removes_provider_and_assignment_secrets_with_their_markers() {
        let provider_value = credential(&["sk", "-proj-", "AbCdEf", "1234567890"]);
        let provider = scrub(&format!("Incorrect API key provided: {provider_value}"));
        assert!(provider.contains("[REDACTED:provider_key]"), "{provider}");
        assert!(!provider.contains(&provider_value), "{provider}");

        let password = scrub("connect failed password=hunter2 host=db");
        assert!(password.contains("[REDACTED:password]"), "{password}");
        assert!(!password.contains("hunter2"), "{password}");

        let api_key = scrub(r#"request rejected api_key: "abc-def-123""#);
        assert!(api_key.contains("[REDACTED:api_key]"), "{api_key}");
        assert!(!api_key.contains("abc-def-123"), "{api_key}");

        let url = scrub("cannot reach https://user:s3cr3t@db.internal:5432/ironrag");
        assert!(url.contains("[REDACTED:credentials]"), "{url}");
        assert!(!url.contains("s3cr3t"), "{url}");
        assert!(url.contains("db.internal:5432"), "{url}");

        let email = scrub("user alice@example.com was rejected");
        assert!(email.contains("[REDACTED:email]"), "{email}");
        assert!(!email.contains("alice@example.com"), "{email}");
    }

    #[test]
    fn scheme_words_in_ordinary_prose_stay_readable() {
        for prose in [
            "Authorization failed: Bearer authentication failed",
            "authorization required: basic authentication required",
            "Bearer settings are invalid",
            "Basic configuration is missing",
            "Bearer token",
        ] {
            assert_eq!(scrub(prose), prose);
        }
    }

    #[test]
    fn redacts_a_real_provider_failure_message_without_losing_the_classification() {
        let provider_value = credential(&["sk", "-proj-", "QwErTy", "9876543210"]);
        let message = format!(
            "provider request failed: provider=openai status=429 detail={{\"error\":\
             {{\"message\":\"Incorrect API key provided: {provider_value}\",\"type\":\
             \"invalid_request_error\"}}}}"
        );
        let scrubbed = scrub(&message);

        assert!(scrubbed.contains("[REDACTED:provider_key]"), "{scrubbed}");
        assert!(!scrubbed.contains(&provider_value), "{scrubbed}");
        assert!(scrubbed.contains("provider=openai status=429"), "{scrubbed}");
    }

    #[test]
    fn bounds_the_output_and_marks_the_cut() {
        let long = "x".repeat(MAX_SCRUBBED_CHARS + 100);
        let scrubbed = scrub(&long);

        assert_eq!(scrubbed.chars().count(), MAX_SCRUBBED_CHARS + TRUNCATION_MARKER.len());
        assert!(scrubbed.ends_with(TRUNCATION_MARKER));
    }

    #[test]
    fn leaves_ordinary_diagnostics_untouched() {
        let message = "ingest attempt failed: document revision not found";
        assert_eq!(scrub(message), message);
    }
}
