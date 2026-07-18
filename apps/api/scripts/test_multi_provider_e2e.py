#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("multi-provider-e2e.py")
SPEC = importlib.util.spec_from_file_location("multi_provider_e2e", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class ReportTests(unittest.TestCase):
    def test_build_report_requires_provider_answer_call(self) -> None:
        answer = {
            "responseTurn": {
                "executionId": "execution-id",
                "contentText": "37 days; Delta Ops Queue",
            },
            "verificationState": "verified",
        }
        provider_calls = [
            {
                "providerCatalogId": "provider-id",
                "providerKind": "alpha",
                "modelName": "opaque-model-id",
                "callKind": "query_answer",
                "callState": "completed",
            }
        ]

        report = MODULE.build_report(
            provider="alpha",
            provider_catalog_id="provider-id",
            workspace_id="workspace-id",
            library_id="library-id",
            credential_id="credential-id",
            document_id="document-id",
            bindings=[],
            answer=answer,
            provider_calls=provider_calls,
        )

        self.assertTrue(report["passed"])
        self.assertTrue(report["providerAnswerCallPresent"])
        self.assertEqual(report["executionId"], "execution-id")
        self.assertEqual(
            report["providerCalls"],
            [
                {
                    "providerKind": "alpha",
                    "modelName": "opaque-model-id",
                    "callKind": "query_answer",
                    "callState": "completed",
                }
            ],
        )

    def test_build_report_fails_when_answer_uses_fallback(self) -> None:
        answer = {
            "responseTurn": {
                "executionId": "execution-id",
                "contentText": "37 days; Delta Ops Queue",
            }
        }
        provider_calls = [
            {
                "providerCatalogId": "fallback-id",
                "callKind": "query_answer",
                "callState": "completed",
            }
        ]

        report = MODULE.build_report(
            provider="alpha",
            provider_catalog_id="provider-id",
            workspace_id="workspace-id",
            library_id="library-id",
            credential_id="credential-id",
            document_id="document-id",
            bindings=[],
            answer=answer,
            provider_calls=provider_calls,
        )

        self.assertFalse(report["passed"])
        self.assertFalse(report["providerAnswerCallPresent"])


if __name__ == "__main__":
    unittest.main()
