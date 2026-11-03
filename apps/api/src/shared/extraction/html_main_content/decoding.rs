use encoding_rs::{Encoding, UTF_8};

use super::{DecodedHtml, HTML_CHARSET_SCAN_BYTES};

pub(super) fn decode_html(file_bytes: &[u8], mime_type: Option<&str>) -> DecodedHtml {
    let encoding = charset_from_mime_type(mime_type)
        .or_else(|| sniff_charset_from_html(file_bytes))
        .and_then(|label| Encoding::for_label(label.as_bytes()).map(|encoding| (label, encoding)))
        .unwrap_or_else(|| ("utf-8".to_string(), UTF_8));
    let (decoded, _, had_errors) = encoding.1.decode(file_bytes);
    DecodedHtml { content: decoded.into_owned(), charset: encoding.0, had_errors }
}

fn charset_from_mime_type(mime_type: Option<&str>) -> Option<String> {
    mime_type.and_then(|value| {
        value.split(';').skip(1).find_map(|segment| {
            let (name, raw_value) = segment.split_once('=')?;
            if name.trim().eq_ignore_ascii_case("charset") {
                Some(raw_value.trim().trim_matches('"').to_ascii_lowercase())
            } else {
                None
            }
        })
    })
}

fn sniff_charset_from_html(file_bytes: &[u8]) -> Option<String> {
    let prefix_len = file_bytes.len().min(HTML_CHARSET_SCAN_BYTES);
    let prefix = String::from_utf8_lossy(&file_bytes[..prefix_len]).to_ascii_lowercase();
    find_html_charset_assignment(&prefix)
}

fn find_html_charset_assignment(prefix: &str) -> Option<String> {
    let charset_index = prefix.find("charset=")?;
    let remainder = &prefix[charset_index + "charset=".len()..];
    let trimmed = remainder
        .trim_start_matches(|ch: char| ch.is_ascii_whitespace() || ch == '"' || ch == '\'');
    let value = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>();
    (!value.is_empty()).then_some(value)
}
