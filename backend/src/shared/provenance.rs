#[must_use]
pub fn build_chunk_reference(document_id: &str, ordinal: i32) -> String {
    format!("document:{document_id}:chunk:{ordinal}")
}
