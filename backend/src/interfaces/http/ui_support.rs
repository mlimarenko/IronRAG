use cookie::{Cookie, SameSite};

pub fn build_session_cookie(cookie_name: &str, value: &str, max_age_hours: u64) -> String {
    Cookie::build((cookie_name, value.to_string()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::hours(i64::try_from(max_age_hours).unwrap_or(720)))
        .build()
        .to_string()
}

pub fn build_cleared_session_cookie(cookie_name: &str) -> String {
    Cookie::build((cookie_name, String::new()))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(cookie::time::Duration::seconds(0))
        .build()
        .to_string()
}
