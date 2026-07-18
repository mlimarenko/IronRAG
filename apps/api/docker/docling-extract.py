#!/opt/ironrag-docling/bin/python
"""IronRAG Docling extraction adapter.

Usage:
    ironrag-docling-extract <input-file>              # full document
    ironrag-docling-extract --page-count <input-file>  # return page count
    ironrag-docling-extract --page N <input-file>     # extract single page (1-based)
    ironrag-docling-extract --pages START-END <input-file>  # extract page range
    ironrag-docling-extract --page-batches SIZE START-END <input-file>  # stream page ranges
"""

import json
import os
import shutil
import sys
import tempfile
import time
from importlib import metadata
from pathlib import Path

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption


EXIT_USAGE = 64
EXIT_INPUT_NOT_FOUND = 66


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


_OCR_READER = None


def _detect_dominant_script(sample_text):
    """Return a coarse Unicode-script hint for RapidOCR model selection."""
    if not sample_text:
        return "latin"
    cyr = lat = cjk = 0
    for ch in sample_text[:8000]:
        cp = ord(ch)
        if 0x0400 <= cp <= 0x04FF or 0x0500 <= cp <= 0x052F:
            cyr += 1
        elif 0x4E00 <= cp <= 0x9FFF or 0x3040 <= cp <= 0x30FF or 0xAC00 <= cp <= 0xD7AF:
            cjk += 1
        elif (0x0041 <= cp <= 0x007A) or (0x00C0 <= cp <= 0x024F):
            lat += 1
    if cyr >= max(lat, cjk):
        return "cyrillic"
    if cjk > lat:
        return "cjk"
    return "latin"


def _ocr_reader(script_hint):
    """Lazily initialize the canonical OCR reader."""
    global _OCR_READER
    if _OCR_READER is None:
        try:
            from rapidocr import RapidOCR
            from rapidocr.utils.typings import EngineType, LangDet, LangRec

            rec_lang = {
                "cyrillic": LangRec.CYRILLIC,
                "cjk": LangRec.CH,
                "latin": LangRec.EN,
            }.get(script_hint, LangRec.CH)
            try:
                _OCR_READER = (
                    "rapidocr",
                    RapidOCR(
                        params={
                            "Det.engine_type": EngineType.TORCH,
                            "Det.lang_type": LangDet.MULTI,
                            "Cls.engine_type": EngineType.TORCH,
                            "Rec.engine_type": EngineType.TORCH,
                            "Rec.lang_type": rec_lang,
                        }
                    ),
                )
                return _OCR_READER
            except Exception:
                pass
        except Exception:
            pass
        _OCR_READER = ("none", None)
    return _OCR_READER


def _ocr_image_bytes(reader_tuple, image_bytes):
    kind, reader = reader_tuple
    if kind == "rapidocr":
        try:
            result = reader(image_bytes)
        except Exception:
            return ""
        texts = getattr(result, "txts", None)
        if texts is None and isinstance(result, tuple):
            texts = result[0] if len(result) >= 1 else None
        if not texts:
            return ""
        if hasattr(texts, "__iter__"):
            return " ".join(str(t).strip() for t in texts if str(t).strip())
        return str(texts).strip()
    return ""


def _ocr_picture_items(document, script_hint="latin"):
    """Run OCR on every PictureItem in the document and return a list
    of cleaned, non-empty text snippets in document order."""
    reader_tuple = _ocr_reader(script_hint)
    if reader_tuple[1] is None:
        return []
    try:
        from docling_core.types.doc.document import PictureItem
    except ImportError:
        return []
    import io

    try:
        from PIL import Image
    except ImportError:
        return []

    snippets = []
    for item, _level in document.iterate_items():
        if not isinstance(item, PictureItem):
            continue
        try:
            pil_image = item.get_image(document)
        except Exception:
            pil_image = None
        if pil_image is None:
            snippets.append("")
            continue
        try:
            buf = io.BytesIO()
            pil_image.save(buf, format="PNG")
            text_blob = _ocr_image_bytes(reader_tuple, buf.getvalue())
        except Exception:
            text_blob = ""
        snippets.append(text_blob)
    return snippets


