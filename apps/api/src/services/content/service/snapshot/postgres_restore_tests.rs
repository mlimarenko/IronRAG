use super::*;

/// `sanitize_json_for_postgres` must strip U+0000 (null byte) and lone
/// surrogates from all string nodes at any depth while leaving every other
/// code point — including multibyte non-ASCII — intact.
#[test]
fn sanitize_json_strips_null_bytes_and_surrogates_preserves_multibyte() {
    // A string mixing a legitimate multibyte character, a null byte, and
    // surrounding ASCII — only the null must be removed.
    let mut v = serde_json::json!("hello\u{0000}wörld");
    sanitize_json_for_postgres(&mut v);
    assert_eq!(v.as_str().unwrap(), "hellowörld", "null byte removed; multibyte char kept");

    // Nested: null byte inside an object value inside an array.
    let mut nested = serde_json::json!([
        { "text": "abc\u{0000}def", "num": 42 },
        "ok\u{0000}"
    ]);
    sanitize_json_for_postgres(&mut nested);
    assert_eq!(nested[0]["text"].as_str().unwrap(), "abcdef", "null stripped from object value",);
    assert_eq!(nested[0]["num"].as_i64().unwrap(), 42, "numeric node untouched",);
    assert_eq!(nested[1].as_str().unwrap(), "ok", "null stripped from array string");

    // A string with no forbidden characters must be returned unchanged
    // (fast-path: no allocation). Deliberately script-agnostic: mixes
    // Latin-extended, Greek, CJK, a 4-byte astral math glyph and an emoji
    // so the multibyte-preservation invariant is exercised without
    // embedding any natural-language phrase.
    let clean = "é·Ω·中·𝛼·🌍";
    let mut v2 = serde_json::json!(clean);
    sanitize_json_for_postgres(&mut v2);
    assert_eq!(v2.as_str().unwrap(), clean, "clean string unchanged");

    // A string consisting entirely of null bytes becomes empty.
    let mut all_null = serde_json::json!("\u{0000}\u{0000}");
    sanitize_json_for_postgres(&mut all_null);
    assert_eq!(all_null.as_str().unwrap(), "", "all-null string becomes empty");
}
