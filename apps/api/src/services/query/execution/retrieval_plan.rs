//! Structural planning and failure policy for retrieval companion lanes.
//!
//! The primary lexical and vector searches are always attempted. Companion
//! lanes are compiled from typed query structure and resolved targets so a
//! generic query does not pay for every specialized retrieval strategy.

use anyhow::Context;
use serde_json::Value;
use thiserror::Error;

use crate::{
    domains::{
        query_ir::{QueryAct, QueryIR},
        retrieval::{DEFAULT_TEXT_SEARCH_CONFIG, RetrievalConfig},
    },
    services::query::latest_versions::query_requests_latest_versions,
};

/// A specialized retrieval lane that augments the lexical/vector primary set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum RetrievalLane {
    DocumentIdentity,
    LatestVersion,
    LatestVersionSemantic,
    EntityBio,
    QueryIrFocus,
    ContentAnchor,
    DocumentEvidenceAnchor,
    VersionedUpdateProcedure,
    VersionedUpdateSourceLocal,
    SetupFocus,
    SetupVariant,
    LinkedAnchorContext,
    ArtifactSiblingSource,
}

impl RetrievalLane {
    pub(crate) const ALL: [Self; 13] = [
        Self::DocumentIdentity,
        Self::LatestVersion,
        Self::LatestVersionSemantic,
        Self::EntityBio,
        Self::QueryIrFocus,
        Self::ContentAnchor,
        Self::DocumentEvidenceAnchor,
        Self::VersionedUpdateProcedure,
        Self::VersionedUpdateSourceLocal,
        Self::SetupFocus,
        Self::SetupVariant,
        Self::LinkedAnchorContext,
        Self::ArtifactSiblingSource,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::DocumentIdentity => "document_identity",
            Self::LatestVersion => "latest_version",
            Self::LatestVersionSemantic => "latest_version_semantic",
            Self::EntityBio => "entity_bio",
            Self::QueryIrFocus => "query_ir_focus",
            Self::ContentAnchor => "content_anchor",
            Self::DocumentEvidenceAnchor => "document_evidence_anchor",
            Self::VersionedUpdateProcedure => "versioned_update_procedure",
            Self::VersionedUpdateSourceLocal => "versioned_update_source_local",
            Self::SetupFocus => "setup_focus",
            Self::SetupVariant => "setup_variant",
            Self::LinkedAnchorContext => "linked_anchor_context",
            Self::ArtifactSiblingSource => "artifact_sibling_source",
        }
    }

    pub(crate) const fn span_name(self) -> &'static str {
        match self {
            Self::DocumentIdentity => "retrieve.document_identity",
            Self::LatestVersion => "retrieve.latest_version",
            Self::LatestVersionSemantic => "retrieve.latest_version_semantic",
            Self::EntityBio => "retrieve.entity_bio",
            Self::QueryIrFocus => "retrieve.query_ir_focus",
            Self::ContentAnchor => "retrieve.content_anchor",
            Self::DocumentEvidenceAnchor => "retrieve.document_evidence_anchor",
            Self::VersionedUpdateProcedure => "retrieve.versioned_update_procedure",
            Self::VersionedUpdateSourceLocal => "retrieve.versioned_update_source_local",
            Self::SetupFocus => "retrieve.setup_focus",
            Self::SetupVariant => "retrieve.setup_variant",
            Self::LinkedAnchorContext => "retrieve.linked_anchor_context",
            Self::ArtifactSiblingSource => "retrieve.artifact_sibling_source",
        }
    }
}

/// Whether a failed lane can safely fall back to already-loaded primary evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LaneCriticality {
    Required,
    Optional,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaneSpec {
    pub(crate) lane: RetrievalLane,
    pub(crate) criticality: LaneCriticality,
}

/// How a latest-version lane was activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LatestSelectionKind {
    None,
    Explicit,
}

/// Structural facts known after primary retrieval and target resolution.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RetrievalPlanningContext<'a> {
    pub(crate) query_ir: Option<&'a QueryIR>,
    pub(crate) has_target_documents: bool,
    pub(crate) has_focus_queries: bool,
    pub(crate) has_content_anchor: bool,
    pub(crate) has_document_evidence_anchor: bool,
    pub(crate) latest_selection: LatestSelectionKind,
    pub(crate) versioned_update_intent: bool,
    pub(crate) setup_intent: bool,
}

pub(crate) fn explicit_content_anchor_requested(
    query_ir: Option<&QueryIR>,
    has_target_documents: bool,
) -> bool {
    has_target_documents
        || query_ir.is_some_and(|query_ir| {
            query_ir.document_focus.is_some() || !query_ir.literal_constraints.is_empty()
        })
}

/// Immutable companion-lane plan for one retrieval pass.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RetrievalPlan {
    lanes: Vec<LaneSpec>,
}

