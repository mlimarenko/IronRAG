use super::*;

pub(super) const PROVIDER_FREE_FALLBACK_LITERAL_LIMIT: usize = 8;
pub(super) const PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS: usize = 80;
pub(super) const PROVIDER_FREE_FALLBACK_QUOTED_LITERAL_MAX_CHARS: usize = 240;
pub(super) const PROVIDER_FREE_FALLBACK_CONFIDENCE: f32 = 0.25;

/// Build a conservative IR without calling a model after the configured query
/// compiler fails.
///
/// Semantic classification belongs exclusively to the operator-configured
/// compiler model. This fallback therefore preserves the full retrieval query
/// and extracts only language-neutral formal evidence (quoted spans and tokens
/// shaped like paths, URLs, versions, or identifiers). It never infers an act,
/// language, entity, ontology target, or ordered source slice from raw words.
///
/// No lexical classifier — word lists, title-case or surface-shape heuristics —
/// may enter this production path: such heuristics are the removed fallback
/// semantics and must stay gone even when the model is unavailable.
#[must_use]
pub(crate) fn provider_free_fallback_query_ir(question: &str) -> QueryIR {
    let retrieval_query = question.trim();

    QueryIR {
        act: QueryAct::Describe,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: Vec::new(),
        target_entities: Vec::new(),
        literal_constraints: provider_free_fallback_literal_constraints(question),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: (!retrieval_query.is_empty()).then(|| retrieval_query.to_string()),
        confidence: PROVIDER_FREE_FALLBACK_CONFIDENCE,
    }
}

pub(super) fn provider_free_fallback_literal_constraints(question: &str) -> Vec<LiteralSpan> {
    let mut literals = provider_free_quoted_literals(question)
        .into_iter()
        .map(|text| LiteralSpan { kind: LiteralKind::infer(&text), text })
        .collect::<Vec<_>>();
    literals.truncate(PROVIDER_FREE_FALLBACK_LITERAL_LIMIT);
    for token in provider_free_structural_question_tokens(question) {
        if literals.len() >= PROVIDER_FREE_FALLBACK_LITERAL_LIMIT {
            break;
        }
        let kind = LiteralKind::infer(&token);
        if provider_free_literal_kind_is_structural(kind, &token) {
            push_provider_free_fallback_literal(&mut literals, LiteralSpan { text: token, kind });
        }
    }
    literals
}

pub(super) fn push_provider_free_fallback_literal(
    literals: &mut Vec<LiteralSpan>,
    literal: LiteralSpan,
) {
    if literals.len() < PROVIDER_FREE_FALLBACK_LITERAL_LIMIT
        && !literals.iter().any(|existing| existing.text.eq_ignore_ascii_case(&literal.text))
    {
        literals.push(literal);
    }
}

pub(super) fn provider_free_literal_kind_is_structural(kind: LiteralKind, text: &str) -> bool {
    matches!(kind, LiteralKind::Url | LiteralKind::Path | LiteralKind::Version)
        || (matches!(kind, LiteralKind::Identifier)
            && text.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':') || ch.is_numeric()))
}

pub(super) fn provider_free_quoted_literals(question: &str) -> Vec<String> {
    let mut literals = Vec::new();
    let mut open: Option<(char, usize)> = None;
    for (index, ch) in question.char_indices() {
        if let Some((closing, start)) = open {
            if ch == closing {
                let literal = question[start..index].trim();
                if !literal.is_empty() {
                    literals.push(
                        literal
                            .chars()
                            .take(PROVIDER_FREE_FALLBACK_QUOTED_LITERAL_MAX_CHARS)
                            .collect(),
                    );
                }
                open = None;
            }
        } else if let Some(closing) = provider_free_quote_closer(ch) {
            open = Some((closing, index + ch.len_utf8()));
        }
    }
    literals
}

pub(super) fn provider_free_quote_closer(opening: char) -> Option<char> {
    match opening {
        '`' | '"' => Some(opening),
        '«' => Some('»'),
        '“' => Some('”'),
        '„' => Some('“'),
        _ => None,
    }
}

pub(super) fn provider_free_structural_question_tokens(question: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in question.chars() {
        if ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':') {
            current.push(ch);
        } else if !current.is_empty() {
            tokens.push(provider_free_take_token(&current));
            current.clear();
        }
    }
    if !current.is_empty() {
        tokens.push(provider_free_take_token(&current));
    }
    tokens
}

