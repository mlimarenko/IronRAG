/// How many sentences of surrounding context we attach to each indexed
/// sentence when forming a retrieval window. Three on each side gives the
/// reader a paragraph-sized neighbourhood without flooding the prompt.
const DEFAULT_WINDOW_SIZE: usize = 3;

/// A single sentence with the local paragraph it lives in.
///
/// `sentence` is what gets embedded — short, focused, the precise unit a
/// query is matched against. `window_text` is what gets surfaced to the
/// answer model — wide enough to carry the surrounding facts that turn a
/// single matched sentence into a useful citation.
///
/// The pair implements LlamaIndex's "small embed, large context" pattern
/// without changing the rest of the retrieve pipeline: at index time we
/// produce both fields, store them on the chunk row, and at retrieve time
/// swap `chunk.text` for `chunk.window_text` before the context assembler
/// builds the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub(super) struct SentenceWindow {
    pub sentence: String,
    pub window_text: String,
}

/// Slice an arbitrary block of text into sentence-sized windows. The split
/// is naive on purpose — we never reach for an external NLP toolkit on the
/// hot path. Whitespace-collapsed text is broken on Latin-style sentence
/// terminators while keeping abbreviation-style "Inc." or "v1.2." attached
/// to the preceding word: any terminator that is followed by another word
/// character rather than whitespace stays inside the current sentence.
///
/// The function is script-agnostic: it operates on Unicode codepoints and
/// never enumerates per-language terminators or keywords. If the input is
/// effectively a single sentence (no terminator), it is returned as a
/// single window whose `sentence` and `window_text` are equal.
#[allow(dead_code)]
pub(super) fn build_sentence_windows(text: &str) -> Vec<SentenceWindow> {
    build_sentence_windows_with_size(text, DEFAULT_WINDOW_SIZE)
}

fn build_sentence_windows_with_size(text: &str, window_size: usize) -> Vec<SentenceWindow> {
    let sentences = split_sentences(text);
    if sentences.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(sentences.len());
    for (index, sentence) in sentences.iter().enumerate() {
        let start = index.saturating_sub(window_size);
        let end = (index + window_size + 1).min(sentences.len());
        let window_text = sentences[start..end].join(" ");
        out.push(SentenceWindow { sentence: sentence.clone(), window_text });
    }
    out
}

fn split_sentences(text: &str) -> Vec<String> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut sentences = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = normalized.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        current.push(*ch);
        if !is_sentence_terminator(*ch) {
            continue;
        }
        let next = chars.get(index + 1).copied();
        let after_next = chars.get(index + 2).copied();
        // Treat the terminator as a real sentence boundary only when what
        // follows is whitespace plus an uppercase-like character. This keeps
        // abbreviations (e.g. "v1.2.3 release") inside the current sentence.
        let is_boundary = match (next, after_next) {
            (Some(ws), Some(letter)) if ws.is_whitespace() && starts_new_sentence(letter) => true,
            (Some(ws), None) if ws.is_whitespace() => true,
            (None, _) => true,
            _ => false,
        };
        if is_boundary {
            let trimmed = current.trim();
            if !trimmed.is_empty() {
                sentences.push(trimmed.to_string());
            }
            current.clear();
        }
    }
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        sentences.push(trimmed.to_string());
    }
    sentences
}

fn is_sentence_terminator(ch: char) -> bool {
    // Sentence-terminal codepoints across the major writing systems. We
    // enumerate by codepoint rather than literal character so the patterns
    // never collide with their ASCII look-alikes after editor normalization:
    // U+002E full stop, U+0021 exclamation mark, U+003F question mark,
    // U+3002 ideographic full stop, U+FF01 fullwidth exclamation mark,
    // U+FF1F fullwidth question mark.
    matches!(ch as u32, 0x002E | 0x0021 | 0x003F | 0x3002 | 0xFF01 | 0xFF1F)
}

fn starts_new_sentence(ch: char) -> bool {
    // Heuristic that holds across Latin / Cyrillic / Greek / fullwidth
    // scripts: uppercase letters and digits begin a new sentence; other
    // characters (lowercase, punctuation) usually do not.
    ch.is_uppercase() || ch.is_ascii_digit()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_sentence_input_returns_one_window() {
        let windows = build_sentence_windows("This text has no terminator inside it");
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].sentence, "This text has no terminator inside it");
        assert_eq!(windows[0].window_text, "This text has no terminator inside it");
    }

    #[test]
    fn three_sentences_emit_three_windows_each_with_full_context() {
        let text = "First sentence here. Second sentence here. Third sentence here.";
        let windows = build_sentence_windows(text);
        assert_eq!(windows.len(), 3);
        // With window size 3, every sentence sees all three in this corpus.
        for window in &windows {
            assert!(window.window_text.contains("First sentence here."));
            assert!(window.window_text.contains("Second sentence here."));
            assert!(window.window_text.contains("Third sentence here."));
        }
        assert_eq!(windows[0].sentence, "First sentence here.");
        assert_eq!(windows[1].sentence, "Second sentence here.");
        assert_eq!(windows[2].sentence, "Third sentence here.");
    }

    #[test]
    fn window_respects_window_size() {
        let text = "A1. B2. C3. D4. E5. F6. G7.";
        let windows = build_sentence_windows_with_size(text, 1);
        assert_eq!(windows.len(), 7);
        // Middle sentence sees one neighbour on each side.
        assert_eq!(windows[3].sentence, "D4.");
        assert_eq!(windows[3].window_text, "C3. D4. E5.");
        // First sentence cannot reach backwards.
        assert_eq!(windows[0].window_text, "A1. B2.");
        // Last sentence cannot reach forwards.
        assert_eq!(windows[6].window_text, "F6. G7.");
    }

    #[test]
    fn abbreviation_inside_sentence_does_not_split() {
        let text = "Release v1.2.3 ships next month. The next minor is v1.3.0.";
        let windows = build_sentence_windows(text);
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0].sentence, "Release v1.2.3 ships next month.");
        assert_eq!(windows[1].sentence, "The next minor is v1.3.0.");
    }

    #[test]
    fn empty_or_whitespace_input_returns_empty() {
        assert!(build_sentence_windows("").is_empty());
        assert!(build_sentence_windows("    \n\n  ").is_empty());
    }

    #[test]
    fn question_and_exclamation_mark_split_sentences() {
        let text = "Is this working? Yes! It is.";
        let windows = build_sentence_windows(text);
        assert_eq!(windows.len(), 3);
        assert_eq!(windows[0].sentence, "Is this working?");
        assert_eq!(windows[1].sentence, "Yes!");
        assert_eq!(windows[2].sentence, "It is.");
    }
}