def _collect_picture_bytes(document):
    """Return a list of `{index, contentBase64, sizePx}` for every
    PictureItem in `document` so the Rust caller can route each
    picture through the active document-understanding binding for OCR.

    The index matches the order of `<!-- image -->` placeholders in
    the markdown (same iterate_items() order as `_ocr_picture_items`).
    Sizes below ~24x24 px are dropped — they are decorative icons,
    not screenshots, and routing them through a multimodal LLM only
    burns budget."""
    pictures = []
    try:
        from docling_core.types.doc.document import PictureItem
    except ImportError:
        return pictures

    import base64
    import io

    idx = -1
    for item, _level in document.iterate_items():
        if not isinstance(item, PictureItem):
            continue
        idx += 1
        try:
            pil_image = item.get_image(document)
        except Exception:
            pil_image = None
        if pil_image is None:
            continue
        width, height = pil_image.size
        if width < 24 or height < 24:
            continue
        try:
            buf = io.BytesIO()
            pil_image.save(buf, format="PNG")
            encoded = base64.b64encode(buf.getvalue()).decode("ascii")
        except Exception:
            continue
        pictures.append(
            {
                "index": idx,
                "mime": "image/png",
                "contentBase64": encoded,
                "sizePx": [int(width), int(height)],
            }
        )
    return pictures


def _get_pdf_page_count(source_path):
    """Return the page count of a PDF using pypdfium2 (already a docling dep)."""
    try:
        import pypdfium2 as pdfium
        pdf = pdfium.PdfDocument(source_path)
        count = len(pdf)
        pdf.close()
        return count
    except Exception:
        return None


def _extract_pdf_page(source_path, page_num, output_path):
    """Extract a single page (0-based) from a PDF and write to output_path."""
    import pypdfium2 as pdfium
    pdf = pdfium.PdfDocument(source_path)
    if page_num >= len(pdf):
        pdf.close()
        raise ValueError(f"page {page_num} out of range ({len(pdf)} pages)")
    new_pdf = pdfium.PdfDocument.new()
    new_pdf.import_pages(pdf, [page_num])
    new_pdf.save(output_path)
    new_pdf.close()
    pdf.close()


def _build_converter():
    pdf_opts = PdfPipelineOptions()
    pdf_opts.images_scale = 2.0
    pdf_opts.generate_picture_images = True
    pdf_opts.do_ocr = True

    return DocumentConverter(
        format_options={
            InputFormat.PDF: PdfFormatOption(pipeline_options=pdf_opts),
        }
    )


def _convert_document(source, started_at, converter=None):
    """Convert a document and return the JSON payload."""
    if converter is None:
        converter = _build_converter()
    result = converter.convert(source)
    document = result.document

    text = document.export_to_text()
    script_hint = _detect_dominant_script(text)
    picture_ocr_text = _ocr_picture_items(document, script_hint=script_hint)
    pictures_payload = _collect_picture_bytes(document)

    markdown = document.export_to_markdown(image_placeholder="<!-- image -->")

    return {
        "markdown": markdown,
        "text": text,
        "pictureOcrText": picture_ocr_text,
        "pageCount": _page_count(result),
        "status": _stringify_status(getattr(result, "status", None)),
        "inputFormat": _input_format(result),
        "doclingVersion": metadata.version("docling"),
        "warnings": _warnings(result),
        "pictures": pictures_payload,
        "timings": {
            "totalSeconds": round(time.perf_counter() - started_at, 6),
        },
    }


def _convert_page_range(source, start_page, end_page, tmpdir, converter):
    """Convert a 0-based inclusive PDF page range and return a batch payload."""
    import os

    started_at = time.perf_counter()
    results = []
    for page_num in range(start_page, end_page + 1):
        page_path = os.path.join(tmpdir, f"page_{page_num + 1}.pdf")
        _extract_pdf_page(source, page_num, page_path)
        try:
            payload = _convert_document(
                Path(page_path), time.perf_counter(), converter=converter
            )
            payload["extractedPage"] = page_num + 1
            results.append(payload)
        finally:
            try:
                os.unlink(page_path)
            except OSError:
                pass

    return {
        "pages": results,
        "pageRange": f"{start_page + 1}-{end_page + 1}",
        "timings": {
            "totalSeconds": round(time.perf_counter() - started_at, 6),
        },
    }


def _parse_page_range(value):
    start_value, end_value = value.split("-")
    return int(start_value) - 1, int(end_value) - 1


def _valid_page_range(start_page, end_page):
    return start_page >= 0 and end_page >= start_page


def _input_file_or_exit(value):
    source = Path(value)
    if source.is_file():
        return source
    print(f"input file not found: {source}", file=sys.stderr)
    return None


def _run_page_count(source):
    print(json.dumps({"pageCount": _get_pdf_page_count(source)}, ensure_ascii=False), flush=True)
    return 0


