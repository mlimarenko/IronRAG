mod answer_context;
mod assembly;
mod chunk_ranking;
mod document_rendering;
mod entity_context;
mod library_context;
mod references;

pub(crate) use answer_context::{assemble_answer_context, load_retrieved_document_briefs};
#[cfg(test)]
pub(crate) use assembly::BoundedContextQuery;
#[cfg(test)]
pub(crate) use assembly::assemble_bounded_context;
pub(crate) use assembly::{
    BoundedContextInventoryQuery, assemble_bounded_context_for_query,
    assemble_bounded_context_for_query_with_inventory_candidates,
    declaration_inventory_document_id, prepend_focused_declaration_context,
    query_requests_declaration_inventory,
};
pub(crate) use entity_context::{
    should_prioritize_retrieved_context_for_query, target_entity_context_lines,
};
pub(crate) use library_context::load_query_execution_library_context;
#[cfg(test)]
pub(crate) use references::build_references;
pub(crate) use references::{
    apply_query_execution_library_summary, apply_query_execution_warning,
    assemble_context_metadata_for_query, build_grouped_reference_candidates,
    build_structured_query_diagnostics, group_visible_references_for_query,
};

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