pub(super) fn provider_free_take_token(value: &str) -> String {
    value.chars().take(PROVIDER_FREE_FALLBACK_TOKEN_MAX_CHARS).collect()
}

pub(super) const QUERY_COMPILER_SYSTEM_PROMPT: &str = "You are the IronRAG query compiler. Your only job is to \
read the user's natural-language question and, where present, a short window of prior conversation \
turns, and return a typed QueryIR JSON object. The JSON schema is supplied through the runtime \
structured-output contract; when the runtime cannot carry the full schema, the same schema is \
included in the system instructions. You MUST follow it exactly and MUST NOT add prose, commentary, \
code fences, or extra fields.\n\
\n\
Guiding principles:\n\
1. `act` captures what the user is fundamentally asking: `retrieve_value` (exact value), \
`describe`, `configure_how` (procedure), `compare`, `enumerate`, `meta` (about the library itself), \
or `follow_up` (refers to prior turn). When the current question is a short disambiguating \
selection for a prior substantive request, keep the prior request's act and use the current \
selection as the target; do not downgrade the act to `follow_up` just because the selection needs \
prior context. Requests for an artifact-shaped output such as a complete example, template, \
snippet, or file block for configuring a subject are still `configure_how`; do not downgrade them \
to `describe` merely because the user also forbids invented values or asks to use only retrieved \
fragments. For configuration-shaped artifacts, include `configuration_file` and `config_key` in \
`target_types` when the wording asks for files, sections, settings, parameters, or literal \
configuration values.\n\
2. `scope` is `multi_document` ONLY when the user explicitly names or clearly implies two or more \
documents / modules / subjects; `library_meta` when the question is about the library itself. \
Default is `single_document`.\n\
`document_focus` is a separate, narrower signal: set it (and keep `scope` = `single_document`) \
ONLY when the user EXPLICITLY references one specific document — a document title, a file name, a \
path-shaped identifier, a heading, or a phrasing that points inside one named document or page. A \
question that merely NAMES a product / module / subsystem / feature is NOT a document reference: \
emit that name as a `target_entities` mention (role `subject`) and leave `document_focus` null so \
retrieval can draw from every relevant document. Decide structurally, not by topic: a bare \
component name (token `S` alone) is a subject entity; a filename / path / heading / explicit \
in-document pointer (`S.conf`, `/v2/pay`) is a `document_focus`. A \
general how-to / describe / configure / enumerate question about a named component therefore stays \
library-scoped (`single_document` scope, no `document_focus`) unless the user also pins a concrete \
document identifier.\n\
3. `literal_constraints` captures verbatim strings the user quoted — URLs, file paths, parameter \
names, code identifiers, version numbers. If the user did not quote anything verbatim, the array \
is empty.\n\
4. `temporal_constraints` captures date/time or date-range references when present. Preserve the \
surface span exactly as visible to the user. Populate `start` and `end` with ISO-8601 UTC bounds \
whenever the surface contains a self-contained absolute date or date-range (year, year+month, \
year+month+day, quarter, year+week, ISO timestamp, decade). Treat `start` as inclusive and `end` as \
exclusive. Use null bounds ONLY when the reference is genuinely under-determined and has no runtime \
anchor or explicit absolute period. Absolute calendar references must resolve regardless of the \
writing system used in the original surface.\n\
\n\
Worked examples use numeric calendar forms so the rule stays script-agnostic:\n\
\n\
- surface: \"2026-03\" -> start: \"2026-03-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"2026-03-27\" -> start: \"2026-03-27T00:00:00Z\", end: \"2026-03-28T00:00:00Z\"\n\
- surface: \"2026-Q1\" -> start: \"2026-01-01T00:00:00Z\", end: \"2026-04-01T00:00:00Z\"\n\
- surface: \"2026-W13\" -> start: \"2026-03-23T00:00:00Z\", end: \"2026-03-30T00:00:00Z\"\n\
- surface: \"2026-03-27T14:30:00Z\" -> start: \"2026-03-27T14:30:00Z\", end: \"2026-03-27T14:30:01Z\"\n\
5. `conversation_refs` lists unresolved anaphora / deixis / ellipsis that point to prior \
user-assistant turns, not to positions, ranges, neighboring units, or anchors inside the source \
documents being searched. `act = follow_up` is typical when the question cannot stand on its own.\n\
6. `target_types` are finite semantic enum values. Select only values advertised by the supplied \
JSON schema. When no enum value fits, leave `target_types` empty and represent the subject in \
`target_entities`; never invent, translate, or alias a target type.\n\
7. `source_slice` is null for ordinary summaries, comparisons, procedures, and needle lookups. \
Set it only when the user asks for a positional slice of a sequential source: earliest units \
(`head`), latest units (`tail`), or a bounded representation of the whole sequence (`all`). \
Populate `count` only when the user asks for a concrete number of units. Set \
`source_slice.filter` to `release_marker` only when the slice asks for latest version/release \
records; otherwise set it to `none`.\n\
For ordered change-summary requests over version or release records, include the `release` or \
`version` target type and use a `tail` source slice when the user asks for the latest records.\n\
8. Every source-bearing text field — `target_entities[*].label`, `literal_constraints[*].text`, \
`temporal_constraints[*].surface`, `document_focus.hint`, `conversation_refs[*].surface`, and each \
non-null `comparison` operand — must be a non-empty, surrounding-whitespace-free verbatim substring \
of the current question or prior turns. Preserve the exact writing system, spelling, case, \
punctuation, and internal whitespace visible to the user; do not translate, transliterate, \
normalize look-alike glyphs, repair spelling, or \
substitute visually similar characters. Omit a source-bearing field when no exact substring supports \
it. When no prior conversation block is supplied, do not emit a history-dependent `follow_up` act, \
conversation reference, or `anaphora_unresolved` clarification.\n\
9. `confidence` ∈ [0.0, 1.0]. Use < 0.6 only when you genuinely cannot pin the question.\n\
10. `language` must use one of the schema enum values; prefer `auto` when the signal is mixed or \
unclear.\n\
11. `retrieval_query` is a self-contained search string a fresh retriever could run with no access \
to the prior turns. It is a SEARCH SURFACE, not a primary label: keep every target entity, \
document hint, and literal exactly as specified above. You may append only short generic \
action/facet words that are already represented by the typed IR fields or visible in the user \
turns. Do not translate, transliterate, normalize the writing system, infer domain vocabulary, or \
invent a new subject, product, provider, version, endpoint, path, or answer value. When the \
question depends on prior turns — a bare selection, a \
pronoun, an omitted subject — fold the subject and scope recovered FROM THE PRIOR TURNS into one \
standalone phrasing while preserving the exact spelling of every carried-over subject or literal. \
Mechanically: if the prior turns established a subject token S and the current turn supplies only a \
refinement token R, `retrieval_query` is a standalone phrasing containing S and R verbatim, plus \
optional source-preserving action/facet terms; if the current turn already carries its own subject \
token, `retrieval_query` starts from the current question and may add only source-preserving \
action/facet terms.\n\
12. `needs_clarification` is a last-resort typed signal. Set it only when a missing user choice makes \
a grounded answer impossible. Do not use it merely because several evidence-backed variants may \
exist. A broad `configure_how`, `describe`, or `enumerate` request remains answerable: retrieve the \
available evidence and explain the evidence-derived variants in the answer. Request clarification \
only when the user must choose one mutually exclusive interpretation before any useful grounded \
answer can be produced.\n\
\n\
Output nothing but the JSON object described by the schema.";

/// Build the user-side prompt: prior turns (if any) plus the current question.
pub(super) fn build_compile_prompt(question: &str, history: &[CompileHistoryTurn]) -> String {
    let mut buffer = String::new();
    if !history.is_empty() {
        buffer.push_str("# Prior conversation (oldest first)\n");
        for turn in history {
            buffer.push_str("- ");
            buffer.push_str(&turn.role);
            buffer.push_str(": ");
            buffer.push_str(turn.content.trim());
            buffer.push('\n');
        }
        buffer.push('\n');
    }
    buffer.push_str("# Current question\n");
    buffer.push_str(question.trim());
    buffer.push('\n');
    buffer
}

pub(super) fn preview(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out = String::new();
        for (index, ch) in text.chars().enumerate() {
            if index >= max {
                break;
            }
            out.push(ch);
        }
        out.push('…');
        out
    }
}
