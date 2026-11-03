# /// script
# requires-python = ">=3.11"
# dependencies = ["pillow==13.0.0", "python-docx==1.2.0", "python-pptx==1.0.2", "reportlab==5.0.1"]
# ///
"""Write real document/scan fixtures into an explicitly supplied artifact directory."""

import argparse
import hashlib
import json
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont
from docx import Document
from docx.shared import Inches as WordInches
from pptx import Presentation
from pptx.util import Inches
from reportlab.pdfgen import canvas
from reportlab.lib.utils import ImageReader
from reportlab.platypus import Table, TableStyle


def main(directory):
    root = Path(directory).resolve()
    root.mkdir(parents=True, exist_ok=True)
    font_path = Path("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf")
    cjk_path = Path("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc")
    font = ImageFont.truetype(str(font_path), 42)
    small = ImageFont.truetype(str(font_path), 34)
    rows = [["Material", "Stock", "Bin"], ["Copper", "7", "Amber"], ["Silver", "19", "Violet"], ["Zinc", "31", "Indigo"]]
    english = Image.new("RGB", (1400, 1000), "white")
    draw = ImageDraw.Draw(english)
    draw.text((70, 60), "WAREHOUSE ENGLISH SCAN", font=font, fill="black")
    for y, row in enumerate(rows):
        for x, cell in enumerate(row):
            draw.text((80 + x * 430, 220 + y * 145), cell, font=font, fill="black")
    for x in [50, 460, 890, 1330]:
        draw.line((x, 190, x, 810), fill="black", width=3)
    for y in [190, 335, 480, 625, 810]:
        draw.line((50, y, 1330, y), fill="black", width=3)
    draw.text((70, 900), "END OF STOCK TABLE", font=small, fill="black")
    russian = Image.new("RGB", (1400, 1000), "white")
    rd = ImageDraw.Draw(russian)
    rd.text((70, 120), "Проверка русского распознавания", font=font, fill="black")
    rd.text((70, 260), "На складе семь медных деталей.", font=font, fill="black")
    rd.text((70, 400), "Номер партии 4832.", font=font, fill="black")
    for suffix in ["png", "jpg", "bmp", "webp", "tiff"]:
        english.copy().save(root / f"english.{suffix}")
    russian.save(root / "russian.png")
    english.copy().save(root / "frames.tiff", save_all=True, append_images=[russian.copy()], compression="raw")
    chinese = Image.new("RGB", (1400, 650), "white")
    cd = ImageDraw.Draw(chinese)
    cjk = ImageFont.truetype(str(cjk_path), 60)
    cd.text((70, 110), "中文识别测试", font=cjk, fill="black")
    cd.text((70, 260), "仓库有七个铜零件", font=cjk, fill="black")
    chinese.save(root / "chinese.png")
    pictures = []
    for label, number, size in [("ALPHA", "731", (900, 260)), ("BETA", "842", (1000, 300))]:
        picture = Image.new("RGB", size, "white")
        ImageDraw.Draw(picture).text((35, 80), f"PICTURE {label} {number}", font=font, fill="black")
        path = root / f"picture_{label.lower()}.png"
        picture.save(path)
        pictures.append(path)
    pdf = canvas.Canvas(str(root / "native.pdf"), pagesize=(595, 842), invariant=1)
    for index, marker in enumerate(["PAGE_ONE_COPPER", "PAGE_TWO_SILVER", "PAGE_THREE_ZINC"]):
        pdf.setFont("Helvetica", 16)
        pdf.drawString(40, 780, marker)
        table = Table(rows, colWidths=[170, 120, 160], rowHeights=34)
        table.setStyle(TableStyle([("GRID", (0, 0), (-1, -1), 1, "black"), ("FONTSIZE", (0, 0), (-1, -1), 14)]))
        table.wrapOn(pdf, 500, 500)
        table.drawOn(pdf, 50, 550)
        if index < 2:
            pdf.drawImage(ImageReader(str(pictures[index])), 50, 300, width=450, height=130)
        pdf.showPage()
    pdf.save()
    english.copy().save(root / "scanned.pdf", save_all=True, append_images=[russian.copy()], resolution=150)
    encrypted = canvas.Canvas(str(root / "encrypted.pdf"), encrypt="qualification-password")
    encrypted.drawString(50, 700, "PRIVATE ENCRYPTED FIXTURE")
    encrypted.save()
    (root / "truncated.pdf").write_bytes((root / "native.pdf").read_bytes()[:100])
    word = Document()
    word.add_heading("Warehouse document", 0)
    table = word.add_table(rows=4, cols=3)
    table.style = "Table Grid"
    for row, values in zip(table.rows, rows):
        for cell, value in zip(row.cells, values):
            cell.text = value
    for path in pictures:
        word.add_picture(str(path), width=WordInches(5))
    word.save(root / "pictures.docx")
    presentation = Presentation()
    for index, picture in enumerate(pictures):
        slide = presentation.slides.add_slide(presentation.slide_layouts[6])
        shape = slide.shapes.add_textbox(Inches(0.5), Inches(0.2), Inches(8), Inches(0.6))
        shape.text = f"WAREHOUSE SLIDE {index + 1}"
        table = slide.shapes.add_table(4, 3, Inches(0.5), Inches(1), Inches(8), Inches(2.5)).table
        for y, row in enumerate(rows):
            for x, cell in enumerate(row):
                table.cell(y, x).text = cell
        slide.shapes.add_picture(str(picture), Inches(0.5), Inches(4), width=Inches(7))
    presentation.save(root / "pictures.pptx")
    # Native and scanned pages coexist; a scan's full-page input must exclude
    # its embedded image inventory while the digital pages retain theirs.
    mixed = canvas.Canvas(str(root / "mixed.pdf"), pagesize=(595, 842), invariant=1)
    mixed.drawString(40, 780, "MIXED_NATIVE_ONE")
    mixed.drawImage(ImageReader(str(pictures[0])), 50, 300, width=450, height=130)
    mixed.showPage()
    mixed.drawImage(ImageReader(english), 0, 0, width=595, height=842)
    mixed.showPage()
    mixed.drawString(40, 780, "MIXED_NATIVE_THREE")
    mixed.showPage()
    mixed.save()
    oversized = canvas.Canvas(str(root / "oversized.pdf"), pagesize=(20000, 20000), invariant=1)
    oversized.showPage()
    oversized.save()
    image_word = Document()
    for path in pictures:
        image_word.add_picture(str(path), width=WordInches(5))
    image_word.save(root / "image-only.docx")
    image_slides = Presentation()
    for path in pictures:
        slide = image_slides.slides.add_slide(image_slides.slide_layouts[6])
        slide.shapes.add_picture(str(path), Inches(0.5), Inches(1), width=Inches(7))
    image_slides.save(root / "image-only.pptx")
    table_word = Document()
    native_table = table_word.add_table(rows=len(rows), cols=len(rows[0]))
    for y, row in enumerate(rows):
        for x, value in enumerate(row):
            native_table.cell(y, x).text = value
    table_word.save(root / "table-only.docx")
    table_slides = Presentation()
    slide = table_slides.slides.add_slide(table_slides.slide_layouts[6])
    native_table = slide.shapes.add_table(4, 3, Inches(0.5), Inches(1), Inches(8), Inches(2.5)).table
    for y, row in enumerate(rows):
        for x, value in enumerate(row):
            native_table.cell(y, x).text = value
    table_slides.save(root / "table-only.pptx")
    literal_word = Document()
    literal_word.add_paragraph("![Picture 1](image_0.png)")
    literal_word.add_picture(str(pictures[0]), width=WordInches(5))
    literal_word.save(root / "literal-picture-label.docx")
    (root / "literal.xml").write_text("<?xml version=\"1.0\"?>\n<!-- native -->\n<root>  literal </root>\n")
    manifest = {
        "rows": rows,
        "sources": [{"path": str(p), "bytes": p.stat().st_size, "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in sorted(root.iterdir()) if p.is_file()],
        "fonts": [{"path": str(p), "sha256": hashlib.sha256(p.read_bytes()).hexdigest()} for p in [font_path, cjk_path]],
    }
    (root / "manifest.json").write_text(json.dumps(manifest, indent=2, ensure_ascii=False) + "\n")
    print(json.dumps({"files": len(manifest["sources"]), "manifest": str(root / "manifest.json")}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("directory")
    args = parser.parse_args()
    main(args.directory)
