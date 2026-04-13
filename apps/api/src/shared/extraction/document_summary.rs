#[derive(Clone, Copy)]
pub struct DocumentSummaryBlock<'a> {
    pub block_kind: &'a str,
    pub text: &'a str,
}

pub const DOCUMENT_SUMMARY_CHAR_LIMIT: usize = 2_000;

#[must_use]
pub fn build_document_summary_from_blocks<'a>(
    blocks: impl IntoIterator<Item = DocumentSummaryBlock<'a>>,
) -> String {
    let mut parts = Vec::new();
    let mut chars_used = 0_usize;

    for block in blocks {
        if chars_used >= DOCUMENT_SUMMARY_CHAR_LIMIT {
            break;
        }

        let text = block.text.trim();
        if text.is_empty() {
            continue;
        }

        if text.chars().count() < 10 && block.block_kind != "heading" {
            continue;
        }

        let remaining = DOCUMENT_SUMMARY_CHAR_LIMIT.saturating_sub(chars_used);
        let truncated = truncate_to_char_limit(text, remaining);
        if truncated.is_empty() {
            continue;
        }

        parts.push(truncated.to_string());
        chars_used += truncated.chars().count();
    }

    parts.join(" ").trim().to_string()
}

fn truncate_to_char_limit(text: &str, max_chars: usize) -> &str {
    if text.chars().count() <= max_chars {
        return text;
    }
    text.char_indices().nth(max_chars).map_or(text, |(index, _)| &text[..index])
}

#[cfg(test)]
mod tests {
    use super::{
        DOCUMENT_SUMMARY_CHAR_LIMIT, DocumentSummaryBlock, build_document_summary_from_blocks,
    };

    #[test]
    fn keeps_heading_blocks_even_when_short() {
        let summary = build_document_summary_from_blocks([
            DocumentSummaryBlock { block_kind: "heading", text: "Overview" },
            DocumentSummaryBlock {
                block_kind: "paragraph",
                text: "Detailed explanation of the system behavior.",
            },
        ]);

        assert_eq!(summary, "Overview Detailed explanation of the system behavior.");
    }

    #[test]
    fn skips_tiny_non_heading_blocks() {
        let summary = build_document_summary_from_blocks([
            DocumentSummaryBlock { block_kind: "paragraph", text: "short" },
            DocumentSummaryBlock {
                block_kind: "paragraph",
                text: "This block is long enough to be preserved.",
            },
        ]);

        assert_eq!(summary, "This block is long enough to be preserved.");
    }

    #[test]
    fn truncates_without_crossing_char_boundaries() {
        let text = "я".repeat(DOCUMENT_SUMMARY_CHAR_LIMIT + 8);
        let summary = build_document_summary_from_blocks([DocumentSummaryBlock {
            block_kind: "paragraph",
            text: &text,
        }]);

        assert_eq!(summary.chars().count(), DOCUMENT_SUMMARY_CHAR_LIMIT);
    }
}
