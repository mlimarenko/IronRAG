use std::collections::{BTreeSet, HashMap};

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::{
    domains::{
        agent_runtime::RuntimeLifecycleState,
        query::{QueryTurnKind, QueryVerificationState},
    },
    infra::{
        knowledge_rows::{
            KnowledgeBundleChunkReferenceRow, KnowledgeBundleEntityReferenceRow,
            KnowledgeBundleRelationReferenceRow, KnowledgeChunkRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow, KnowledgeEvidenceRow,
            KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
        },
        repositories::query_repository,
    },
    services::query::execution::{
        QueryChunkReferenceSnapshot, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    },
};

use super::{
    ExternalConversationTurn, MAX_DETAIL_GRAPH_EDGE_REFERENCES, MAX_DETAIL_GRAPH_NODE_REFERENCES,
    MAX_DETAIL_PREPARED_SEGMENT_REFERENCES, MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION,
    MAX_DETAIL_TECHNICAL_FACT_REFERENCES, QUERY_CONVERSATION_TITLE_LIMIT, RankedBundleReference,
    context::bundle_references::{
        derive_fact_rank_refs, seed_chunk_refs_from_answer_context,
        seed_entity_refs_from_answer_graph, seed_relation_endpoint_refs_from_answer_graph,
        seed_relation_refs_from_answer_graph, select_diversified_fact_ids,
        selected_fact_ids_for_detail,
    },
    formatting::{
        build_prepared_segment_references, map_chunk_references, map_entity_references,
        map_execution_runtime_summary, map_relation_references, parse_query_verification_state,
    },
    session::{
        build_conversation_runtime_context,
        build_conversation_runtime_context_with_external_history,
        build_prior_grounded_answer_context_messages, derive_conversation_title,
        normalize_explicit_conversation_title, select_prior_grounded_answer_replay_executions,
    },
};

mod context_references;
mod conversation;
mod prepared_segments;
