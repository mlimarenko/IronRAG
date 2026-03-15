# RustRAG Operator Console Roadmap

RustRAG frontend is already a routed operator surface. The near-term product trajectory is to make that surface coherent before widening the scope.

## Product Direction

- keep the product spine focused on `dashboard -> setup -> ingest -> ask`
- treat `/graph` and `/api` as supporting surfaces, not the main story
- standardize shared shell, status, and state patterns before adding more top-level destinations
- keep ingest messaging text-first and honest

## Required Route Qualities

Every primary route must show:

- active workspace/project context
- loading, empty, success, warning, and blocker states
- one obvious primary action
- one honest next-step hint

## Expansion Order

1. Stabilize text-first ingestion and grounded query UX across `/setup`, `/ingest`, and `/ask`.
2. Tighten shell consistency: shared page headers, status badges, panels, and state cards everywhere.
3. Add richer file extraction support for PDF and images only after the backend adapters and provenance story are real.
4. Revisit container sources such as archives or folders only after file-level extraction and operator visibility are solved.
