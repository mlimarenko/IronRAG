//! Structural, script-agnostic capture of acronym/abbreviation aliases from
//! chunk text.
//!
//! When a document writes a short form next to its full form as a parenthetical
//! gloss `<full form> ( <short> )`, the short form is an alias of that entity.
//! Attaching it to the entity's `aliases_json` lets the alias-driven retrieval
//! lanes (entity-bio search, graph anchor profiles) resolve a later short-form
//! query to the correct concept node instead of a vague vector neighbourhood.
//!
//! One detector, purely structural (no language/script branching, no stop-word
//! or keyword lists). It relates a candidate short token to a specific
//! full-form `label`, so the alias attaches only to the node whose evidence is
//! the current chunk — a different sense glossed in a different chunk attaches
//! to its own node, preserving polysemy.
//!
//! - **Parenthetical gloss**: the chunk contains `<phrase> ( <short> )` where
//!   the per-word initials of `<phrase>` equal `<short>` and `<phrase>` is the
//!   `label` itself. The full form physically precedes the parenthesised short
//!   form, which is the highest-precision structural signal available.
//!
//! Every candidate short token is admitted only through the existing shape
//! gate [`literal_text_is_identifier_shaped`], which accepts all-uppercase
//! acronyms (any writing system) and rejects lowercase prose words.

use std::collections::BTreeSet;

use crate::domains::query_ir::literal_text_is_identifier_shaped;

/// Upper bound on detected aliases returned for one (chunk, label) pair.
/// A single full form maps to at most a couple of short forms in practice;
/// the cap guards against pathological inputs without affecting real glosses.
const MAX_ALIASES_PER_LABEL: usize = 4;

/// Detects short-form aliases in `chunk_text` that abbreviate the multi-word
/// `label` via a parenthetical gloss (`<label> ( <short> )`).
///
/// Returns the original-case short tokens (sorted, deduplicated). Empty when
/// `label` is not multi-word or the chunk carries no parenthetical gloss for it.
#[must_use]
pub(crate) fn detect_acronym_aliases_for_label(chunk_text: &str, label: &str) -> Vec<String> {
    let Some(label_initials) = phrase_initials(&word_tokens(label)) else {
        return Vec::new();
    };
    // A meaningful acronym abbreviates at least two words.
    if label_initials.chars().count() < 2 {
        return Vec::new();
    }

    let mut aliases: BTreeSet<String> = BTreeSet::new();
    collect_parenthetical_gloss_aliases(chunk_text, label, &label_initials, &mut aliases);

    aliases.into_iter().take(MAX_ALIASES_PER_LABEL).collect()
}

/// Detects full-form phrase aliases in `chunk_text` glossed by the short,
/// identifier-shaped `short_label` via a parenthetical gloss
/// (`<phrase> ( <short_label> )`).
///
/// The reverse of [`detect_acronym_aliases_for_label`]: that function, given a
/// multi-word full form, returns the short token; this one, given a node whose
/// label *is* the short token, returns the preceding full-form phrase. Both are
/// driven only by the parenthetical gloss physically present in the chunk, so
/// the alias is evidence-local — the caller restricts detection to the short
/// node's own evidence chunks, and a short label glossed differently in another
/// chunk attaches its own full form to its own node, preserving polysemy.
///
/// Returns the single-spaced, original-case full-form phrases (sorted,
/// deduplicated). Empty when `short_label` is not an identifier-shaped acronym
/// of at least two characters or the chunk carries no parenthetical gloss that
/// names it with matching per-word initials.
#[must_use]
pub(crate) fn detect_fullform_aliases_for_short_label(
    chunk_text: &str,
    short_label: &str,
) -> Vec<String> {
    let short = short_label.trim();
    // The short label must itself be an identifier-shaped acronym of >= 2 chars
    // carrying at least one alphabetic character (mirrors `short_matches_initials`,
    // so the reverse direction admits exactly the tokens the forward one emits).
    if !literal_text_is_identifier_shaped(short)
        || !short.chars().any(char::is_alphabetic)
        || short.chars().filter(|ch| ch.is_alphanumeric()).count() < 2
    {
        return Vec::new();
    }
    let short_upper = short.to_uppercase();

    let mut aliases: BTreeSet<String> = BTreeSet::new();
    collect_parenthetical_fullform_aliases(chunk_text, &short_upper, &mut aliases);
    aliases.into_iter().take(MAX_ALIASES_PER_LABEL).collect()
}

