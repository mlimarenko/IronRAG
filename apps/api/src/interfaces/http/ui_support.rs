use axum::http::HeaderMap;
use cookie::{Cookie, SameSite};

pub(super) fn session_cookie_secure_for_request(
    configured_secure: bool,
    headers: &HeaderMap,
) -> bool {
    configured_secure
        || comma_header_contains(headers, "x-forwarded-proto", "https")
        || comma_header_contains(headers, "x-forwarded-scheme", "https")
        || comma_header_contains(headers, "x-forwarded-ssl", "on")
        || forwarded_header_has_https_proto(headers)
}

pub(super) fn build_session_cookie(
    cookie_name: &str,
    value: &str,
    max_age_hours: u64,
    secure: bool,
) -> String {
    Cookie::build((cookie_name, value.to_string()))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::hours(i64::try_from(max_age_hours).unwrap_or(720)))
        .build()
        .to_string()
}

pub(super) fn build_cleared_session_cookie(cookie_name: &str, secure: bool) -> String {
    Cookie::build((cookie_name, String::new()))
        .path("/")
        .http_only(true)
        .secure(secure)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::seconds(0))
        .build()
        .to_string()
}

fn comma_header_contains(headers: &HeaderMap, name: &'static str, expected: &str) -> bool {
    headers.get_all(name).iter().any(|value| {
        value.to_str().ok().is_some_and(|raw| {
            raw.split(',').map(str::trim).any(|item| item.eq_ignore_ascii_case(expected))
        })
    })
}

fn forwarded_header_has_https_proto(headers: &HeaderMap) -> bool {
    headers.get_all("forwarded").iter().any(|value| {
        value.to_str().ok().is_some_and(|raw| {
            raw.split(',').any(|entry| {
                entry.split(';').any(|part| {
                    let mut pieces = part.splitn(2, '=');
                    let key = pieces.next().unwrap_or_default().trim();
                    let value = pieces.next().unwrap_or_default().trim().trim_matches('"');
                    key.eq_ignore_ascii_case("proto") && value.eq_ignore_ascii_case("https")
                })
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::{
        build_cleared_session_cookie, build_session_cookie, session_cookie_secure_for_request,
    };

    #[test]
    fn secure_session_cookie_sets_secure_attribute() {
        let cookie = build_session_cookie("ironrag_ui_session", "session-value", 24, true);

        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn local_session_cookie_omits_secure_attribute() {
        let cookie = build_session_cookie("ironrag_ui_session", "session-value", 24, false);

        assert!(cookie.contains("HttpOnly"));
        assert!(!cookie.contains("Secure"));
    }

    #[test]
    fn cleared_cookie_matches_secure_session_policy() {
        let cookie = build_cleared_session_cookie("ironrag_ui_session", true);

        assert!(cookie.contains("Max-Age=0"));
        assert!(cookie.contains("Secure"));
    }

    #[test]
    fn forwarded_https_marks_session_cookie_secure() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", "https".parse().unwrap());

        assert!(session_cookie_secure_for_request(false, &headers));
    }

    #[test]
    fn forwarded_standard_proto_marks_session_cookie_secure() {
        let mut headers = HeaderMap::new();
        headers.insert("forwarded", "for=192.0.2.60;proto=https".parse().unwrap());

        assert!(session_cookie_secure_for_request(false, &headers));
    }

    #[test]
    fn local_request_without_https_signal_keeps_session_cookie_not_secure() {
        assert!(!session_cookie_secure_for_request(false, &HeaderMap::new()));
    }
}
