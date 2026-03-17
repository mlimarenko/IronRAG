#[derive(Debug, Clone, Copy)]
pub struct ChunkingProfile {
    pub max_chars: usize,
    pub overlap_chars: usize,
}

impl Default for ChunkingProfile {
    fn default() -> Self {
        Self { max_chars: 1200, overlap_chars: 120 }
    }
}

#[must_use]
pub fn split_text_into_chunks(text: &str, max_chars: usize) -> Vec<String> {
    split_text_into_chunks_with_profile(text, ChunkingProfile { max_chars, overlap_chars: 0 })
}

#[must_use]
pub fn split_text_into_chunks_with_profile(text: &str, profile: ChunkingProfile) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for raw_block in text.split("\n\n") {
        let block = raw_block.trim();
        if block.is_empty() {
            continue;
        }

        if current.is_empty() {
            if char_count(block) <= profile.max_chars {
                current.push_str(block);
            } else {
                split_long_block(block, profile.max_chars, &mut chunks);
            }
            continue;
        }

        if char_count(&current) + 2 + char_count(block) <= profile.max_chars {
            current.push_str("\n\n");
            current.push_str(block);
        } else {
            push_chunk_with_overlap(&mut chunks, &current, profile.overlap_chars);
            current = String::new();
            if char_count(block) <= profile.max_chars {
                current.push_str(block);
            } else {
                split_long_block(block, profile.max_chars, &mut chunks);
            }
        }
    }

    if !current.is_empty() {
        push_chunk_with_overlap(&mut chunks, &current, 0);
    }

    chunks
}

fn push_chunk_with_overlap(out: &mut Vec<String>, chunk: &str, overlap_chars: usize) {
    if chunk.trim().is_empty() {
        return;
    }

    out.push(chunk.to_string());

    if overlap_chars == 0 {
        return;
    }

    let overlap = trailing_chars(chunk, overlap_chars);
    if !overlap.is_empty() {
        out.push(overlap);
    }
}

fn trailing_chars(input: &str, max_chars: usize) -> String {
    let mut chars = input.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

fn char_count(input: &str) -> usize {
    input.chars().count()
}

fn split_at_char_count(input: &str, max_chars: usize) -> (&str, &str) {
    if max_chars == 0 {
        return ("", input);
    }

    let split_idx = input.char_indices().nth(max_chars).map_or(input.len(), |(idx, _)| idx);
    input.split_at(split_idx)
}

fn split_long_block(block: &str, max_chars: usize, out: &mut Vec<String>) {
    let mut current = String::new();
    for sentence in block.split_terminator(['.', '!', '?']) {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        let sentence = format!("{}. ", sentence.trim_end_matches(['.', '!', '?']));
        if char_count(&current) + char_count(&sentence) <= max_chars {
            current.push_str(&sentence);
        } else {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
            }
            if char_count(&sentence) > max_chars {
                let mut rest = sentence.as_str();
                while char_count(rest) > max_chars {
                    let (head, tail) = split_at_char_count(rest, max_chars);
                    out.push(head.trim().to_string());
                    rest = tail;
                }
                current = rest.to_string();
            } else {
                current = sentence;
            }
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::{ChunkingProfile, char_count, split_text_into_chunks_with_profile};

    #[test]
    fn splits_cyrillic_text_without_utf8_boundary_panic() {
        let text = "Это длинный русский текст без латиницы, который должен разбиваться безопасно. "
            .repeat(60);

        let chunks = split_text_into_chunks_with_profile(
            &text,
            ChunkingProfile { max_chars: 120, overlap_chars: 0 },
        );

        assert!(!chunks.is_empty());
        assert!(chunks.iter().all(|chunk| char_count(chunk) <= 120));
        assert!(chunks.iter().all(|chunk| !chunk.trim().is_empty()));
    }

    #[test]
    fn respects_multibyte_boundaries_in_single_long_sentence() {
        let text = format!("{}!", "я".repeat(245));

        let chunks = split_text_into_chunks_with_profile(
            &text,
            ChunkingProfile { max_chars: 100, overlap_chars: 0 },
        );

        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| char_count(chunk) <= 100));
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().all(|ch| ch == 'я' || matches!(ch, '.' | '!' | ' ')))
        );
        assert!(chunks.iter().map(|chunk| char_count(chunk)).sum::<usize>() >= 245);
    }
}
