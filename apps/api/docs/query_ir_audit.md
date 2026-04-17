# QueryIR Audit: Hardcoded Markers → IR Fields

Reverse-engineering of ~450 hardcoded keyword markers across 15 files in the query pipeline. Each row maps a hardcoded decision site to the proposed canonical `QueryIR` field that will replace it once `QueryCompiler` (NL→IR) is live.

| File | Line | Decision Name | What It Decides | Downstream Effect | Proposed IR Field |
|------|------|---------------|-----------------|-------------------|-------------------|
| planner.rs | 10–16 | is_stop_word | Filters common English structural words during keyword extraction | Reduces noise in search, improves keyword quality | `ir.keywords` (filtered before expansion) |
| planner.rs | 20–45 | expand_with_synonyms | Maps abbreviations to canonical terms (auth→authentication, db→database, k8s→kubernetes) | Broadens search coverage, improves recall | `ir.keywords_expanded` |
| planner.rs | 142–209 | determine_query_scope | Detects document/entity/graph routing markers (document, file, pdf; relationship, connected; who is, what is) | Selects retrieval index and answer builder chain | `ir.scope: LocalEntity \| Global \| SingleDocument \| Hybrid` |
| planner.rs | 301–332 | is_exact_literal_technical | Detects API/technical literal queries (url, wsdl, endpoint, method, path, parameter, rest, soap, graphql, port, status code, prefix, http://, https://, /) | Routes to literal extraction, stricter verification, fact-bias search | `ir.intent.is_exact_literal_technical` |
| planner.rs | 340–356 | is_multi_document_technical | Detects cross-document comparison intent (compare, сравни, both, two, several, neskol'k, cross-document, separately) | Changes retrieval aggregation, document scope | `ir.scope == MultiDocument` |
| question_intent.rs | 65–161 | classify_question_intents | Bilingual keyword table mapping to intent types (Endpoint, Parameter, HttpMethod, Version, ErrorCode, EnvVar, ConfigKey, Protocol, BasePrefix, Port) | Routes to specialized answer builders (endpoint_answer, parameter_answer, port_answer) | `ir.intents: Vec<QuestionIntent>` |
| verification.rs | 143–162 | verify_graph_database_claim | Detects questions about graph databases mentioning gremlin AND sparql AND cypher AND 2019 | Adds verification warning, checks canonical target mention | `ir.target_types.contains(GraphDatabase) && ir.verification_level == Strict` |
| verification.rs | 339–351 | parse_http_literal | formal, keep | Parses "GET /path" style literals for verification corpus | `ir.literal_constraints` (parsing utility, not IR decision) |
| technical_literal_focus.rs | 9–37 | filter_literal_focus_keywords | Removes meta-keywords (если, какой, endpoint, url, path, method, нужно) for technical focus scoring | Improves focus keyword ranking, sharpens chunk selection | `ir.keywords_for_chunking` (filtered variant) |
| table_row_answer.rs | 246–260 | request_table_headers | Paired keyword→column alias mappings (должност→job title, цен→price, компани→company, телефон→phone) | Filters and formats table row output fields | `ir.table_column_requests: Vec<(Marker, Vec<Alias>)>` |
| table_row_answer.rs | 180–196 | is_value_inventory_request | Detects value listing intent (какие значения, покажи значения, what values, list values, show values) | Routes to value enumeration answer builder | `ir.table_mode == InventoryValues` |
| document_target.rs | 17 | identify_document_context_markers | Keywords appearing in document names/labels (runtime, upload, smoke, fixture, check) | Helps document targeting and scope narrowing | `ir.document_context_hints: Vec<String>` |
| document_target.rs | 18–20 | expand_acronym_labels | Common acronyms in document names (rag, llm, ocr, pdf, csv, api) | Document matching and label normalization | `ir.document_label_expansions: Vec<String>` |
| document_target.rs | 108–184 | is_multi_document_comparison | Comparison markers (compare, contrast, difference between, versus, across documents, both documents, сравни, между документ) | Changes document aggregation and scope | `ir.scope == MultiDocument` (duplicate with planner.rs) |
| canonical_target.rs | 43–72 | verify_target_presence_in_corpus | Checks if canonical targets (VectorDatabase, LargeLanguageModel, GraphDatabase, etc.) are mentioned in evidence | Validates answer claims, determines verification path | `ir.expected_targets: Vec<CanonicalTarget>` |
| table_summary_answer.rs | 363–365 | is_aggregate_average_request | Detects numeric aggregation (average, avg, средн, mean) | Routes to numeric summary aggregation builder | `ir.table_aggregation == Average` |
| table_summary_answer.rs | 368–380 | is_aggregate_frequency_request | Detects categorical aggregation (чаще всего, самый част, most frequent, most common) | Routes to categorical summary aggregation builder | `ir.table_aggregation == MostFrequent` |
| answer.rs | 212–213 | detect_response_language | Russian characters present → Russian response (Cyrillic detection) | Answer formatting language and phrasing | `ir.language: RU \| EN \| AUTO` |
| session.rs | 256–262 | filter_common_words_from_entities | Stop words for entity extraction (The, This, And, For, But, When, etc.) | Coreference resolution quality, entity deduplication | `ir.coreference_filter` (implicit in entity extraction) |
| session.rs | 363–388 | is_explicit_follow_up | Continuation markers (да, ок, continue, go on, show me, walk me through, подробнее) | Activates conversation context merging | `ir.conversation_intent == ContextDependent` |
| session.rs | 389–417 | has_contextual_reference | Pronouns/deixis (это, этот, there, this, it, them, that, same, again) | Enables coreference resolution path | `ir.has_anaphora: bool` |
| session.rs | 419–420 | filter_low_signal_tokens | Particles removing significance (а, и, ну, please, just, the, this, it) | Follow-up detection threshold calculation | `ir.signal_weight` (implicit in follow-up logic) |
| service/mod.rs | 46–69 | filter_focus_stopwords_from_segments | Stopwords for prepared segment ranking (a, an, and, how, what, is, to, in, the, как, какая, это, этот) | Source link relevance scoring, segment focus boost | `ir.segment_focus_filter` (implicit in ranking) |
| search.rs | 513–529 | is_exact_literal_technical | Strong indicators of technical queries (http://, wsdl, endpoint, method, path, port, graphql, rest, soap, /, ?) | Fact search bias multiplier, ranking adjustment | `ir.intent.is_exact_literal_technical` (duplicate with planner.rs line 301) |
| table_retrieval.rs | 13–31 | extract_requested_row_count | Parses "first N rows" pattern (первые/первых/first + строк/rows + digit) | Limits table chunk retrieval (1–32 rows) | `ir.table_row_limit: Option<usize>` |

## Summary observations

1. **Duplicate multi-document detection (3 sources)** — `planner.rs:340`, `document_target.rs:108`, `question_intent.rs` (via port). Consolidate into single `ir.scope == MultiDocument`.
2. **Duplicate exact-literal-technical (2 sources)** — `planner.rs:301`, `search.rs:513`. planner is authoritative; search becomes bias modifier only.
3. **Table mode branches (3 independent decisions)** — aggregation / inventory / row-count → one `ir.table_mode` object with optional fields.
4. **Follow-up / context (3-layer composition)** — explicit markers + anaphora + low-signal filter → one `ir.conversation_intent` object capturing all three.
5. **Keyword processing chain (3 stages)** — stop-filter → synonym-expand → tech-focus. Track in `ir.keywords: { original, filtered, expanded, tech_focused }` for debugging.
6. **Document targeting (2 layers)** — acronym expansion + context markers → `ir.document_targeting_hints`.
7. **Verification strictness (implicit tree)** — make explicit: `ir.verification_level: Strict | Moderate | Lenient` derived from IR, not rehomed literal checks.
8. **Intent classification** — `INTENT_TABLE` is already canonical; preserve as `ir.intents: Vec<QuestionIntent>`.
9. **Language preference** — single source (`answer.rs:212`), maps to `ir.language`.
10. **Hidden dependencies** — `is_multi_document_comparison` blocks some endpoint lookups; `is_context_dependent_follow_up` composes 3 marker lists; `exact_literal_technical` affects both retrieval bias and verification strictness. Document these explicitly in IR schema comments.
