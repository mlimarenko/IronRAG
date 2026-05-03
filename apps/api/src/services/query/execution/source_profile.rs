use crate::infra::arangodb::document_store::KnowledgeChunkRow;

use super::RuntimeMatchedChunk;

pub(crate) const SOURCE_PROFILE_CHUNK_KIND: &str = "source_profile";
const SOURCE_PROFILE_TEXT_PREFIX: &str = "[source_profile ";
const RECORD_STREAM_SEQUENCE_MARKER: &str = "sequence_kind=record_stream";

pub(crate) fn is_source_profile_kind(kind: Option<&str>) -> bool {
    kind == Some(SOURCE_PROFILE_CHUNK_KIND)
}

pub(crate) fn is_source_profile_text(text: &str) -> bool {
    text.trim_start().starts_with(SOURCE_PROFILE_TEXT_PREFIX)
}

pub(crate) fn is_source_profile_chunk_row(row: &KnowledgeChunkRow) -> bool {
    is_source_profile_kind(row.chunk_kind.as_deref())
        || is_source_profile_text(&row.normalized_text)
        || is_source_profile_text(&row.content_text)
}

pub(crate) fn is_record_stream_source_profile_text(text: &str) -> bool {
    let trimmed = text.trim_start();
    is_source_profile_text(trimmed) && trimmed.contains(RECORD_STREAM_SEQUENCE_MARKER)
}

pub(crate) fn is_record_stream_source_profile_row(row: &KnowledgeChunkRow) -> bool {
    is_record_stream_source_profile_text(&row.normalized_text)
        || is_record_stream_source_profile_text(&row.content_text)
}

pub(crate) fn is_source_profile_runtime_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    is_source_profile_kind(chunk.chunk_kind.as_deref())
        || is_source_profile_text(&chunk.source_text)
        || is_source_profile_text(&chunk.excerpt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_stream_profile_requires_canonical_sequence_marker() {
        assert!(is_record_stream_source_profile_text(
            "[source_profile source_format=record_jsonl sequence_kind=record_stream unit_count=3]"
        ));
        assert!(!is_record_stream_source_profile_text(
            "[source_profile source_format=record_jsonl unit_count=3]"
        ));
    }

    #[test]
    fn non_record_stream_profile_is_not_ordered_slice_eligible() {
        assert!(!is_record_stream_source_profile_text(
            "[source_profile source_format=plain_text unit_count=3]"
        ));
    }
}
