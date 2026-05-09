#!/opt/ironrag-docling/bin/python
import json
import sys
import time
from importlib import metadata
from pathlib import Path

from docling.datamodel.base_models import InputFormat
from docling.datamodel.pipeline_options import PdfPipelineOptions
from docling.document_converter import DocumentConverter, PdfFormatOption


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
    """Return 'cyrillic', 'latin', or 'cjk' based on the dominant Unicode
    block in `sample_text`. Used to pick the matching rapidocr Rec model
    so that Russian PDFs don't get fed through the Chinese-only model
    that transliterates Cyrillic into Latin look-alikes."""
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
    """Lazily initialize the OCR reader.

    Tries (in order): rapidocr (Docling 2.x default, torch backend, Rec
    model selected by detected script — Cyrillic / Latin / CJK), easyocr,
    tesseract subprocess. The first that initializes successfully wins.
    """
    global _OCR_READER
    if _OCR_READER is None:
        try:
            from rapidocr import RapidOCR  # type: ignore
            from rapidocr.utils.typings import EngineType, LangDet, LangRec  # type: ignore

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
        try:
            import easyocr  # type: ignore

            _OCR_READER = ("easyocr", easyocr.Reader(["en", "ru"], gpu=False))
            return _OCR_READER
        except Exception:
            pass
        # Tesseract subprocess fallback. The binary ships in the image
        # for Docling's existing OCR path; we just use it directly on
        # the cropped picture image bytes.
        import shutil

        if shutil.which("tesseract"):
            _OCR_READER = ("tesseract", "tesseract")
            return _OCR_READER
        _OCR_READER = ("none", None)
    return _OCR_READER


def _ocr_image_bytes(reader_tuple, image_bytes):
    kind, reader = reader_tuple
    if kind == "rapidocr":
        try:
            result = reader(image_bytes)
        except Exception:
            return ""
        # rapidocr 3.x returns RapidOCROutput with .txts attribute.
        texts = getattr(result, "txts", None)
        if texts is None and isinstance(result, tuple):
            texts = result[0] if len(result) >= 1 else None
        if not texts:
            return ""
        if hasattr(texts, "__iter__"):
            return " ".join(str(t).strip() for t in texts if str(t).strip())
        return str(texts).strip()
    if kind == "easyocr":
        try:
            ocr_lines = reader.readtext(image_bytes, detail=0, paragraph=True)
            return " ".join(line.strip() for line in ocr_lines if line.strip())
        except Exception:
            return ""
    if kind == "tesseract":
        import subprocess
        import tempfile

        try:
            with tempfile.NamedTemporaryFile(suffix=".png", delete=False) as fp:
                fp.write(image_bytes)
                tmp_path = fp.name
            proc = subprocess.run(
                ["tesseract", tmp_path, "stdout", "-l", "rus+eng", "--psm", "6"],
                capture_output=True,
                text=True,
                timeout=30,
            )
            return " ".join(
                line.strip() for line in proc.stdout.splitlines() if line.strip()
            )
        except Exception:
            return ""
        finally:
            try:
                import os

                os.unlink(tmp_path)
            except Exception:
                pass
    return ""


def _ocr_picture_items(document, script_hint="latin"):
    """Run OCR on every PictureItem in the document and return a list
    of cleaned, non-empty text snippets in document order. `script_hint`
    selects the rapidocr Rec model so Cyrillic PDFs stay in Cyrillic."""
    reader_tuple = _ocr_reader(script_hint)
    if reader_tuple[1] is None:
        return []
    try:
        from docling_core.types.doc.document import PictureItem  # type: ignore
    except ImportError:
        return []
    import io

    try:
        from PIL import Image  # type: ignore  # noqa: F401
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


def _splice_picture_ocr(markdown, snippets):
    """Replace each `<!-- image -->` placeholder with the placeholder
    plus a fenced block carrying the OCR'd text from that picture, in
    document order. Empty snippets keep the placeholder untouched."""
    placeholder = "<!-- image -->"
    out_parts = []
    cursor = 0
    snippet_idx = 0
    while True:
        hit = markdown.find(placeholder, cursor)
        if hit == -1:
            out_parts.append(markdown[cursor:])
            break
        end = hit + len(placeholder)
        out_parts.append(markdown[cursor:end])
        if snippet_idx < len(snippets):
            text = snippets[snippet_idx].strip()
            if text:
                out_parts.append(f"\n\n> Image OCR: {text}\n")
        snippet_idx += 1
        cursor = end
    return "".join(out_parts)


def main():
    if len(sys.argv) != 2:
        print("usage: ironrag-docling-extract <input-file>", file=sys.stderr)
        return 64

    source = Path(sys.argv[1])
    if not source.is_file():
        print(f"input file not found: {source}", file=sys.stderr)
        return 66

    started_at = time.perf_counter()

    # Enable per-picture image generation so we can OCR embedded raster
    # images that the text layer doesn't cover (screenshots, diagrams,
    # tables-as-image inside PDFs). Without this, embedded images in
    # PDFs become bare `<!-- image -->` placeholders in the markdown
    # and their textual content never reaches the chunker / graph
    # extractor.
    pdf_opts = PdfPipelineOptions()
    pdf_opts.images_scale = 2.0
    pdf_opts.generate_picture_images = True
    pdf_opts.do_ocr = True

    converter = DocumentConverter(
        format_options={
            InputFormat.PDF: PdfFormatOption(pipeline_options=pdf_opts),
        }
    )
    result = converter.convert(source)
    document = result.document

    # Post-process: run the in-process OCR engine on each embedded
    # picture's cropped image and inline the extracted text after the
    # picture placeholder so chunking + graph extraction see it.
    # Detect the dominant script from the already-extracted text layer
    # so rapidocr loads the matching Rec model (Cyrillic vs CJK vs Latin)
    # — the default Chinese-only Rec transliterated Russian into Latin
    # look-alikes ("ИНВЕНТАРИЗАЦИЯ" -> "MHBeHTapN3aUNA").
    text = document.export_to_text()
    script_hint = _detect_dominant_script(text)
    picture_ocr_text = _ocr_picture_items(document, script_hint=script_hint)

    markdown = document.export_to_markdown(image_placeholder="<!-- image -->")
    if picture_ocr_text:
        markdown = _splice_picture_ocr(markdown, picture_ocr_text)
    if picture_ocr_text:
        text = text + "\n\n" + "\n\n".join(picture_ocr_text)

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
