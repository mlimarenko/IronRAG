# RustRAG Ingest Support Matrix

Current product promise: text works. Everything else should be documented relative to that baseline.

| Input | Status | Current contract |
|------|--------|------------------|
| Plain text | supported now | `POST /v1/content/ingest-text` accepts text, creates an ingestion job, and the worker processes it asynchronously. |
| Single-file UTF-8 text-like upload | supported now | `POST /v1/uploads/ingest` accepts one file per request, decodes supported text-like content as UTF-8 text, and queues an ingestion job. |
| PDF upload | planned, blocked | The backend classifies PDF explicitly but rejects it until PDF text extraction exists. The UI should keep this visible as planned, not supported. |
| Image upload | planned, blocked | The backend classifies images explicitly but rejects them until OCR/image extraction exists. The UI should keep this visible as planned, not supported. |
| Archive upload | not supported | No unzip/unpack adapter exists. Archives fall outside the current ingest contract and should be treated as unsupported binary/container inputs. |
| Folder ingest | not supported | There is no folder picker, recursive ingest API, or file-by-file provenance contract for local directories today. |

## Scope Rules

- Single-file upload only.
- Text-like support means UTF-8 decodable content with text-like extension or MIME type.
- PDF/image are the next file-adapter tier, but they are not partial support today.
- Archive/folder ingestion is not committed for the current product slice.

## Product Trajectory

1. Keep the text-first path reliable and easy to understand.
2. Add real extraction adapters for PDF and images.
3. Consider archive/folder flows only after file extraction, job visibility, and provenance are solid.
