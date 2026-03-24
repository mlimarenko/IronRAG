#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import subprocess
import zipfile
from pathlib import Path


def write_text_fixtures(target: Path) -> None:
    (target / "release.txt").write_text(
        "Acme Corp signed a partnership with Beta Labs in 2026. "
        "CEO Alice Ivanova announced a graph roadmap in Berlin.",
        encoding="utf-8",
    )
    (target / "release.md").write_text(
        "# Release Notes\n"
        "RustRAG Cloud launched in Berlin.\n"
        "Budget 2026 approved for Acme and Beta collaboration.\n",
        encoding="utf-8",
    )
    (target / "release.csv").write_text(
        "company,city,topic\n"
        "Acme Corp,Berlin,Graph Analytics\n"
        "Beta Labs,Warsaw,Entity Extraction\n",
        encoding="utf-8",
    )
    (target / "release.json").write_text(
        json.dumps(
            {
                "project": "RustRAG",
                "initiative": "Release Validation",
                "partners": ["Acme Corp", "Beta Labs"],
                "city": "Berlin",
                "budget": "Budget 2026",
            },
            ensure_ascii=False,
        ),
        encoding="utf-8",
    )
    (target / "release.html").write_text(
        "<html><body><h1>Acme and Beta</h1>"
        "<p>RustRAG roadmap approved in Berlin with Budget 2026.</p>"
        "</body></html>",
        encoding="utf-8",
    )
    (target / "release.rtf").write_text(
        r"{\rtf1\ansi\deff0 {\fonttbl {\f0 Arial;}}\f0\fs24 "
        r"RTF briefing: Acme Corp and Beta Labs confirmed Budget 2026 in Berlin.}",
        encoding="utf-8",
    )


def write_docx(target: Path) -> None:
    content_types = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
</Types>"""
    rels = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"""
    doc_xml = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>DOCX brief: Acme Corp and Beta Labs expanded RustRAG in Berlin under Budget 2026.</w:t></w:r></w:p>
  </w:body>
</w:document>"""
    with zipfile.ZipFile(target / "release.docx", "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("[Content_Types].xml", content_types)
        archive.writestr("_rels/.rels", rels)
        archive.writestr("word/document.xml", doc_xml)


def write_png_and_pdf(target: Path) -> None:
    png_path = target / "release.png"
    pdf_path = target / "release.pdf"
    subprocess.run(
        [
            "convert",
            "-size",
            "1200x500",
            "xc:white",
            "-gravity",
            "center",
            "-pointsize",
            "42",
            "-fill",
            "black",
            "-annotate",
            "+0-45",
            "Acme + Beta",
            "-annotate",
            "+0+35",
            "RustRAG Budget 2026 Berlin",
            str(png_path),
        ],
        check=True,
    )
    subprocess.run(["convert", str(png_path), str(pdf_path)], check=True)


def write_negative_fixtures(target: Path) -> None:
    (target / "invalid-empty.bin").write_bytes(b"")
    (target / "invalid-truncated.png").write_bytes(b"\x89PNG\r\n\x1a\nBADPNG")


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate deterministic release validation fixtures")
    parser.add_argument("--output-dir", required=True, help="Output fixture directory")
    parser.add_argument("--include-negative", action="store_true", help="Emit invalid fixtures too")
    args = parser.parse_args()

    out = Path(args.output_dir).resolve()
    out.mkdir(parents=True, exist_ok=True)

    write_text_fixtures(out)
    write_docx(out)
    write_png_and_pdf(out)
    if args.include_negative:
        write_negative_fixtures(out)

    print(str(out))


if __name__ == "__main__":
    main()
