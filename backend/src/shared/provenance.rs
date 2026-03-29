#[must_use]
pub fn build_chunk_reference(document_id: &str, ordinal: i32) -> String {
    format!("document:{document_id}:chunk:{ordinal}")
}

#[must_use]
pub fn build_page_reference(document_id: &str, page_number: u32) -> String {
    format!("document:{document_id}:page:{page_number}")
}

#[must_use]
pub fn build_graph_node_reference(library_id: &str, canonical_key: &str) -> String {
    format!("library:{library_id}:node:{canonical_key}")
}

#[must_use]
pub fn build_graph_edge_reference(library_id: &str, canonical_key: &str) -> String {
    format!("library:{library_id}:edge:{canonical_key}")
}