/// Splits `text` into maximal runs of alphanumeric characters. Punctuation and
/// whitespace are separators, so `"<phrase> (AS)."` yields the tokens of the
/// phrase plus `"AS"`. Unicode-aware via [`char::is_alphanumeric`].
fn word_tokens(text: &str) -> Vec<&str> {
    text.split(|ch: char| !ch.is_alphanumeric()).filter(|token| !token.is_empty()).collect()
}

/// Per-word initials of a token sequence, uppercased. `None` when fewer than
/// one token contributes an alphabetic initial. A token whose first character
/// is not alphabetic (e.g. a bare number) contributes nothing and aborts the
/// phrase, since such a phrase does not form a clean acronym.
fn phrase_initials(tokens: &[&str]) -> Option<String> {
    if tokens.is_empty() {
        return None;
    }
    let mut initials = String::new();
    for token in tokens {
        let first = token.chars().next()?;
        if !first.is_alphabetic() {
            return None;
        }
        initials.extend(first.to_uppercase());
    }
    Some(initials)
}

/// True when the identifier-shaped `short` is an acronym of `label_initials`.
fn short_matches_initials(short: &str, label_initials: &str) -> bool {
    literal_text_is_identifier_shaped(short)
        && short.to_uppercase() == label_initials
        && short.chars().any(char::is_alphabetic)
}

/// Detector A — parenthetical gloss `<label> ( <short> )`.
///
/// Scans each parenthesised span; when its trimmed content is a single
/// identifier-shaped token whose initials match the words immediately
/// preceding the `(`, and those words spell `label`, the token is admitted.
fn collect_parenthetical_gloss_aliases(
    chunk_text: &str,
    label: &str,
    label_initials: &str,
    aliases: &mut BTreeSet<String>,
) {
    let label_tokens = word_tokens(label);
    if label_tokens.is_empty() {
        return;
    }

    let chars: Vec<char> = chunk_text.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '(' {
            index += 1;
            continue;
        }
        let Some(close_offset) = chars[index + 1..].iter().position(|&ch| ch == ')') else {
            break;
        };
        let inner: String = chars[index + 1..index + 1 + close_offset].iter().collect();
        let preceding: String = chars[..index].iter().collect();
        index += close_offset + 2;

        let short = inner.trim();
        if !short_matches_initials(short, label_initials) {
            continue;
        }
        // The words right before the `(` must spell exactly the label.
        let preceding_tokens = word_tokens(&preceding);
        if preceding_tokens.len() < label_tokens.len() {
            continue;
        }
        let tail = &preceding_tokens[preceding_tokens.len() - label_tokens.len()..];
        if phrase_eq_ignore_case(tail, &label_tokens) {
            aliases.insert(short.to_string());
        }
    }
}

/// Reverse detector A — parenthetical gloss `<phrase> ( <short> )` keyed by the
/// short form rather than the full form.
///
/// Scans each parenthesised span; when its trimmed content is a single
/// identifier-shaped token whose uppercase equals `short_upper`, the trailing
/// run of words immediately preceding the `(` whose per-word initials spell
/// `short_upper` is admitted as a full-form alias (single-spaced, original case).
fn collect_parenthetical_fullform_aliases(
    chunk_text: &str,
    short_upper: &str,
    aliases: &mut BTreeSet<String>,
) {
    let short_len = short_upper.chars().count();
    let chars: Vec<char> = chunk_text.chars().collect();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '(' {
            index += 1;
            continue;
        }
        let Some(close_offset) = chars[index + 1..].iter().position(|&ch| ch == ')') else {
            break;
        };
        let inner: String = chars[index + 1..index + 1 + close_offset].iter().collect();
        let preceding: String = chars[..index].iter().collect();
        index += close_offset + 2;

        let short = inner.trim();
        if !short_matches_initials(short, short_upper) {
            continue;
        }
        // The trailing `short_len` words before the `(` must spell the short form.
        let preceding_tokens = word_tokens(&preceding);
        if preceding_tokens.len() < short_len {
            continue;
        }
        let tail = &preceding_tokens[preceding_tokens.len() - short_len..];
        if phrase_initials(tail).as_deref() == Some(short_upper) {
            aliases.insert(tail.join(" "));
        }
    }
}