impl RetrievalPlan {
    pub(crate) fn compile(context: RetrievalPlanningContext<'_>) -> Self {
        let mut lanes = Vec::new();
        let mut push = |lane, criticality| lanes.push(LaneSpec { lane, criticality });

        if context.has_target_documents {
            push(RetrievalLane::DocumentIdentity, LaneCriticality::Required);
        }
        if !matches!(context.latest_selection, LatestSelectionKind::None) {
            push(RetrievalLane::LatestVersion, LaneCriticality::Required);
        }
        if matches!(context.latest_selection, LatestSelectionKind::Explicit) {
            push(RetrievalLane::LatestVersionSemantic, LaneCriticality::Optional);
        }

        let explicit_latest = context.query_ir.is_some_and(query_requests_latest_versions);
        let entity_bio_intent = context.query_ir.is_some_and(|query_ir| {
            matches!(query_ir.act, QueryAct::Describe)
                && !query_ir.target_entities.is_empty()
                && !explicit_latest
                && !context.versioned_update_intent
                && !context.setup_intent
        });
        if entity_bio_intent {
            push(RetrievalLane::EntityBio, LaneCriticality::Optional);
        }
        if context.has_focus_queries {
            push(RetrievalLane::QueryIrFocus, LaneCriticality::Optional);
        }
        if context.has_content_anchor {
            push(RetrievalLane::ContentAnchor, LaneCriticality::Optional);
        }
        if context.has_document_evidence_anchor {
            push(RetrievalLane::DocumentEvidenceAnchor, LaneCriticality::Optional);
        }
        if context.versioned_update_intent {
            push(RetrievalLane::VersionedUpdateProcedure, LaneCriticality::Optional);
            push(RetrievalLane::VersionedUpdateSourceLocal, LaneCriticality::Optional);
        }
        if context.setup_intent {
            push(RetrievalLane::SetupFocus, LaneCriticality::Optional);
            push(RetrievalLane::SetupVariant, LaneCriticality::Optional);
        }
        if context.has_content_anchor || context.has_document_evidence_anchor {
            push(RetrievalLane::LinkedAnchorContext, LaneCriticality::Optional);
        }
        if context.setup_intent || context.versioned_update_intent {
            push(RetrievalLane::ArtifactSiblingSource, LaneCriticality::Optional);
        }

        Self { lanes }
    }

    pub(crate) fn spec(&self, lane: RetrievalLane) -> Option<LaneSpec> {
        self.lanes.iter().copied().find(|spec| spec.lane == lane)
    }

    pub(crate) fn planned_names(&self) -> String {
        self.lanes.iter().map(|spec| spec.lane.name()).collect::<Vec<_>>().join(",")
    }

    pub(crate) const fn planned_count(&self) -> usize {
        self.lanes.len()
    }

    pub(crate) fn skipped_count(&self) -> usize {
        RetrievalLane::ALL.len().saturating_sub(self.lanes.len())
    }

    pub(crate) fn skipped_names(&self) -> String {
        RetrievalLane::ALL
            .into_iter()
            .filter(|lane| self.spec(*lane).is_none())
            .map(RetrievalLane::name)
            .collect::<Vec<_>>()
            .join(",")
    }

    #[cfg(test)]
    pub(crate) fn lanes(&self) -> &[LaneSpec] {
        &self.lanes
    }
}

/// Central result of applying companion-lane failure policy.
#[derive(Debug)]
pub(crate) enum LaneResolution<T> {
    Ready(T),
    Degraded(anyhow::Error),
}

/// Required failures always abort. Optional failures degrade only when the
/// primary vector/lexical set already contains evidence.
pub(crate) fn resolve_lane_result<T>(
    spec: LaneSpec,
    result: anyhow::Result<T>,
    primary_evidence_available: bool,
) -> anyhow::Result<LaneResolution<T>> {
    match result {
        Ok(value) => Ok(LaneResolution::Ready(value)),
        Err(error)
            if matches!(spec.criticality, LaneCriticality::Optional)
                && primary_evidence_available =>
        {
            Ok(LaneResolution::Degraded(error))
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "{} retrieval lane {} failed",
                match spec.criticality {
                    LaneCriticality::Required => "required",
                    LaneCriticality::Optional => "optional-without-primary",
                },
                spec.lane.name()
            )
        }),
    }
}

/// Returns a cached primary result during a focus-broaden replan. The execute
/// closure is lazy, which guarantees embeddings/vector/lexical work is not
/// launched again when the first pass already produced a reusable snapshot.
pub(crate) async fn reuse_or_execute_primary<T, E, Execute, ExecuteFuture>(
    reusable: Option<T>,
    execute: Execute,
) -> Result<(T, bool), E>
where
    Execute: FnOnce() -> ExecuteFuture,
    ExecuteFuture: std::future::Future<Output = Result<T, E>>,
{
    if let Some(reusable) = reusable {
        return Ok((reusable, true));
    }
    execute().await.map(|value| (value, false))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum RetrievalPlanningError {
    #[error("stored retrieval config is invalid: {0}")]
    InvalidStoredConfig(String),
}

/// Missing catalog rows use the historical default; malformed persisted JSON
/// is an operational data error and must not silently change search semantics.
pub(crate) fn resolve_text_search_config(
    stored_config: Option<Value>,
) -> Result<String, RetrievalPlanningError> {
    let Some(stored_config) = stored_config else {
        return Ok(DEFAULT_TEXT_SEARCH_CONFIG.to_string());
    };
    RetrievalConfig::from_json(stored_config)
        .map(|config| config.lexical.text_search_config)
        .map_err(RetrievalPlanningError::InvalidStoredConfig)
}
