use std::collections::{HashSet, VecDeque};

use chrono::Utc;
use futures::stream::{self, StreamExt};
use tracing::error;
use uuid::Uuid;

use super::{
    ApiError, AppState, FetchedWebResource, HostClassification, UpdateAsyncOperationCommand,
    UpdateMutationCommand, UpdateWebIngestRun, WebCandidateState, WebClassificationReason,
    WebDiscoveredPageRow, WebIngestRunRow, WebIngestService, WebRunFailure, WebRunFailureCode,
    WebRunState, derive_terminal_run_state, extract_html_preferred_url, ingest_repository,
    is_direct_image_web_resource, map_web_run_counts_row, now_if_terminal, parse_run_url_filter,
    payload_looks_like_html_document, source_file_name_from_url, telemetry,
};
use crate::infra::repositories::ingest_repository::NewWebDiscoveredPage;
use crate::services::{
    ingest::service::AdmitIngestJobCommand, ops::service::CreateAsyncOperationCommand,
};
use crate::shared::web::ingest::{
    WebIngestUrlFilter, classify_web_crawl_filter_exclusion,
    classify_web_materialization_filter_exclusion, parse_web_boundary_policy,
};

mod discovery;
mod lifecycle;
mod queue;

pub(super) use discovery::*;
pub(super) use lifecycle::*;
pub(super) use queue::*;
