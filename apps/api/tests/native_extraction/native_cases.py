"""Run real model-free child regressions against freshly authored fixtures.

No provider, asset manifest, model directory or language argument is accepted.
The caller owns building the binary and generating the corpus.
"""
import argparse
import base64
import hashlib
import json
from pathlib import Path
import zipfile

from supervision import run_owned

MIMES = {
    ".pdf": "application/pdf", ".docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ".pptx": "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ".png": "image/png", ".jpg": "image/jpeg", ".bmp": "image/bmp", ".webp": "image/webp",
    ".tiff": "image/tiff", ".xml": "application/xml",
}


class NativeCases:
    def __init__(self, args):
        self.args = args
        self.results = []
        args.output.mkdir(parents=True, exist_ok=False)

    def invoke(self, name, filename, *, operation="extract", start=None, end=None,
               changes=None, expected_error=False):
        path = (self.args.corpus / filename).resolve()
        request = {
            "path": str(path), "operation": operation, "mime": MIMES[path.suffix],
            "source_sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
            "start": start, "end": end,
            "limits": {"input_bytes": 50 * 1024**2, "output_bytes": 64 * 1024**2,
                       "text_bytes": 32 * 1024**2, "units": 10000},
        }
        if changes:
            request.update(changes)
        directory = self.args.output / name
        directory.mkdir()
        receipt = run_owned([str(self.args.binary)], json.dumps(request).encode(), directory)
        (directory / "receipt.json").write_text(json.dumps({**receipt, "request": request}, indent=2) + "\n")
        assert not receipt["timeout"] and receipt["scratchRemoved"] and receipt["processGroupGone"], receipt
        payload = json.loads((directory / "stdout.json").read_text())
        assert payload["contract"] == "ironrag-extraction/3", payload
        assert payload["profile"] == "native-formats/vision-inputs/1", payload
        assert payload["engine"] == "xberg/1.3.11+git.b27b2d848048df778bf32be96fa2cfcccbf3d6ce", payload
        if expected_error:
            assert receipt["exit"] != 0 and payload["status"] == "error" and payload.get("result") is None, payload
            return payload
        assert receipt["exit"] == 0 and payload["status"] == "success", payload
        result = payload["result"]
        assert result["sourceSha256"] == request["source_sha256"]
        assert result["textByteLimit"] == request["limits"]["text_bytes"]
        assert not {"ocrIdentity", "callbackOutcomes", "originalRaster", "pictures"} & result.keys()
        assert result["selectedStart"] == (start or 1)
        assert result["selectedEnd"] == (end or result["sourceUnitCount"])
        if operation == "count":
            assert not result["units"] and not result["visualInputs"] and not result["content"]
            return result
        assert [unit["ordinal"] for unit in result["units"]] == list(range(result["selectedStart"], result["selectedEnd"] + 1))
        assert result["content"] == "\n\n".join(unit["content"] for unit in result["units"])
        previous = None
        seen = set()
        pages, pictures = set(), set()
        for ordinal, visual in enumerate(result["visualInputs"]):
            assert visual["ordinal"] == ordinal
            assert result["selectedStart"] <= visual["sourceUnit"] <= result["selectedEnd"]
            identity = (visual["sourceUnit"], visual["nativeIndex"])
            assert identity not in seen and (previous is None or previous <= identity)
            seen.add(identity)
            previous = identity
            data = base64.b64decode(visual["contentBase64"], validate=True)
            assert hashlib.sha256(data).hexdigest() == visual["payloadSha256"]
            assert visual["width"] > 0 and visual["height"] > 0
            assert visual["width"] * visual["height"] <= 25000000
            assert not {"ocrText", "ocrElements", "model", "transcript"} & visual.keys()
            (pages if visual["kind"] == "pdf_page" else pictures).add(visual["sourceUnit"])
        assert pages.isdisjoint(pictures)
        return result

    def check(self, name, action):
        try:
            action()
            self.results.append({"name": name, "status": "PASS"})
        except (AssertionError, OSError, ValueError, KeyError, RuntimeError) as error:
            self.results.append({"name": name, "status": "FAIL", "error": str(error)[:4000]})
            raise
        finally:
            (self.args.output / "checks.json").write_text(json.dumps(self.results, indent=2) + "\n")

    def pdf_inventory(self):
        digital = self.invoke("digital", "native.pdf")
        assert len(digital["units"]) == 3
        for marker in ["PAGE_ONE_COPPER", "PAGE_TWO_SILVER", "PAGE_THREE_ZINC"]:
            assert marker in digital["content"]
        assert digital["visualInputs"] and all(v["kind"] == "embedded_picture" for v in digital["visualInputs"])
        assert all(unit["tables"] for unit in digital["units"])
        authored = [["Material", "Stock", "Bin"], ["Copper", "7", "Amber"], ["Silver", "19", "Violet"], ["Zinc", "31", "Indigo"]]
        for unit in digital["units"]:
            cells = [row for table in unit["tables"] for row in table["cells"]]
            assert all(row in cells for row in authored), cells
        scanned = self.invoke("scanned", "scanned.pdf")
        assert [v["kind"] for v in scanned["visualInputs"]] == ["pdf_page", "pdf_page"]
        assert all(not u["content"].strip() for u in scanned["units"])
        mixed = self.invoke("mixed", "mixed.pdf")
        assert [(v["sourceUnit"], v["kind"]) for v in mixed["visualInputs"]] == [(1, "embedded_picture"), (2, "pdf_page")]
        selected = self.invoke("selected", "mixed.pdf", start=2, end=2)
        assert selected["sourceUnitCount"] == 3 and selected["units"][0]["ordinal"] == 2
        assert selected["visualInputs"][0]["sourceUnit"] == 2
        assert selected["visualInputs"][0]["payloadSha256"] == mixed["visualInputs"][1]["payloadSha256"]

    def office_inventory(self):
        baseline_pptx = None
        for filename in ["pictures.docx", "pictures.pptx", "image-only.docx", "image-only.pptx"]:
            result = self.invoke(filename.replace(".", "-"), filename)
            if filename.startswith("image-only."):
                assert not result["content"].strip(), "generated image/slide markers are not native source text"
                assert all(not unit["content"].strip() for unit in result["units"])
            assert len(result["visualInputs"]) == 2
            assert all(v["kind"] == "embedded_picture" for v in result["visualInputs"])
            assert [v["sourceUnit"] for v in result["visualInputs"]] == ([1, 1] if filename.endswith("docx") else [1, 2])
            if filename == "pictures.docx":
                cells = [row for table in result["units"][0]["tables"] for row in table["cells"]]
                assert ["Copper", "7", "Amber"] in cells, cells
            with zipfile.ZipFile(self.args.corpus / filename) as archive:
                for visual in result["visualInputs"]:
                    data = base64.b64decode(visual["contentBase64"], validate=True)
                    path = visual["sourcePath"]
                    if filename.endswith("docx"):
                        # DOCX exposes a verified archive-root member, not a relationship target.
                        assert isinstance(path, str) and path and not path.startswith("/")
                        assert "\\" not in path and all(part not in {"", ".", ".."} for part in path.split("/"))
                        assert archive.read(path) == data
                    else:
                        # Pinned PPTX has no member path. Prove exact bytes exist without
                        # inventing provenance or requiring the matching member to be unique.
                        assert path is None
                        assert any(archive.read(member) == data for member in archive.infolist()
                                   if not member.is_dir() and member.file_size == len(data))
            if filename == "pictures.pptx":
                baseline_pptx = result
        assert baseline_pptx is not None
        # A valid package may contain multiple distinct members with identical payloads.
        # Add an unreferenced duplicate using ZIP APIs, not a second presentation parser.
        duplicate = (self.args.output / "identical-members.pptx").resolve()
        with zipfile.ZipFile(self.args.corpus / "pictures.pptx") as source:
            with zipfile.ZipFile(duplicate, "w") as target:
                for member in source.infolist():
                    target.writestr(member, source.read(member))
                target.writestr("ppt/media/identical-payload-regression.png",
                                base64.b64decode(baseline_pptx["visualInputs"][0]["contentBase64"], validate=True))
        repeated = self.invoke("pptx-identical-members", str(duplicate))
        assert len(repeated["visualInputs"]) == len(baseline_pptx["visualInputs"])
        def identities(result):
            return [(v["sourceUnit"], v["nativeIndex"], v["payloadSha256"], v["contentBase64"], v["sourcePath"])
                    for v in result["visualInputs"]]
        assert identities(repeated) == identities(baseline_pptx)

        for filename in ["table-only.docx", "table-only.pptx"]:
            result = self.invoke(filename.replace(".", "-"), filename)
            assert not result["visualInputs"]
            assert all(marker in result["content"] for marker in ["Material", "Copper", "7", "Amber"])
        literal = self.invoke("literal-picture-label", "literal-picture-label.docx")
        assert "Picture 1" in literal["content"] and literal["content"].strip()
        assert len(literal["visualInputs"]) == 1

    def raster_inventory(self):
        for filename in ["english.png", "english.jpg", "english.bmp", "english.webp"]:
            result = self.invoke(filename.replace(".", "-"), filename)
            assert not result["content"] and result["visualInputs"][0]["kind"] == "raster_frame"
            assert base64.b64decode(result["visualInputs"][0]["contentBase64"]) == (self.args.corpus / filename).read_bytes()
        result = self.invoke("tiff", "frames.tiff")
        assert result["sourceUnitCount"] == 2
        assert [(v["sourceUnit"], v["kind"]) for v in result["visualInputs"]] == [(1, "raster_frame"), (2, "raster_frame")]
        selected = self.invoke("tiff-selected", "frames.tiff", start=2, end=2)
        assert selected["visualInputs"][0]["payloadSha256"] == result["visualInputs"][1]["payloadSha256"]

    def bounds_and_count(self):
        for filename in ["native.pdf", "scanned.pdf", "oversized.pdf", "image-only.docx", "image-only.pptx", "frames.tiff"]:
            self.invoke("count-" + filename.replace(".", "-"), filename, operation="count")
        self.invoke("oversized-render", "oversized.pdf", expected_error=True)
        self.invoke("digest-refusal", "native.pdf", changes={"source_sha256": "0" * 64}, expected_error=True)
        self.invoke("range-refusal", "native.pdf", start=4, end=4, expected_error=True)
        self.invoke("unknown-ocr", "native.pdf", changes={"ocr": True}, expected_error=True)
        self.invoke("malformed", "truncated.pdf", expected_error=True)
        self.invoke("encrypted", "encrypted.pdf", expected_error=True)
        literal = self.invoke("literal", "literal.xml")
        assert literal["content"] == (self.args.corpus / "literal.xml").read_text()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--corpus", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    groups = ["pdf_inventory", "office_inventory", "raster_inventory", "bounds_and_count"]
    parser.add_argument("--groups", nargs="+", choices=groups, default=groups)
    args = parser.parse_args()
    args.binary = args.binary.resolve(strict=True)
    args.corpus = args.corpus.resolve(strict=True)
    cases = NativeCases(args)
    for name in args.groups:
        cases.check(name, getattr(cases, name))


if __name__ == "__main__":
    main()
