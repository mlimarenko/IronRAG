mod relevance;
mod retrieval;
mod target_profiles;
#[cfg(test)]
mod tests;

pub(super) use relevance::GraphQueryRelevanceProfile;
#[cfg(test)]
pub(crate) use relevance::query_relevant_entity_hits;
pub(crate) use relevance::query_relevant_graph_evidence_target_hits;
// Preserve the crate-visible function surface across the mechanical split.
#[allow(unused_imports)]
pub(crate) use retrieval::{
    GraphRetrievalRequest, associative_edges_for_entities, entities_from_relationships,
    map_edge_hit, merge_primary_then_expanded_entities, merge_relationships,
    retrieve_global_bundle, retrieve_local_bundle, retrieve_mixed_graph_bundle,
    retrieve_relationship_hits, score_desc_entities, score_desc_relationships,
};
pub(crate) use target_profiles::{
    GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind, GraphTargetEntityProfile,
    graph_target_entity_coverage_score, graph_target_entity_profiles,
};

#[cfg(test)]
use super::score_value;
#[cfg(test)]
use relevance::{graph_relevance_profile, lexical_entity_hits, lexical_relationship_hits};
#[cfg(test)]
use retrieval::{merge_entity_retrieval_lane_slices, merge_entity_retrieval_lanes};
