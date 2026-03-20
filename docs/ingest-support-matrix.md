# Ingest Support Matrix

This matrix describes the current upload-admission truth for mixed-format ingestion in RustRAG.

## Accepted upload fields

- `file`
- `files`

Any other multipart field names are ignored by the document-upload routes.

## Global admission limits

- Upload size limit is controlled by `upload_max_size_mb`.
- Current default is `50 MB`.
- UI and runtime batch uploads are all-or-nothing at admission time.
  If one file in the batch is rejected, the route returns a structured error and the batch is not queued.
- Rejection payloads are file-specific and include `fileName`, `detectedFormat`, `mimeType`, `fileSizeBytes`, `uploadLimitMb`, `rejectionCause`, and `operatorAction` when the route can determine them honestly.

## Format matrix

| Format family | Admission basis | Extraction path | Current truth | Known caveats |
| --- | --- | --- | --- | --- |
| TXT / UTF-8 text-like files | Extension or MIME maps to `TextLike` | `text_like` | Accepted when the payload decodes as UTF-8 and stays under `upload_max_size_mb` | Non-UTF-8 text-like payloads are rejected with `invalid_text_encoding` |
| MD and other text-like extensions | Extension or MIME maps to `TextLike` | `text_like` | Accepted under the same rules as TXT | The file is treated as text, not format-aware markdown or code |
| PDF | `.pdf` extension or `application/pdf` MIME | `pdf_text` | Accepted when bytes form a readable PDF and stay under `upload_max_size_mb` | Extraction uses `lopdf` first and may fall back to `pdftotext`; malformed PDFs can fail with `upload_extraction_failed` |
| DOCX | `.docx` extension or Office MIME | `docx_text` | Accepted when the archive parses and stays under `upload_max_size_mb` | Malformed Office payloads fail as `upload_extraction_failed` |
| Images | Common image extensions or `image/*` MIME | `vision_image` | Accepted when the file is under the upload limit and the provider can transcribe visible text | OCR output is normalized before persistence; wrapper prompt text is removed from `content_text`, and residual OCR concerns are surfaced as warnings instead of being mixed into the preview text |
| Unsupported binary | Everything else | none | Rejected | Returns `unsupported_upload_type` |

## Stable rejection kinds

- `invalid_multipart_payload`
- `invalid_file_body`
- `upload_limit_exceeded`
- `unsupported_upload_type`
- `invalid_text_encoding`
- `upload_extraction_failed`
- `missing_upload_file`

## Stable provider graph-call classes

- `internal_request_invalid`
- `upstream_timeout`
- `upstream_rejection`
- `invalid_model_output`
- `recovered_after_retry`

## Rejection-kind intent

- `invalid_multipart_payload`: the multipart envelope itself is malformed or unreadable before a file can be isolated.
- `missing_upload_file`: the request was accepted structurally but no supported `file` or `files` part was present.
- `upload_limit_exceeded`: the route can prove the payload exceeds `upload_max_size_mb`.
- `unsupported_upload_type`: the route can identify the file but the extension or MIME family is unsupported.
- `invalid_text_encoding`: the file is text-like, but the bytes cannot be decoded as valid UTF-8.
- `upload_extraction_failed`: the file type is supported, but extraction failed after admission because the document or image bytes are unreadable for that extractor.
- `invalid_file_body`: reserved for genuinely indeterminate body-read failures where the route cannot honestly distinguish truncated multipart input from a corrupt body stream.

## Settled versus residual operator model

- `live_in_flight` means the library still has queue, active stage work, graph backlog, or visible in-flight provider-call accounting.
- `fully_settled` means queue, processing, pending graph, and residual failure counters are all zero and collection totals are considered terminal.
- `failed_with_residual_work` means the library reached a terminal failure boundary with explicit residual reasons such as `projection_contention`, `graph_persistence_integrity`, `settlement_refresh_failed`, `provider_failure`, `diagnostics_unavailable`, or `upload_limit_exceeded`.
- Provider graph-call failures now surface `requestShapeKey`, `requestSizeBytes`, `upstreamStatus`, and `retryDecision` so operators can distinguish RustRAG request invalidity from upstream timeout or rejection.

## Queue-isolation semantics

- Queue isolation is enforced on the main stack through reserved capacity, not through an auxiliary worker pool.
- Ordinary backlog cannot occupy every claim slot: at least one worker slot remains available for a newly eligible library slice.
- The guarantee is about claim-time isolation, not preemption. Long-running `extracting_graph` jobs keep their slot until the next claim cycle.
- Runtime and UI diagnostics expose `waitingReason`, isolated-capacity counts, active backlog, and final collection settlement so operators can see whether a library is waiting behind ordinary backlog or because isolated capacity is already in use.

## Operator guidance

- For large files, split the file or raise `upload_max_size_mb`.
- For mixed-format batches, inspect the returned file-level `rejectionKind`, `rejectionCause`, and `operatorAction` before retrying the whole batch.
- For PDFs, prefer standard readable PDFs over image-only or malformed files when possible.
- For image-heavy batches, inspect preview text and normalization warnings separately; warnings no longer indicate that prompt wrapper text polluted the stored content.
- For queue complaints, trust collection diagnostics first: if `waitingReason=isolated_capacity_wait`, the library is waiting on reserved capacity rather than being starved by ordinary backlog.
