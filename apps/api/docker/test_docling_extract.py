from __future__ import annotations

import contextlib
import importlib.util
import io
import json
import sys
import tempfile
import types
import unittest
from pathlib import Path

SCRIPT_PATH = Path(__file__).resolve().parent / "docling-extract.py"
MODULE_NAME = "docling_extract_for_test"
DOCLING_MODULE_NAMES = (
    "docling",
    "docling.datamodel",
    "docling.datamodel.base_models",
    "docling.datamodel.pipeline_options",
    "docling.document_converter",
)


class DoclingExtractTests(unittest.TestCase):
    def setUp(self) -> None:
        self.original_modules = {name: sys.modules.get(name) for name in DOCLING_MODULE_NAMES}
        sys.modules.update(self._docling_modules())
        spec = importlib.util.spec_from_file_location(MODULE_NAME, SCRIPT_PATH)
        assert spec is not None
        assert spec.loader is not None
        self.module = importlib.util.module_from_spec(spec)
        sys.modules[MODULE_NAME] = self.module
        spec.loader.exec_module(self.module)
        self.original_argv = sys.argv[:]

    def tearDown(self) -> None:
        sys.argv = self.original_argv
        sys.modules.pop(MODULE_NAME, None)
        for name, original_module in self.original_modules.items():
            if original_module is None:
                sys.modules.pop(name, None)
            else:
                sys.modules[name] = original_module

    @staticmethod
    def _docling_modules() -> dict[str, types.ModuleType]:
        docling = types.ModuleType("docling")
        datamodel = types.ModuleType("docling.datamodel")
        base_models = types.ModuleType("docling.datamodel.base_models")
        base_models.InputFormat = types.SimpleNamespace(PDF="pdf")
        pipeline_options = types.ModuleType("docling.datamodel.pipeline_options")
        pipeline_options.PdfPipelineOptions = type("PdfPipelineOptions", (), {})
        pipeline_options.TesseractCliOcrOptions = type("TesseractCliOcrOptions", (), {})
        document_converter = types.ModuleType("docling.document_converter")
        document_converter.DocumentConverter = type("DocumentConverter", (), {})
        document_converter.PdfFormatOption = type("PdfFormatOption", (), {})
        document_converter.ImageFormatOption = type("ImageFormatOption", (), {})
        return {
            "docling": docling,
            "docling.datamodel": datamodel,
            "docling.datamodel.base_models": base_models,
            "docling.datamodel.pipeline_options": pipeline_options,
            "docling.document_converter": document_converter,
        }

    def test_page_count_mode_serializes_counter_result(self) -> None:
        with tempfile.NamedTemporaryFile() as source_file:
            counter_paths: list[Path] = []

            def fake_page_count(path: Path) -> int:
                counter_paths.append(path)
                return 3

            self.module._get_pdf_page_count = fake_page_count
            sys.argv = [str(SCRIPT_PATH), "--page-count", source_file.name]
            stdout = io.StringIO()
            with contextlib.redirect_stdout(stdout):
                exit_code = self.module.main()

        self.assertEqual(exit_code, 0)
        self.assertEqual(counter_paths, [Path(source_file.name)])
        self.assertEqual(json.loads(stdout.getvalue()), {"pageCount": 3})

    def test_page_batches_reject_invalid_batch_size_before_opening_source(self) -> None:
        sys.argv = [str(SCRIPT_PATH), "--page-batches", "0", "1-2", "missing.pdf"]
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr):
            exit_code = self.module.main()

        self.assertEqual(exit_code, 64)
        self.assertIn("invalid page batch arguments", stderr.getvalue())

    def test_ocr_text_preserves_nonempty_results_in_order(self) -> None:
        observed_images: list[bytes] = []

        def reader(image: bytes) -> types.SimpleNamespace:
            observed_images.append(image)
            return types.SimpleNamespace(txts=(" first ", "", " second "))

        self.assertEqual(self.module._ocr_image_bytes(reader, b"image"), "first second")
        self.assertEqual(observed_images, [b"image"])

    def test_no_detected_text_is_an_empty_result(self) -> None:
        def reader(image: bytes) -> types.SimpleNamespace:
            return types.SimpleNamespace(txts=None)

        self.assertEqual(self.module._ocr_image_bytes(reader, b"image"), "")

    def test_ocr_inference_error_is_not_empty_success(self) -> None:
        def reader(image: bytes) -> types.SimpleNamespace:
            raise RuntimeError("model failed")

        with self.assertRaisesRegex(RuntimeError, "model failed"):
            self.module._ocr_image_bytes(reader, b"image")

    def test_page_scripts_do_not_share_cached_readers(self) -> None:
        latin = self.module._ocr_reader("latin")
        cyrillic = self.module._ocr_reader("cyrillic")
        self.assertIsNot(latin, cyrillic)
        self.assertIs(self.module._ocr_reader("latin"), latin)
        self.assertIs(self.module._ocr_reader("cyrillic"), cyrillic)

    def test_unknown_script_fails_without_initializing_a_reader(self) -> None:
        with self.assertRaises(KeyError):
            self.module._ocr_reader("unknown-script")
        self.assertFalse(self.module._OCR_READERS)


if __name__ == "__main__":
    unittest.main()
