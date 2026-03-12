#[must_use]
pub fn split_text_into_chunks(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();

    for raw_block in text.split("\n\n") {
        let block = raw_block.trim();
        if block.is_empty() {
            continue;
        }

        if current.is_empty() {
            if block.len() <= max_chars {
                current.push_str(block);
            } else {
                split_long_block(block, max_chars, &mut chunks);
            }
            continue;
        }

        if current.len() + 2 + block.len() <= max_chars {
            current.push_str("\n\n");
            current.push_str(block);
        } else {
            chunks.push(current);
            current = String::new();
            if block.len() <= max_chars {
                current.push_str(block);
            } else {
                split_long_block(block, max_chars, &mut chunks);
            }
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }

    chunks
}

fn split_long_block(block: &str, max_chars: usize, out: &mut Vec<String>) {
    let mut current = String::new();
    for sentence in block.split_terminator(['.', '!', '?']) {
        let sentence = sentence.trim();
        if sentence.is_empty() {
            continue;
        }
        let sentence = format!("{}. ", sentence.trim_end_matches(['.', '!', '?']));
        if current.len() + sentence.len() <= max_chars {
            current.push_str(&sentence);
        } else {
            if !current.trim().is_empty() {
                out.push(current.trim().to_string());
            }
            if sentence.len() > max_chars {
                let mut rest = sentence.as_str();
                while rest.len() > max_chars {
                    let (head, tail) = rest.split_at(max_chars);
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
