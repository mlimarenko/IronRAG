# Changelog

## 0.0.2 - 2026-03-31

### Added
- Added a dedicated Assistant surface for per-library chat sessions with preserved history, session rollover at five sessions, file attachments, grounded context panel, and responsive desktop/tablet/mobile layouts.
- Added an Admin `MCP` section with copyable setup snippets for Codex, Cursor, Claude Code, VS Code, and generic HTTP clients.
- Added a canonical `CHANGELOG.md` for release tracking.
- Added grounded-query benchmark harnesses for live corpus validation, including a baseline grounded QA suite and a stricter graph-backed agent workflow suite.
- Added one canonical benchmark execution path through `make benchmark-grounded` and `make benchmark-grounded-all`, plus scheduled/manual GitLab benchmark job support with result artifacts.

### Changed
- Consolidated the shell and page layout contracts across `home`, `documents`, `graph`, `admin`, `assistant`, `swagger`, and `404` into a single responsive surface model.
- Reworked the Documents page into a canonical workbench with a real sortable table, sticky filters, compact header states, inspector-first destructive actions, and responsive card/table fallbacks.
- Reworked the Graph page around one canonical canvas path with restored curved edges, better node targeting, animated layout transitions, improved cluster packing, and responsive left/right panels.
- Reworked Admin into a consistent control-plane workbench for Access, Operations, AI setup, Pricing, and MCP setup.
- Reworked Assistant into a chat-first interface with stable active session routing, markdown rendering, context drawer, compact session rail, sticky composer, and cleaner evidence presentation.
- Switched Assistant to one canonical deep retrieval mode instead of exposing retrieval mode selection in product UI.
- Increased assistant retrieval depth and context budget so the runtime can synthesize across many documents instead of stopping on shallow hits.
- Aligned the recommended MCP prompt with the runtime assistant behavior so both emphasize deep iterative search, full document reads, and fact-focused answers.
- Expanded assistant lexical retrieval to fan out across multiple canonical query variants instead of stopping at a single narrow keyword path.
- Reworked grounded query execution around one canonical context-bundle path so answer generation, debug evidence, graph references, and benchmark validation all consume the same runtime truth.
- Reworked technical-answer handling for API-like documents so exact literals such as URLs, endpoints, methods, parameters, and ports are preserved and verified more strictly.
- Reworked dashboard and document readiness semantics so `processing`, `search-ready`, `graph-sparse`, and `graph-ready` states no longer contradict each other across `home`, `documents`, and `graph`.

### Fixed
- Fixed document list status modeling to use canonical document summary/readiness state instead of historical mutation/job fanout.
- Fixed multipart upload handling so PDF replacement/upload uses one canonical reader path and does not lose bytes.
- Fixed document detail responses to return canonical `fileName` directly.
- Fixed tolerant PNG decoding for edge-case images with broken CRC that should still be readable by the pipeline.
- Fixed UI upload flows for `pdf`, `docx`, `pptx`, `png`, and `jpg`, and validated them end-to-end through the live runtime.
- Fixed the graph cursor contract so empty canvas uses pan cursors and nodes use pointer cursors.
- Fixed graph node selection and dragging so dense clusters are easier to target and drag reliably.
- Fixed graph sparse/error states so placeholder surfaces do not render together with half-alive canvas controls.
- Fixed the Assistant chat shell so the composer no longer falls below the viewport on long threads.
- Fixed the Assistant new-session flow so a message no longer lands in the wrong thread during session rollover.
- Fixed Assistant answers for latest-document and library-summary questions by enriching runtime context with library summary, recent documents, and document briefs.
- Fixed Assistant behavior so it no longer defaults to telling the user to upload or resend documents before exhausting the current library context.
- Fixed assistant answer prompting so final responses no longer narrate document-reading or retrieval internals unless sources are explicitly requested.
- Fixed graph query/runtime regressions so relation traversal, provenance lookup, lexical recall, and exact-literal answer paths no longer silently degrade into weak chunk-only behavior.
- Fixed graph readiness mismatches where one surface could report documents as available while another still implied graph processing was incomplete.
- Fixed graph picking and dragging accuracy in dense clusters by correcting renderer/picking coordinate handling and stabilizing node-hit selection.

### Pipeline and Retrieval
- Rebuilt the assistant query path to merge vector and lexical document retrieval instead of using lexical search only as an empty fallback.
- Added recent-document previews and retrieved-document briefs to the runtime answer context.
- Kept query answer streaming over SSE and preserved staged/streamed assistant response rendering in the frontend.
- Added technical-layout repair for broken PDF line wraps so API identifiers like `pageNumber`, `withCards`, `number_starting`, and wrapped URLs remain retrievable.
- Added deterministic technical-answer assembly for common exact-literal question classes such as WSDL/protocol lookup, endpoint selection, pagination parameters, protocol comparison, and explicit unsupported-capability answers.
- Added graph-usage assertions to the regression benchmark so a passing suite now requires real chunk/entity/relation participation rather than answer text alone.

### UI/UX Polish
- Reduced duplicate metrics and repetitive status messaging on `home`, `documents`, `graph`, and `admin`.
- Tightened compact and sparse states on desktop, tablet, and mobile layouts across the product.
- Reduced low-signal chips, duplicate context blocks, and oversized empty states in Admin and Assistant side panels.
- Improved the Documents workbench so in-flight processing states are visually explicit, with clearer status grouping and more truthful progress/readiness messaging.
- Improved the Graph inspector so selected-node detail is larger, more readable, less repetitive, and more responsive across desktop and narrow layouts.

## 0.0.1

### Release
- Initial release.
