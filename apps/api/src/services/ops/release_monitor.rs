use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use reqwest::header::{ACCEPT, USER_AGENT};
use semver::Version;
use serde::Deserialize;
use tokio::sync::RwLock;

const RELEASE_CHECK_TIMEOUT_SECONDS: u64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReleaseUpdateStatus {
    UpToDate,
    UpdateAvailable,
    Unknown,
}

impl ReleaseUpdateStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UpToDate => "up_to_date",
            Self::UpdateAvailable => "update_available",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseUpdateSnapshot {
    pub status: ReleaseUpdateStatus,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub release_url: Option<String>,
    pub repository_url: String,
    pub checked_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct ReleaseMonitorService {
    repository: String,
    check_interval_hours: u64,
    cache: Arc<RwLock<Option<ReleaseUpdateSnapshot>>>,
}

#[derive(Deserialize)]
struct GithubTagPayload {
    name: String,
}

impl ReleaseMonitorService {
    #[must_use]
    pub fn new(repository: String, check_interval_hours: u64) -> Self {
        Self { repository, check_interval_hours, cache: Arc::default() }
    }

    pub async fn get_release_update(&self, current_version: &str) -> ReleaseUpdateSnapshot {
        let normalized_current_version = normalize_version_label(current_version);

        if let Some(snapshot) = self.read_fresh_snapshot(normalized_current_version.as_str()).await
        {
            return snapshot;
        }

        match fetch_release_update(normalized_current_version.as_str(), self.repository.as_str())
            .await
        {
            Ok(snapshot) => {
                *self.cache.write().await = Some(snapshot.clone());
                snapshot
            }
            Err(_) => self
                .read_cached_snapshot(normalized_current_version.as_str())
                .await
                .unwrap_or_else(|| {
                    unknown_release_update(
                        normalized_current_version.as_str(),
                        self.repository.as_str(),
                    )
                }),
        }
    }

    async fn read_fresh_snapshot(&self, current_version: &str) -> Option<ReleaseUpdateSnapshot> {
        self.read_cached_snapshot(current_version).await.filter(|snapshot| {
            snapshot.checked_at
                + ChronoDuration::hours(
                    i64::try_from(self.check_interval_hours).unwrap_or(i64::MAX),
                )
                > Utc::now()
        })
    }

    async fn read_cached_snapshot(&self, current_version: &str) -> Option<ReleaseUpdateSnapshot> {
        self.cache
            .read()
            .await
            .as_ref()
            .filter(|snapshot| snapshot.current_version == current_version)
            .cloned()
    }
}

async fn fetch_release_update(
    current_version: &str,
    repository: &str,
) -> Result<ReleaseUpdateSnapshot, reqwest::Error> {
    let current = match parse_release_version(current_version) {
        Some(version) => version,
        None => return Ok(unknown_release_update(current_version, repository)),
    };
    let repository_url = release_repository_url(repository);

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(RELEASE_CHECK_TIMEOUT_SECONDS))
        .build()?;

    let tags = client
        .get(release_tags_api_url(repository))
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, format!("IronRAG/{current_version}"))
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<GithubTagPayload>>()
        .await?;

    let checked_at = Utc::now();
    let latest = select_latest_release_version(tags.iter().map(|tag| tag.name.as_str()));

    let Some(latest) = latest else {
        return Ok(ReleaseUpdateSnapshot {
            status: ReleaseUpdateStatus::Unknown,
            current_version: current.to_string(),
            latest_version: None,
            release_url: None,
            repository_url,
            checked_at,
        });
    };

    let update_available = latest > current;
    let latest_version = latest.to_string();

    Ok(ReleaseUpdateSnapshot {
        status: if update_available {
            ReleaseUpdateStatus::UpdateAvailable
        } else {
            ReleaseUpdateStatus::UpToDate
        },
        current_version: current.to_string(),
        latest_version: Some(latest_version.clone()),
        release_url: Some(format!("{repository_url}/releases/tag/v{latest_version}")),
        repository_url,
        checked_at,
    })
}

fn unknown_release_update(current_version: &str, repository: &str) -> ReleaseUpdateSnapshot {
    ReleaseUpdateSnapshot {
        status: ReleaseUpdateStatus::Unknown,
        current_version: normalize_version_label(current_version),
        latest_version: None,
        release_url: None,
        repository_url: release_repository_url(repository),
        checked_at: Utc::now(),
    }
}

fn release_repository_url(repository: &str) -> String {
    format!("https://github.com/{repository}")
}

fn release_tags_api_url(repository: &str) -> String {
    format!("https://api.github.com/repos/{repository}/tags?per_page=100")
}

fn select_latest_release_version<'a>(tags: impl Iterator<Item = &'a str>) -> Option<Version> {
    tags.filter_map(parse_release_version).max()
}

fn parse_release_version(value: &str) -> Option<Version> {
    let normalized = normalize_version_label(value);
    let version = Version::parse(&normalized).ok()?;
    if version.pre.is_empty() && version.build.is_empty() { Some(version) } else { None }
}

fn normalize_version_label(value: &str) -> String {
    value.trim().trim_start_matches('v').to_string()
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_version_label, parse_release_version, release_repository_url,
        release_tags_api_url, select_latest_release_version,
    };

    #[test]
    fn normalizes_optional_v_prefix() {
        assert_eq!(normalize_version_label("v0.1.2"), "0.1.2");
        assert_eq!(normalize_version_label("0.1.2"), "0.1.2");
    }

    #[test]
    fn parses_only_stable_semver_tags() {
        assert!(parse_release_version("v0.1.2").is_some());
        assert!(parse_release_version("0.1.2").is_some());
        assert!(parse_release_version("release-0.1.2").is_none());
        assert!(parse_release_version("v0.1.2-rc.1").is_none());
    }

    #[test]
    fn selects_latest_semver_tag() {
        let tags = ["v0.1.0", "v0.1.2", "v0.1.1", "junk", "v0.1.2-rc.1"];
        let latest = select_latest_release_version(tags.into_iter());
        assert_eq!(latest.map(|value| value.to_string()), Some("0.1.2".to_string()));
    }

    #[test]
    fn builds_repository_urls_from_configured_slug() {
        assert_eq!(release_repository_url("example/project"), "https://github.com/example/project");
        assert_eq!(
            release_tags_api_url("example/project"),
            "https://api.github.com/repos/example/project/tags?per_page=100"
        );
    }
}