/// Case-insensitive, Unicode-aware equality of two token sequences.
fn phrase_eq_ignore_case(left: &[&str], right: &[&str]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| a.to_lowercase() == b.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parenthetical_gloss_emits_short_form() {
        let aliases =
            detect_acronym_aliases_for_label("Deploy the Alpha Suite (AS) now.", "Alpha Suite");
        assert_eq!(aliases, vec!["AS".to_string()]);
    }

    #[test]
    fn no_parenthetical_gloss_yields_nothing() {
        // The short form appears in the chunk but without a parenthetical
        // gloss — only detector A fires, so nothing is admitted.
        let aliases = detect_acronym_aliases_for_label(
            "Alpha Suite notes: restart AS after upgrade.",
            "Alpha Suite",
        );
        assert!(
            aliases.is_empty(),
            "standalone short form without parenthetical gloss must not attach: {aliases:?}"
        );
    }

    #[test]
    fn short_form_without_any_context_is_rejected() {
        let aliases =
            detect_acronym_aliases_for_label("Restart AS after the upgrade.", "Alpha Suite");
        assert!(
            aliases.is_empty(),
            "short form without its full form must not attach: {aliases:?}"
        );
    }

    #[test]
    fn lowercase_short_token_is_rejected() {
        let aliases =
            detect_acronym_aliases_for_label("Deploy the Alpha Suite (as) now.", "Alpha Suite");
        assert!(aliases.is_empty(), "lowercase short token must not be admitted: {aliases:?}");
    }

    #[test]
    fn single_word_label_yields_nothing() {
        let aliases = detect_acronym_aliases_for_label("Alpha (A) standalone.", "Alpha");
        assert!(aliases.is_empty());
    }

    #[test]
    fn non_matching_initials_are_rejected() {
        // "XY" does not spell the initials of "Alpha Suite".
        let aliases = detect_acronym_aliases_for_label("Alpha Suite (XY).", "Alpha Suite");
        assert!(aliases.is_empty());
    }

    #[test]
    fn parenthetical_requires_label_to_precede() {
        // The parenthetical short form follows an unrelated phrase, so detector
        // A does not fire.
        let aliases =
            detect_acronym_aliases_for_label("Some Beta Service (BS) here.", "Alpha Suite");
        assert!(aliases.is_empty());
    }

    #[test]
    fn multibyte_label_and_short_are_supported() {
        // Synthetic non-ASCII full form + all-uppercase short form. Exercised
        // purely through Unicode case/alphabetic predicates (no script logic).
        let label = "Ωμέγα Σύστημα"; // two words, initials Ω + Σ
        let aliases = detect_acronym_aliases_for_label("Install Ωμέγα Σύστημα (ΩΣ) here.", label);
        assert_eq!(aliases, vec!["ΩΣ".to_string()]);
    }

    #[test]
    fn polysemy_collision_keeps_senses_local() {
        // Same short form "AS" glossed for two different full forms in two
        // different chunks. Each chunk only attaches the alias to its own
        // matching label — the senses never cross.
        let alpha_chunk = "The Alpha Suite (AS) ships first.";
        let audit_chunk = "The Audit Service (AS) runs nightly.";

        assert_eq!(
            detect_acronym_aliases_for_label(alpha_chunk, "Alpha Suite"),
            vec!["AS".to_string()]
        );
        assert_eq!(
            detect_acronym_aliases_for_label(audit_chunk, "Audit Service"),
            vec!["AS".to_string()]
        );
        // Cross-attachments must be empty.
        assert!(detect_acronym_aliases_for_label(alpha_chunk, "Audit Service").is_empty());
        assert!(detect_acronym_aliases_for_label(audit_chunk, "Alpha Suite").is_empty());
    }

    #[test]
    fn detection_is_deterministic_and_deduplicated() {
        // Parenthetical gloss is emitted once even when the chunk also carries
        // standalone mentions of the short form, and re-running yields the
        // same vector.
        let chunk = "Alpha Suite (AS). Later, AS restarts; AS again.";
        let first = detect_acronym_aliases_for_label(chunk, "Alpha Suite");
        let second = detect_acronym_aliases_for_label(chunk, "Alpha Suite");
        assert_eq!(first, vec!["AS".to_string()]);
        assert_eq!(first, second);
    }

    // --- reverse direction: short label gains the full-form phrase alias ---

    #[test]
    fn reverse_parenthetical_gloss_emits_full_form() {
        let aliases =
            detect_fullform_aliases_for_short_label("Deploy the Alpha Suite (AS) now.", "AS");
        assert_eq!(aliases, vec!["Alpha Suite".to_string()]);
    }

    #[test]
    fn reverse_lowercase_short_label_is_rejected() {
        // A lowercase short label is not identifier-shaped, so the node never
        // qualifies as an acronym target.
        let aliases =
            detect_fullform_aliases_for_short_label("Deploy the Alpha Suite (AS) now.", "as");
        assert!(aliases.is_empty(), "lowercase short label must not gain a full form: {aliases:?}");
    }

    #[test]
    fn reverse_lowercase_parenthetical_token_is_rejected() {
        // The parenthesised token is lowercase, so the structural gloss is
        // rejected even though the short label itself is identifier-shaped.
        let aliases =
            detect_fullform_aliases_for_short_label("Deploy the Alpha Suite (as) now.", "AS");
        assert!(aliases.is_empty(), "lowercase parenthetical token must not attach: {aliases:?}");
    }

    #[test]
    fn reverse_without_parenthetical_gloss_yields_nothing() {
        // Same-chunk evidence requirement modelled at the detector: a chunk that
        // does not physically carry the gloss attaches nothing. A short node
        // whose own evidence chunks lack the gloss therefore gains no full form.
        let aliases =
            detect_fullform_aliases_for_short_label("Notes: restart AS after the upgrade.", "AS");
        assert!(
            aliases.is_empty(),
            "standalone short form must not attach a full form: {aliases:?}"
        );
    }

    #[test]
    fn reverse_non_matching_initials_are_rejected() {
        // The parenthesised token equals the short label, but the preceding
        // phrase's initials ("BS") do not spell it, so nothing is admitted.
        let aliases = detect_fullform_aliases_for_short_label("Some Beta Service (AS).", "AS");
        assert!(aliases.is_empty(), "phrase whose initials differ must not attach: {aliases:?}");
    }

    #[test]
    fn reverse_single_char_short_label_yields_nothing() {
        let aliases = detect_fullform_aliases_for_short_label("Alpha (A) standalone.", "A");
        assert!(aliases.is_empty());
    }

    #[test]
    fn reverse_multibyte_short_and_full_form_are_supported() {
        // Synthetic non-ASCII full form + all-uppercase short form, exercised
        // purely through Unicode case/alphabetic predicates (no script logic).
        let aliases =
            detect_fullform_aliases_for_short_label("Install Ωμέγα Σύστημα (ΩΣ) here.", "ΩΣ");
        assert_eq!(aliases, vec!["Ωμέγα Σύστημα".to_string()]);
    }

    #[test]
    fn reverse_polysemy_collision_keeps_senses_local() {
        // The same short form "AS" glossed for two different full forms in two
        // different chunks. Each chunk only yields its own full form — the caller
        // passes each short node only its own evidence chunks, so the senses
        // never cross.
        let alpha_chunk = "The Alpha Suite (AS) ships first.";
        let audit_chunk = "The Audit Service (AS) runs nightly.";
        assert_eq!(
            detect_fullform_aliases_for_short_label(alpha_chunk, "AS"),
            vec!["Alpha Suite".to_string()]
        );
        assert_eq!(
            detect_fullform_aliases_for_short_label(audit_chunk, "AS"),
            vec!["Audit Service".to_string()]
        );
    }

    #[test]
    fn reverse_detection_is_deterministic_and_deduplicated() {
        let chunk = "Alpha Suite (AS). Later, AS restarts; AS again.";
        let first = detect_fullform_aliases_for_short_label(chunk, "AS");
        let second = detect_fullform_aliases_for_short_label(chunk, "AS");
        assert_eq!(first, vec!["Alpha Suite".to_string()]);
        assert_eq!(first, second);
    }
}
