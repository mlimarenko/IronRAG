use serde::{Deserialize, Serialize};

use super::url_identity::HostClassification;

pub const DEFAULT_WEB_CRAWL_DEPTH: i32 = 3;
pub const DEFAULT_WEB_CRAWL_MAX_PAGES: i32 = 100;
pub const MAX_WEB_CRAWL_DEPTH: i32 = 100;
pub const MAX_WEB_CRAWL_MAX_PAGES: i32 = 10_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebIngestMode {
    SinglePage,
    RecursiveCrawl,
}

impl WebIngestMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SinglePage => "single_page",
            Self::RecursiveCrawl => "recursive_crawl",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebBoundaryPolicy {
    SameHost,
    SameHostAndSubdomains,
    AllowExternal,
}

impl WebBoundaryPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameHost => "same_host",
            Self::SameHostAndSubdomains => "same_host_and_subdomains",
            Self::AllowExternal => "allow_external",
        }
    }

    #[must_use]
    pub const fn allows_host_classification(self, classification: &HostClassification) -> bool {
        match self {
            Self::SameHost => matches!(classification, HostClassification::SameHost),
            Self::SameHostAndSubdomains => matches!(
                classification,
                HostClassification::SameHost | HostClassification::SeedSubdomain
            ),
            Self::AllowExternal => true,
        }
    }
}

mod runs;
#[cfg(test)]
mod tests;
mod url_filters;

pub use runs::*;
pub use url_filters::*;
