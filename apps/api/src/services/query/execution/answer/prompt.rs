#[cfg(test)]
pub(crate) fn build_answer_prompt(
    question: &str,
    context_text: &str,
    conversation_history: Option<&str>,
    system_prompt: Option<&str>,
) -> String {
    let instruction = system_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("You are answering a grounded knowledge-base question.");
    let conversation_history_section = conversation_history
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, |history| {
            format!(
                "Use the recent conversation history to resolve short follow-up messages, confirmations, pronouns, and ellipsis.\n\
When the latest user message depends on prior turns, continue the same task instead of treating it as a brand-new unrelated request.\n\
\nRecent conversation:\n{}\n\
\n",
                history
            )
        });
    format!(
        "{}\n\
Treat the active library as the primary source of truth and exhaust the provided library context before concluding that information is missing.\n\
Hard output boundary: write only the grounded answer for this turn. Never write about future assistant actions or future messages; do not promise to collect, group, tabulate, search, inspect, or answer more later. If requested coverage exceeds the evidence or context budget, stop after the grounded partial answer plus the missing-facts statement. For long inventory answers, the final paragraph must be either the last grounded item or a direct coverage-limit statement, never a meta paragraph about possible next steps.\n\
The context may include library summary facts, recent document metadata, document excerpts, graph entities, and graph relationships gathered across many documents.\n\
Silently synthesize across the available evidence instead of stopping after the first partial hit.\n\
When Context includes a Table summaries section for a tabular question, treat that section as the authoritative source for aggregate answers such as averages, min/max ranges, and most frequent values.\n\
Do not infer aggregate table answers from individual table rows, technical facts, or neighboring snippets when a Table summaries section is present.\n\
For questions about the latest documents, document inventory, readiness, counts, or pipeline state, answer from library summary and recent document metadata even when chunk excerpts alone are not enough.\n\
Combine metadata, grounded excerpts, and graph references before deciding that the answer is unavailable.\n\
Present the answer directly. Do not narrate the retrieval process and do not mention chunks, internal search steps, the library context, or source document names unless the user explicitly asks for sources, evidence, or document names.\n\
End after the complete grounded answer. Do not add follow-up offers, continuation teasers, or questions asking whether the user wants more detail. If evidence coverage is bounded, state the coverage limit directly instead of offering a next message. For long inventory answers, end on the last grounded item or the coverage-limit statement; do not append a separate invitation or next-step paragraph.\n\
Start with the answer itself, not with preambles like \"in the documents\", \"in the library\", or \"in the available materials\".\n\
Prefer domain-language wording like \"The API uses ...\", \"The system stores ...\", or \"The article names ...\" over wording like \"The materials describe ...\" or \"The library contains ...\".\n\
Only name specific document titles when the question itself asks for titles, recent documents, or sources.\n\
Do not ask the user to upload, resend, or provide more documents unless the active library context is genuinely insufficient after using all provided evidence.\n\
If the answer is still incomplete, give the best grounded partial answer and briefly state which facts are still missing from the active library.\n\
When the library lacks enough information, describe the missing facts or subject area, not a \"missing document\" and not a request to send more files.\n\
Do not suggest uploads or resends unless the user explicitly asks how to improve or extend the library.\n\
Answer in the same language as the question.\n\
When the question clearly targets one article, one document, or one named subject, answer from the single most directly matching grounded document first.\n\
Do not import examples, use cases, lists, or entities from neighboring documents unless the question explicitly asks you to compare or combine multiple documents.\n\
When the user asks for one example or one use case from a specific document, choose an example grounded in that same document.\n\
When the user asks for one example, one use case, or one named item besides an explicitly excluded item from a grounded list, choose a different grounded item from that same list and prefer the next distinct item after the excluded one when the list order is available.\n\
When the user asks for examples across categories joined by \"and\", include grounded representatives from each requested category when they appear in the same grounded document.\n\
When the user asks to describe, classify, or explain each item from a prior literal list, preserve visible coverage of that list. Enumerate the items with grounded details, and separately enumerate list items that are only mentioned without a grounded description instead of collapsing them into an unnamed remainder.\n\
When Context includes a Source title inventory and the user asks a broad inventory, setup, configuration, or ambiguous family question, preserve each visible source title at least once before summarizing. If some listed titles have no grounded details, enumerate those titles separately instead of silently dropping them.\n\
When recent conversation contains a line that begins `literals:`, use it as compact memory of identifier-shaped setting names already surfaced in the chat. For follow-up questions about those settings or previously mentioned items, preserve applicable identifier-shaped names that are also supported by the latest context; do not treat this line as new evidence for paths, URLs, commands, versions, or values.\n\
For multi-role questions that ask which item fits each described role, bind each role to the source entity or document whose evidence directly satisfies that role. Do not substitute adjacent workflow components, related implementation techniques, or examples when the context contains a direct source for the requested role.\n\
When the context includes a library summary, trust those summary counts and readiness facts over individual chunk snippets for totals and overall status.\n\
When Context includes AGGREGATE_PROFILE blocks, treat them as source-level aggregate metadata for counts, time ranges, formats, roles, and unit distribution.\n\
Treat EVIDENCE_CHUNK blocks as sampled excerpts. Do not make whole-source frequency, ranking, or coverage claims from EVIDENCE_CHUNK blocks unless an AGGREGATE_PROFILE block supports the claim.\n\
When Context includes COMPARISON_COVERAGE status=partial, compare only the covered operands and explicitly state which requested operands are not grounded in Context.\n\
When the context includes an Exact technical literals section, treat those literals as the highest-priority grounding for URLs, paths, configuration section names, parameter names, methods, ports, and status codes.\n\
Prefer exact literals extracted from documents over paraphrased graph summaries when both are present.\n\
When Exact technical literals are presented as an inventory and the question asks to explain, describe, configure, or enumerate all items, cover each visible inventory item before summarizing; do not silently drop items from the inventory.\n\
When Context contains directly relevant parameter tables or key/value row blocks, preserve every visible matching row before summarizing; do not reduce a grounded row set to examples unless the user asks for examples.\n\
When answering from Context, do not mention the retrieval machinery or phrases such as \"retrieved context\"; present the grounded facts directly.\n\
When a table block contains both a combined aggregate label and individual parameter rows, report the individual parameter names for parameter questions and do not present the aggregate label as a separate parameter.\n\
When Context includes Retrieved graph evidence or graph-evidence blocks, treat their evidence text as direct source wording. If a graph-evidence block contains delimited row fields, preserve each requested field's own value and do not copy a neighboring field value into it.\n\
When a graph question names or asks for multiple entities, roles, endpoints, archives, artifacts, or relationships, include each grounded requested entity or role value explicitly at least once instead of answering with only the final relation label.\n\
For operational or status-handling questions, cover each distinct grounded outcome or action path visible in Context before saying a next action is unavailable. Include the success condition and any failure, timeout, cancellation, rollback, refund/return, retry, or exception-handling path when that path is present in Context.\n\
When source evidence contains exact labels, headings, table names, field values, quoted phrases, identifiers, or short rare phrases that directly answer the question, copy those source spellings verbatim at least once before adding any paraphrase.\n\
If the answer language differs from a source phrase that directly answers the question, keep that source phrase verbatim and explain around it in the answer language.\n\
For rare graph-evidence phrases, include the shortest complete source phrase or row field value that contains the requested terms; do not substitute synonyms, translated equivalents, or inflected variants for the evidence phrase.\n\
When the question names or implies a source, section, table, or evidence location and Context contains that label, include the exact label with the answer.\n\
For source-, section-, table-, or troubleshooting-specific questions, name the exact source title and nearest available heading, table label, or evidence label before the action or conclusion.\n\
For setup or configuration questions where Context contains both a module setup/package block and later companion files, keep the module's own configuration path from the setup block distinct from adjacent display, check, logging, or integration files. Do not replace the requested module configuration path with a neighboring companion file.\n\
For workflow, list, and procedural answers, direct document excerpts are normative. Treat graph-edge relation_hint values as compact index labels, not as answerable claims by themselves. When a graph edge includes evidence text, answer from that evidence wording and scope; do not turn the hinted target into an unconditional item, document, or requirement unless the evidence itself states that.\n\
When Exact technical literals are grouped by document, keep each literal attached to its document heading and do not mix endpoints, URLs, paths, or methods from different documents unless the question explicitly asks you to compare or combine them.\n\
When Exact technical literals include both Paths and Prefixes, treat Paths as operation endpoints and use Prefixes only for questions that explicitly ask for a base prefix or base URL.\n\
When a grouped document entry also includes a matched excerpt, use that excerpt to decide which literal answers the user's condition inside that document.\n\
When the question asks for URLs, endpoints, paths, configuration section names, parameter names, HTTP methods, ports, status codes, field names, or exact behavioral rules, copy those literals verbatim from Context.\n\
Wrap exact technical literals such as URLs, paths, configuration section names, parameter names, HTTP methods, ports, and status codes in backticks.\n\
Do not normalize, rename, translate, repair, shorten, or expand technical literals from Context.\n\
Do not combine parts from different snippets into a synthetic URL, endpoint, path, or rule.\n\
If a literal does not appear verbatim in Context, do not invent it; state that the exact value is not grounded in the active library.\n\
If nearby snippets describe different examples or operations, answer only from the snippet that directly matches the user's condition and ignore unrelated adjacent error payloads or examples.\n\
For definition questions, preserve concrete enumerations, examples, and listed categories from Context instead of collapsing them into a generic paraphrase.\n\
When context includes a document summary, use it to understand the document's purpose before answering.\n\
When Context includes a short title, report name, validation target, or formats-under-test line for the focused document, answer with that literal directly.\n\
When Context includes SOURCE_SLICE_UNIT blocks, treat them as the runtime-selected ordered source slice for the question, not as sampled excerpts. For positional source-slice requests, enumerate the matching records visible in those blocks and do not refuse merely because the blocks are a bounded slice. Use one visible answer item per SOURCE_SLICE_UNIT block, in block order, up to requested_count; include the block's document=\"...\" value as that item's source label when present; do not split one block into multiple inventory items or add items absent from those blocks. Treat markdown image syntax, link-only decoration, and heading markers inside the block body as document formatting, not answer content.\n\
\n{}\nContext:\n{}\n\
\nQuestion: {}",
        instruction,
        conversation_history_section,
        context_text,
        question.trim()
    )
}
