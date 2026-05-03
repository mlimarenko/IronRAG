#!/opt/ironrag-docling/bin/python
import json
import sys
import time
from importlib import metadata
from pathlib import Path

from docling.document_converter import DocumentConverter


def _stringify_status(status):
    if status is None:
        return None
    return getattr(status, "value", None) or getattr(status, "name", None) or str(status)


def _stringify_format(value):
    if value is None:
        return None
    return getattr(value, "value", None) or getattr(value, "name", None) or str(value)


def _page_count(result):
    pages = getattr(result, "pages", None)
    if pages is None:
        return None
    try:
        return len(pages)
    except TypeError:
        return None


def _input_format(result):
    source_input = getattr(result, "input", None)
    return _stringify_format(getattr(source_input, "format", None))


def _warnings(result):
    errors = getattr(result, "errors", None) or []
    return [str(error) for error in errors if str(error).strip()]


def main():
    if len(sys.argv) != 2:
        print("usage: ironrag-docling-extract <input-file>", file=sys.stderr)
        return 64

    source = Path(sys.argv[1])
    if not source.is_file():
        print(f"input file not found: {source}", file=sys.stderr)
        return 66

    started_at = time.perf_counter()
    converter = DocumentConverter()
    result = converter.convert(source)
    document = result.document
    markdown = document.export_to_markdown(image_placeholder="<!-- image -->")
    text = document.export_to_text()

    payload = {
        "markdown": markdown,
        "text": text,
        "pageCount": _page_count(result),
        "status": _stringify_status(getattr(result, "status", None)),
        "inputFormat": _input_format(result),
        "doclingVersion": metadata.version("docling"),
        "warnings": _warnings(result),
        "timings": {
            "totalSeconds": round(time.perf_counter() - started_at, 6),
        },
    }
    print(json.dumps(payload, ensure_ascii=False), flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
