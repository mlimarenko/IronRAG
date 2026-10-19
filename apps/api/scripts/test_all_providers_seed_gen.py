#!/usr/bin/env python3
from __future__ import annotations

import base64
import importlib.util
import json
import os
import unittest
from decimal import Decimal
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).with_name("all-providers-seed-gen.py")
SPEC = importlib.util.spec_from_file_location("all_providers_seed_gen", SCRIPT)
assert SPEC is not None
assert SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class TypedModelSignatureTests(unittest.TestCase):
    def test_model_name_never_implies_eligibility(self) -> None:
        for model_id in (
            "looks-like-an-embedding-model",
            "looks-like-a-vision-model",
            "looks-like-a-chat-model",
        ):
            self.assertIsNone(MODULE.typed_model_signature({"id": model_id}))

    def test_provider_typed_metadata_selects_authoritative_purposes_and_observations(self) -> None:
        signature = MODULE.typed_model_signature(
            {
                "id": "opaque-model-id",
                "metadata": {
                    "observedInputModalities": ["text", "image"],
                    "allowedBindingPurposes": [
                        "extract_text",
                        "query_compile",
                        "query_rerank",
                        "graph_entity_type",
                    ],
                },
            }
        )

        self.assertEqual(
            signature,
            (
                ["image", "text"],
                ["extract_text", "graph_entity_type", "query_compile", "query_rerank"],
            ),
        )

    def test_operator_manifest_is_typed_and_takes_precedence(self) -> None:
        signature = MODULE.typed_model_signature(
            {
                "id": "opaque-model-id",
                "allowedBindingPurposes": ["query_answer"],
            },
            {
                "observedInputModalities": ["text"],
                "allowedBindingPurposes": ["embed_chunk"],
            },
        )

        self.assertEqual(signature, (["text"], ["embed_chunk"]))

    def test_emit_uses_normalized_purpose_relation_without_scalar_authority(self) -> None:
        sql = MODULE.emit(
            "openai",
            [
                {
                    "id": "opaque-model-id",
                    "ironragCapabilities": {
                        "observedInputModalities": ["text"],
                        "allowedBindingPurposes": ["query_answer"],
                    },
                }
            ],
        )

        self.assertIn("INSERT INTO ai_model_binding_purpose", sql)
        self.assertIn("ON CONFLICT (provider_catalog_id, model_name) DO NOTHING", sql)
        self.assertIn('"observedInputModalities":["text"]', sql)
        self.assertNotIn("defaultRoles", sql)
        self.assertNotIn("capability_kind", sql)
        self.assertNotIn("modality_kind", sql)

    def test_invalid_observed_metadata_fails_closed(self) -> None:
        self.assertIsNone(
            MODULE.typed_model_signature(
                {"id": "opaque-model-id"},
                {
                    "observedInputModalities": ["text", 7],
                    "allowedBindingPurposes": ["query_answer"],
                },
            )
        )

    def test_incomplete_typed_metadata_does_not_invent_binding_purposes(self) -> None:
        self.assertIsNone(
            MODULE.typed_model_signature(
                {
                    "id": "opaque-model-id",
                    "observedInputModalities": ["text", "image"],
                }
            )
        )

    def test_observed_metadata_never_restricts_explicit_binding_purposes(self) -> None:
        self.assertEqual(
            MODULE.typed_model_signature(
                {"id": "opaque-model-id"},
                {
                    "observedInputModalities": ["custom-provider-modality"],
                    "allowedBindingPurposes": ["embed_chunk", "agent"],
                },
            ),
            (["custom-provider-modality"], ["agent", "embed_chunk"]),
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
    def test_discovered_models_get_prices_only_from_the_live_payload(self) -> None:
        # The static fallback table was removed: a discovered model without a
        # live price stays unpriced instead of inheriting a drifted number.
        self.assertFalse(hasattr(MODULE, "PRICES_USD"))
        self.assertFalse(hasattr(MODULE, "RANGE_PRICES_USD"))
        self.assertIsNone(MODULE.model_prices("openai", {"id": "gpt-6-astra"}))
        self.assertEqual(
            MODULE.model_prices(
                "openai",
                {"id": "gpt-6-astra", "pricing": {"prompt": "0.00001", "completion": "0.00005"}},
            ),
            (Decimal("10"), Decimal("50"), None),
        )

    def test_direct_provider_catalog_ids_and_effective_date_are_stable(self) -> None:
        self.assertEqual(MODULE.EFFECTIVE_FROM, "2026-07-22T00:00:00Z")
        self.assertEqual(
            MODULE.PROVIDER_CATALOG_IDS["anthropic"],
            "00000000-0000-0000-0000-000000000109",
        )
        self.assertEqual(
            MODULE.PROVIDER_CATALOG_IDS["gemini"],
            "00000000-0000-0000-0000-00000000010a",
        )


if __name__ == "__main__":
    unittest.main()
