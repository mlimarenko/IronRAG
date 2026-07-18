#!/usr/bin/env python3
from __future__ import annotations

import base64
import importlib.util
import json
import os
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("all-providers-seed-gen.py")
SPEC = importlib.util.spec_from_file_location("all_providers_seed_gen", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class TypedModelSignatureTests(unittest.TestCase):
    def test_model_name_never_implies_capability(self) -> None:
        for model_id in (
            "looks-like-an-embedding-model",
            "looks-like-a-vision-model",
            "looks-like-a-chat-model",
        ):
            self.assertIsNone(MODULE.typed_model_signature({"id": model_id}))

    def test_provider_typed_metadata_selects_canonical_profile(self) -> None:
        signature = MODULE.typed_model_signature(
            {
                "id": "opaque-model-id",
                "metadata": {
                    "capabilityKind": "chat",
                    "modalityKind": "multimodal",
                },
            }
        )

        self.assertIsNotNone(signature)
        capability, modality, roles = signature
        self.assertEqual((capability, modality), ("chat", "multimodal"))
        self.assertIn("query_compile", roles)
        self.assertIn("extract_text", roles)
        self.assertNotIn("rerank", roles)
        self.assertNotIn("vision", roles)

    def test_operator_manifest_is_typed_and_takes_precedence(self) -> None:
        signature = MODULE.typed_model_signature(
            {
                "id": "opaque-model-id",
                "capabilityKind": "chat",
                "modalityKind": "text",
            },
            {"capabilityKind": "embedding", "modalityKind": "text"},
        )

        self.assertEqual(signature, ("embedding", "text", ["embed_chunk"]))

    def test_invalid_typed_metadata_fails_closed(self) -> None:
        self.assertIsNone(
            MODULE.typed_model_signature(
                {"id": "opaque-model-id"},
                {"capabilityKind": "chat", "modalityKind": "unknown"},
            )
        )

    def test_embedding_multimodal_metadata_fails_closed(self) -> None:
        self.assertIsNone(
            MODULE.typed_model_signature(
                {"id": "opaque-model-id"},
                {"capabilityKind": "embedding", "modalityKind": "multimodal"},
            )
        )


class ProviderConfigurationTests(unittest.TestCase):
    @staticmethod
    def encoded_json(value: object) -> str:
        return base64.b64encode(json.dumps(value).encode("utf-8")).decode("ascii")

    def test_manifest_loader_selects_requested_provider(self) -> None:
        payload = {
            "alpha": {
                "opaque-model-id": {
                    "capabilityKind": "chat",
                    "modalityKind": "text",
                }
            },
            "beta": {},
        }
        with mock.patch.dict(
            os.environ,
            {MODULE.MODEL_CAPABILITY_ENV: self.encoded_json(payload)},
            clear=False,
        ):
            self.assertEqual(
                MODULE.load_model_capability_manifest("alpha"), payload["alpha"]
            )

    def test_manifest_loader_rejects_duplicate_keys(self) -> None:
        duplicate_payload = base64.b64encode(
            b'{"alpha":{"opaque-model-id":{},"opaque-model-id":{}}}'
        ).decode("ascii")
        with mock.patch.dict(
            os.environ,
            {MODULE.MODEL_CAPABILITY_ENV: duplicate_payload},
            clear=False,
        ):
            with self.assertRaisesRegex(SystemExit, "is invalid"):
                MODULE.load_model_capability_manifest("alpha")

    def test_api_key_loader_returns_requested_provider_key(self) -> None:
        encoded_keys = self.encoded_json({"alpha": "secret-value", "beta": ""})
        with mock.patch.dict(
            os.environ,
            {MODULE.PROVIDER_API_KEYS_ENV: encoded_keys},
            clear=False,
        ):
            self.assertEqual(MODULE.load_api_key("alpha"), "secret-value")
            self.assertIsNone(MODULE.load_api_key("beta"))

    def test_api_key_loader_rejects_duplicate_providers(self) -> None:
        duplicate_payload = base64.b64encode(
            b'{"alpha":"first","alpha":"second"}'
        ).decode("ascii")
        with mock.patch.dict(
            os.environ,
            {MODULE.PROVIDER_API_KEYS_ENV: duplicate_payload},
            clear=False,
        ):
            with self.assertRaisesRegex(SystemExit, "is invalid"):
                MODULE.load_api_key("alpha")


if __name__ == "__main__":
    unittest.main()