def _run_single_page(source, page_num):
    started_at = time.perf_counter()
    tmpdir = tempfile.mkdtemp(prefix="docling-page-")
    try:
        converter = _build_converter()
        page_path = os.path.join(tmpdir, f"page_{page_num + 1}.pdf")
        _extract_pdf_page(source, page_num, page_path)
        payload = _convert_document(Path(page_path), started_at, converter=converter)
        payload["extractedPage"] = page_num + 1
        print(json.dumps(payload, ensure_ascii=False), flush=True)
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
    return 0


def _run_page_range(source, start_page, end_page):
    started_at = time.perf_counter()
    tmpdir = tempfile.mkdtemp(prefix="docling-pages-")
    try:
        converter = _build_converter()
        payload = _convert_page_range(source, start_page, end_page, tmpdir, converter)
        payload["timings"]["totalSeconds"] = round(time.perf_counter() - started_at, 6)
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
    print(json.dumps(payload, ensure_ascii=False), flush=True)
    return 0


def _run_page_batches(source, batch_size, start_page, end_page):
    tmpdir = tempfile.mkdtemp(prefix="docling-page-batches-")
    try:
        converter = _build_converter()
        for current_page in range(start_page, end_page + 1, batch_size):
            batch_end = min(current_page + batch_size - 1, end_page)
            payload = _convert_page_range(source, current_page, batch_end, tmpdir, converter)
            print(json.dumps(payload, ensure_ascii=False), flush=True)
    finally:
        shutil.rmtree(tmpdir, ignore_errors=True)
    return 0


def _print_usage():
    print(
        "usage: ironrag-docling-extract <input-file>\n"
        "       ironrag-docling-extract --page-count <input-file>\n"
        "       ironrag-docling-extract --page N <input-file>\n"
        "       ironrag-docling-extract --pages START-END <input-file>\n"
        "       ironrag-docling-extract --page-batches SIZE START-END <input-file>",
        file=sys.stderr,
    )


def _run_page_count_command(args):
    source = _input_file_or_exit(args[1])
    return _run_page_count(source) if source else EXIT_INPUT_NOT_FOUND


def _run_single_page_command(args):
    try:
        page_num = int(args[1]) - 1
    except ValueError:
        print(f"invalid page number: {args[1]}", file=sys.stderr)
        return EXIT_USAGE
    source = _input_file_or_exit(args[2])
    return _run_single_page(source, page_num) if source else EXIT_INPUT_NOT_FOUND


def _run_page_range_command(args):
    try:
        start_page, end_page = _parse_page_range(args[1])
        if not _valid_page_range(start_page, end_page):
            raise ValueError("invalid page range")
    except ValueError:
        print(f"invalid page range: {args[1]} (expected START-END)", file=sys.stderr)
        return EXIT_USAGE
    source = _input_file_or_exit(args[2])
    return _run_page_range(source, start_page, end_page) if source else EXIT_INPUT_NOT_FOUND


def _run_page_batches_command(args):
    try:
        batch_size = int(args[1])
        if batch_size <= 0:
            raise ValueError("batch size must be positive")
        start_page, end_page = _parse_page_range(args[2])
        if not _valid_page_range(start_page, end_page):
            raise ValueError("invalid page range")
    except ValueError as error:
        print(
            f"invalid page batch arguments: {' '.join(args[1:3])} ({error})",
            file=sys.stderr,
        )
        return EXIT_USAGE
    source = _input_file_or_exit(args[3])
    return (
        _run_page_batches(source, batch_size, start_page, end_page)
        if source
        else EXIT_INPUT_NOT_FOUND
    )


def _run_full_document_command(args):
    if len(args) != 1:
        _print_usage()
        return EXIT_USAGE
    source = _input_file_or_exit(args[0])
    if source is None:
        return EXIT_INPUT_NOT_FOUND
    payload = _convert_document(source, time.perf_counter())
    print(json.dumps(payload, ensure_ascii=False), flush=True)
    return 0


def main():
    args = sys.argv[1:]
    command = args[0] if args else None
    if command == "--page-count" and len(args) >= 2:
        return _run_page_count_command(args)
    if command == "--page" and len(args) >= 3:
        return _run_single_page_command(args)
    if command == "--pages" and len(args) >= 3:
        return _run_page_range_command(args)
    if command == "--page-batches" and len(args) >= 4:
        return _run_page_batches_command(args)
    return _run_full_document_command(args)


if __name__ == "__main__":
    raise SystemExit(main())
