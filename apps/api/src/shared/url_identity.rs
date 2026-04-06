use reqwest::Url;
use std::fmt;

const TRACKING_PARAM_PREFIXES: &[&str] = &["utm_", "mc_"];
const TRACKING_PARAM_NAMES: &[&str] = &["fbclid", "gclid", "dclid", "msclkid", "ref", "source"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostClassification {
    SameHost,
    External,
}

impl HostClassification {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameHost => "same_host",
            Self::External => "external",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlIdentityError {
    message: String,
}

impl UrlIdentityError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for UrlIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for UrlIdentityError {}

/// Normalizes the configured seed URL into its canonical HTTP or HTTPS form.
///
/// # Errors
///
/// Returns a [`UrlIdentityError`] when the URL is invalid or uses a non-HTTP scheme.
pub fn normalize_seed_url(seed_url: &str) -> Result<String, UrlIdentityError> {
    normalize_absolute_url(seed_url)
}

/// Normalizes an absolute HTTP or HTTPS URL by stripping tracking data and
/// canonicalizing host and scheme details.
///
/// # Errors
///
/// Returns a [`UrlIdentityError`] when the URL cannot be parsed or is not HTTP(S).
pub fn normalize_absolute_url(url: &str) -> Result<String, UrlIdentityError> {
    let parsed = parse_http_url(url)?;
    Ok(normalize_url(parsed).to_string())
}

/// Resolves a discovered link against a base HTTP or HTTPS URL and normalizes it.
///
/// # Errors
///
/// Returns a [`UrlIdentityError`] when either URL is invalid or uses a non-HTTP scheme.
pub fn resolve_discovered_url(base_url: &str, href: &str) -> Result<String, UrlIdentityError> {
    let base = parse_http_url(base_url)?;
    let joined = base
        .join(href.trim())
        .map_err(|error| UrlIdentityError::new(format!("invalid discovered url: {error}")))?;
    ensure_http_scheme(&joined)?;
    Ok(normalize_url(joined).to_string())
}

/// Classifies whether a candidate URL shares the same host as the seed URL.
///
/// # Errors
///
/// Returns a [`UrlIdentityError`] when either URL is invalid or uses a non-HTTP scheme.
pub fn classify_host(
    seed_url: &str,
    candidate_url: &str,
) -> Result<HostClassification, UrlIdentityError> {
    let seed = parse_http_url(seed_url)?;
    let candidate = parse_http_url(candidate_url)?;
    let seed_host =
        seed.host_str().ok_or_else(|| UrlIdentityError::new("seed url host is missing"))?;
    let candidate_host = candidate
        .host_str()
        .ok_or_else(|| UrlIdentityError::new("candidate url host is missing"))?;
    if seed_host.eq_ignore_ascii_case(candidate_host) {
        Ok(HostClassification::SameHost)
    } else {
        Ok(HostClassification::External)
    }
}

fn parse_http_url(raw: &str) -> Result<Url, UrlIdentityError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UrlIdentityError::new("url is required"));
    }
    let parsed = Url::parse(trimmed)
        .map_err(|error| UrlIdentityError::new(format!("invalid url: {error}")))?;
    ensure_http_scheme(&parsed)?;
    Ok(parsed)
}

fn ensure_http_scheme(url: &Url) -> Result<(), UrlIdentityError> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        _ => Err(UrlIdentityError::new("only http and https urls are supported")),
    }
}

fn normalize_url(mut url: Url) -> Url {
    url.set_fragment(None);
    let should_strip_port = (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443));
    if should_strip_port {
        let _ = url.set_port(None);
    }
    if let Some(host) = url.host_str() {
        let lower = host.to_ascii_lowercase();
        let _ = url.set_host(Some(&lower));
    }
    if let Some(query) = url.query() {
        let retained = url
            .query_pairs()
            .filter(|(name, _)| !is_tracking_query_param(name.as_ref()))
            .map(|(name, value)| (name.into_owned(), value.into_owned()))
            .collect::<Vec<_>>();
        if retained.is_empty() && !query.is_empty() {
            url.set_query(None);
        } else {
            let mut pairs = url.query_pairs_mut();
            pairs.clear();
            for (name, value) in retained {
                pairs.append_pair(&name, &value);
            }
        }
    }
    if url.path().is_empty() {
        url.set_path("/");
    }
    url
}

fn is_tracking_query_param(name: &str) -> bool {
    TRACKING_PARAM_NAMES.iter().any(|candidate| candidate.eq_ignore_ascii_case(name))
        || TRACKING_PARAM_PREFIXES
            .iter()
            .any(|prefix| name.to_ascii_lowercase().starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::{
        HostClassification, classify_host, normalize_absolute_url, resolve_discovered_url,
    };

    #[test]
    fn normalize_strips_tracking_query_and_fragment() {
        let normalized = match normalize_absolute_url(
            "https://Docs.Example.com:443/path?q=1&utm_source=test#heading",
        ) {
            Ok(value) => value,
            Err(error) => panic!("normalize url: {error}"),
        };
        assert_eq!(normalized, "https://docs.example.com/path?q=1");
    }

    #[test]
    fn resolve_relative_url_against_base() {
        let normalized =
            match resolve_discovered_url("https://docs.example.com/guide/index.html", "../api") {
                Ok(value) => value,
                Err(error) => panic!("resolve discovered url: {error}"),
            };
        assert_eq!(normalized, "https://docs.example.com/api");
    }

    #[test]
    fn classify_same_host_vs_external() {
        assert_eq!(
            match classify_host("https://docs.example.com/start", "https://docs.example.com/api",) {
                Ok(value) => value,
                Err(error) => panic!("classify: {error}"),
            },
            HostClassification::SameHost
        );
        assert_eq!(
            match classify_host("https://docs.example.com/start", "https://example.org/api") {
                Ok(value) => value,
                Err(error) => panic!("classify: {error}"),
            },
            HostClassification::External
        );
    }
}
