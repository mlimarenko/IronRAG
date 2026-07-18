import importlib.util
import io
import json
import pathlib
import stat
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT_PATH = pathlib.Path(__file__).resolve().parent / "profile-agent-surfaces.py"
SPEC = importlib.util.spec_from_file_location("profile_agent_surfaces", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AgentSurfaceProfileTests(unittest.TestCase):
    def test_profile_rejects_raw_mcp_token_cli_argument(self) -> None:
        with mock.patch.object(MODULE.sys, "stderr", io.StringIO()):
            with self.assertRaises(SystemExit):
                MODULE.parse_args(
                    [
                        "--library-id",
                        "library-1",
                        "--mcp-token",
                        "private-cli-token",
                    ]
                )

    def test_probe_inputs_accept_private_suite_control_payload_over_stdin(self) -> None:
        args = MODULE.parse_args(
            ["--library-id", "library-1", "--probe-input-stdin"]
        )
        control_payload = {
            "password": "private-password",
            "mcpToken": "private-token",
            "question": "Приватный проверочный вопрос",
        }

        with mock.patch.object(
            MODULE.sys,
            "stdin",
            io.StringIO(json.dumps(control_payload, ensure_ascii=False)),
        ):
            inputs = MODULE.resolve_probe_inputs(args)

        self.assertEqual(inputs.password, control_payload["password"])
        self.assertEqual(inputs.mcp_token, control_payload["mcpToken"])
        self.assertEqual(inputs.question, control_payload["question"])

    def test_sanitized_subprocess_environment_removes_probe_secrets(self) -> None:
        environment = MODULE.sanitized_subprocess_environment(
            {
                "PATH": "/usr/bin",
                MODULE.MCP_TOKEN_ENV: "private-token",
                MODULE.PROBE_PASSWORD_ENV: "private-password",
                MODULE.PROBE_QUESTION_ENV: "private-question",
            }
        )

        self.assertEqual(environment, {"PATH": "/usr/bin"})

    def test_private_report_writer_enforces_directory_and_file_modes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_root:
            reports_dir = pathlib.Path(temporary_root) / "reports"
            MODULE.ensure_private_directory(reports_dir)
            report_path = reports_dir / "profile.md"
            MODULE.write_private_text(report_path, "private report")

            self.assertEqual(stat.S_IMODE(reports_dir.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(report_path.stat().st_mode), 0o600)
            self.assertEqual(report_path.read_text(encoding="utf-8"), "private report")

    def test_assistant_turn_request_keeps_question_out_of_process_argv(self) -> None:
        question = "private acceptance question"

        args, payload = MODULE.build_assistant_turn_curl_request(
            base_url="https://example.invalid",
            cookie_jar="/secure/cookies",
            session_id="session-1",
            question=question,
            top_k=12,
            timeout_seconds=90,
        )

        self.assertNotIn(question, "\0".join(args))
        self.assertEqual(payload, '{"contentText": "private acceptance question", "topK": 12}')
        self.assertIn("@-", args)

    def test_curl_header_lines_reject_newline_injection(self) -> None:
        with self.assertRaises(ValueError):
            MODULE.safe_curl_header_line("Authorization", "Bearer safe\nInjected: value")

    def test_curl_response_headers_use_the_last_http_block_and_lowercase_names(self) -> None:
        headers = MODULE.parse_curl_response_headers(
            "HTTP/1.1 100 Continue\r\nX-Ignored: first\r\n\r\n"
            "HTTP/2 200\r\nMcp-Session-Id: session-1\r\nX-Request-Id: request-1\r\n\r\n"
        )

        self.assertEqual(
            headers,
            {"mcp-session-id": "session-1", "x-request-id": "request-1"},
        )

    def test_mcp_requests_initialize_once_and_echo_strict_transport_headers(self) -> None:
        session = MODULE.CurlSession("https://example.invalid")
        initialize = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={"jsonrpc": "2.0", "result": {}},
            response_headers={MODULE.MCP_SESSION_HEADER: "session-1"},
        )
        tool_result = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={"jsonrpc": "2.0", "result": {}},
        )
        try:
            with mock.patch.object(
                session,
                "request_json",
                side_effect=[initialize, tool_result, tool_result],
            ) as request_json:
                for request_id in ("tool-1", "tool-2"):
                    session.mcp_request_json(
                        MODULE.MCP_ANSWER_ROUTE,
                        body={
                            "jsonrpc": "2.0",
                            "id": request_id,
                            "method": "tools/list",
                            "params": {},
                        },
                        bearer_token="private-token",
                    )

            self.assertEqual(request_json.call_count, 3)
            initialize_body = request_json.call_args_list[0].kwargs["body"]
            self.assertEqual(initialize_body["method"], "initialize")
            self.assertEqual(
                initialize_body["params"]["protocolVersion"],
                MODULE.MCP_PROTOCOL_VERSION,
            )
            for call in request_json.call_args_list[1:]:
                self.assertEqual(
                    call.kwargs["headers"],
                    {
                        "Content-Type": "application/json",
                        MODULE.MCP_PROTOCOL_HEADER: MODULE.MCP_PROTOCOL_VERSION,
                        MODULE.MCP_SESSION_HEADER: "session-1",
                    },
                )
        finally:
            session._mcp_sessions.clear()
            session.cleanup()

    def test_json_request_keeps_bearer_and_body_out_of_process_argv(self) -> None:
        bearer = "private-bearer-value"
        private_body = "private request body"
        observed_headers = ""

        def fake_run(args, **kwargs):
            nonlocal observed_headers
            argv = "\0".join(args)
            self.assertNotIn(bearer, argv)
            self.assertNotIn(private_body, argv)
            header_index = args.index("--header") + 1
            header_reference = args[header_index]
            self.assertTrue(header_reference.startswith("@"))
            observed_headers = pathlib.Path(header_reference[1:]).read_text(encoding="utf-8")
            self.assertEqual(kwargs["input"], '{"question": "private request body"}')
            self.assertNotIn(MODULE.MCP_TOKEN_ENV, kwargs["env"])
            self.assertNotIn(MODULE.PROBE_PASSWORD_ENV, kwargs["env"])
            self.assertNotIn(MODULE.PROBE_QUESTION_ENV, kwargs["env"])
            return mock.Mock(
                returncode=0,
                stdout='{}\n__CURL_METRICS__ 200 0.01 2',
                stderr="",
            )

        session = MODULE.CurlSession("https://example.invalid")
        try:
            with mock.patch.object(MODULE.subprocess, "run", side_effect=fake_run):
                sample = session.request_json(
                    "POST",
                    "/v1/mcp",
                    body={"question": private_body},
                    headers={"Content-Type": "application/json"},
                    bearer_token=bearer,
                )
        finally:
            session.cleanup()

        self.assertEqual(sample.status_code, 200)
        self.assertIn(f"Authorization: Bearer {bearer}", observed_headers)

    def test_probe_mcp_tool_uses_diagnostics_surface_for_raw_tools(self) -> None:
        class FakeSession:
            def __init__(self) -> None:
                self.calls = []

            def mcp_request_json(self, uri, **kwargs):
                self.calls.append(("POST", uri, kwargs))
                return MODULE.CurlSample(
                    status_code=200,
                    time_total_s=0.01,
                    size_download_bytes=2,
                    payload={"jsonrpc": "2.0", "result": {}},
                )

        session = FakeSession()

        MODULE.probe_mcp_tool(
            session,
            bearer_token=None,
            tool_name="search_entities",
            arguments={"library": "workspace/library", "query": "orion"},
        )

        self.assertEqual(session.calls[0][1], MODULE.MCP_DIAGNOSTICS_ROUTE)

    def test_document_search_arguments_use_canonical_library_refs(self) -> None:
        arguments = MODULE.build_document_search_arguments("workspace/library", "alpha", 5)

        self.assertEqual(arguments["libraries"], ["workspace/library"])
        self.assertEqual(arguments["query"], "alpha")
        self.assertEqual(arguments["limit"], 5)
        self.assertTrue(arguments["includeReferences"])
        self.assertNotIn("libraryIds", arguments)

    def test_library_catalog_context_uses_direct_library_lookup(self) -> None:
        class FakeSession:
            def __init__(self) -> None:
                self.calls = []

            def request_json(self, method, uri, **kwargs):
                self.calls.append((method, uri, kwargs))
                if uri == "/v1/catalog/libraries/lib-1":
                    return MODULE.CurlSample(
                        status_code=200,
                        time_total_s=0.01,
                        size_download_bytes=2,
                        payload={
                            "id": "lib-1",
                            "workspaceId": "workspace-1",
                            "slug": "library",
                        },
                    )
                if uri == "/v1/catalog/workspaces/workspace-1":
                    return MODULE.CurlSample(
                        status_code=200,
                        time_total_s=0.01,
                        size_download_bytes=2,
                        payload={"id": "workspace-1", "slug": "workspace"},
                    )
                raise AssertionError(f"unexpected request {method} {uri}")

        session = FakeSession()

        context = MODULE.discover_library_catalog_context(session, "lib-1")

        self.assertEqual(context.workspace_id, "workspace-1")
        self.assertEqual(context.catalog_ref, "workspace/library")
        self.assertEqual(
            [call[1] for call in session.calls],
            ["/v1/catalog/libraries/lib-1", "/v1/catalog/workspaces/workspace-1"],
        )

    def test_probe_mcp_tool_can_target_answer_surface_explicitly(self) -> None:
        class FakeSession:
            def __init__(self) -> None:
                self.calls = []

            def mcp_request_json(self, uri, **kwargs):
                self.calls.append(("POST", uri, kwargs))
                return MODULE.CurlSample(
                    status_code=200,
                    time_total_s=0.01,
                    size_download_bytes=2,
                    payload={"jsonrpc": "2.0", "result": {}},
                )

        session = FakeSession()

        MODULE.probe_mcp_tool(
            session,
            bearer_token=None,
            tool_name="grounded_answer",
            arguments={"library": "workspace/library", "query": "What is Orion?"},
            route=MODULE.MCP_ANSWER_ROUTE,
        )

        self.assertEqual(session.calls[0][1], MODULE.MCP_ANSWER_ROUTE)

    def test_probe_mcp_tool_uses_unique_request_ids(self) -> None:
        class FakeSession:
            def __init__(self) -> None:
                self.calls = []

            def mcp_request_json(self, uri, **kwargs):
                self.calls.append(("POST", uri, kwargs))
                return MODULE.CurlSample(
                    status_code=200,
                    time_total_s=0.01,
                    size_download_bytes=2,
                    payload={"jsonrpc": "2.0", "result": {}},
                )

        session = FakeSession()
        arguments = {"library": "workspace/library", "query": "What is Orion?"}

        MODULE.probe_mcp_tool(
            session,
            bearer_token=None,
            tool_name="grounded_answer",
            arguments=arguments,
            route=MODULE.MCP_ANSWER_ROUTE,
        )
        MODULE.probe_mcp_tool(
            session,
            bearer_token=None,
            tool_name="grounded_answer",
            arguments=arguments,
            route=MODULE.MCP_ANSWER_ROUTE,
        )

        request_ids = [call[2]["body"]["id"] for call in session.calls]
        self.assertNotEqual(request_ids[0], request_ids[1])
        self.assertTrue(all(value.startswith("agent-probe-grounded_answer-") for value in request_ids))

    def test_answer_mcp_discovery_requires_grounded_answer_and_matching_contract(self) -> None:
        contract_hash = "sha256:" + "a" * 64
        capabilities = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "tools": ["grounded_answer", "list_libraries"],
                "toolContractVersion": 1,
                "toolContractHash": contract_hash,
            },
        )
        tools_list = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "jsonrpc": "2.0",
                "result": {
                    "tools": [{"name": "grounded_answer"}, {"name": "list_libraries"}],
                    "_meta": {
                        "ironrag/toolContractVersion": 1,
                        "ironrag/toolContractHash": contract_hash,
                    },
                },
            },
        )

        self.assertEqual(
            MODULE.validate_answer_mcp_discovery(capabilities, tools_list),
            contract_hash,
        )

    def test_answer_mcp_discovery_fails_before_question_when_tool_is_missing(self) -> None:
        contract_hash = "sha256:" + "b" * 64
        capabilities = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "tools": ["list_libraries"],
                "toolContractVersion": 1,
                "toolContractHash": contract_hash,
            },
        )
        tools_list = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "jsonrpc": "2.0",
                "result": {
                    "tools": [{"name": "list_libraries"}],
                    "_meta": {
                        "ironrag/toolContractVersion": 1,
                        "ironrag/toolContractHash": contract_hash,
                    },
                },
            },
        )

        with self.assertRaisesRegex(RuntimeError, "grounded_answer"):
            MODULE.validate_answer_mcp_discovery(capabilities, tools_list)

    def test_answer_mcp_discovery_rejects_contract_hash_drift(self) -> None:
        capabilities = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "tools": ["grounded_answer"],
                "toolContractVersion": 1,
                "toolContractHash": "sha256:" + "c" * 64,
            },
        )
        tools_list = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "jsonrpc": "2.0",
                "result": {
                    "tools": [{"name": "grounded_answer"}],
                    "_meta": {
                        "ironrag/toolContractVersion": 1,
                        "ironrag/toolContractHash": "sha256:" + "d" * 64,
                    },
                },
            },
        )

        with self.assertRaisesRegex(RuntimeError, "contract"):
            MODULE.validate_answer_mcp_discovery(capabilities, tools_list)

    def test_answer_mcp_discovery_rejects_missing_or_non_positive_versions(self) -> None:
        contract_hash = "sha256:" + "e" * 64
        for version in (None, 0, -1, True, "1"):
            with self.subTest(version=version):
                capabilities = MODULE.CurlSample(
                    status_code=200,
                    time_total_s=0.01,
                    size_download_bytes=2,
                    payload={
                        "tools": ["grounded_answer"],
                        "toolContractVersion": version,
                        "toolContractHash": contract_hash,
                    },
                )
                tools_list = MODULE.CurlSample(
                    status_code=200,
                    time_total_s=0.01,
                    size_download_bytes=2,
                    payload={
                        "jsonrpc": "2.0",
                        "result": {
                            "tools": [{"name": "grounded_answer"}],
                            "_meta": {
                                "ironrag/toolContractVersion": version,
                                "ironrag/toolContractHash": contract_hash,
                            },
                        },
                    },
                )

                with self.assertRaisesRegex(RuntimeError, "version"):
                    MODULE.validate_answer_mcp_discovery(capabilities, tools_list)

    def test_answer_mcp_discovery_rejects_noncanonical_hash_shape(self) -> None:
        capabilities = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "tools": ["grounded_answer"],
                "toolContractVersion": 1,
                "toolContractHash": "sha256:not-a-digest",
            },
        )
        tools_list = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "jsonrpc": "2.0",
                "result": {
                    "tools": [{"name": "grounded_answer"}],
                    "_meta": {
                        "ironrag/toolContractVersion": 1,
                        "ironrag/toolContractHash": "sha256:not-a-digest",
                    },
                },
            },
        )

        with self.assertRaisesRegex(RuntimeError, "hash"):
            MODULE.validate_answer_mcp_discovery(capabilities, tools_list)

    def test_answer_mcp_discovery_rejects_visible_tool_set_drift(self) -> None:
        contract_hash = "sha256:" + "f" * 64
        capabilities = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "tools": ["grounded_answer", "list_libraries"],
                "toolContractVersion": 1,
                "toolContractHash": contract_hash,
            },
        )
        tools_list = MODULE.CurlSample(
            status_code=200,
            time_total_s=0.01,
            size_download_bytes=2,
            payload={
                "jsonrpc": "2.0",
                "result": {
                    "tools": [{"name": "grounded_answer"}, {"name": "search_documents"}],
                    "_meta": {
                        "ironrag/toolContractVersion": 1,
                        "ironrag/toolContractHash": contract_hash,
                    },
                },
            },
        )

        with self.assertRaisesRegex(RuntimeError, "tool set"):
            MODULE.validate_answer_mcp_discovery(capabilities, tools_list)

    def test_quality_tokenization_is_unicode_agnostic(self) -> None:
        self.assertEqual(
            MODULE.tokenize_quality_text("Alpha/Бета-42"),
            ("alpha", "бета", "42"),
        )
        self.assertEqual(
            MODULE.normalize_quality_text("Checkout.Endpoint / 支払い"),
            "checkout endpoint 支払い",
        )

    def test_answer_overlap_detects_unrelated_verified_text(self) -> None:
        related = MODULE.answer_token_overlap_ratio(
            "Alpha Gateway connects to the checkout endpoint.",
            "The checkout endpoint is served by Alpha Gateway.",
        )
        unrelated = MODULE.answer_token_overlap_ratio(
            "Alpha Gateway connects to the checkout endpoint.",
            "No supported evidence is available for this request.",
        )

        self.assertIsNotNone(related)
        self.assertIsNotNone(unrelated)
        self.assertGreater(related, 0.5)
        self.assertLess(unrelated, 0.16)

    def test_summarize_graph_quality_detects_document_coverage_and_duplicates(self) -> None:
        summary = MODULE.summarize_graph_quality(
            {
                "documents": [
                    {"documentId": "doc-1", "title": "Primary"},
                    {"documentId": "doc-2", "title": "Secondary"},
                ],
                "entities": [
                    {"entityId": "entity-1", "label": "Orion", "supportCount": 10},
                    {"entityId": "entity-2", "label": "orion", "supportCount": 8},
                ],
                "relations": [
                    {
                        "relationId": "rel-1",
                        "sourceEntityId": "entity-1",
                        "targetEntityId": "entity-2",
                        "relationType": "depends_on",
                        "supportCount": 5,
                    },
                    {
                        "relationId": "rel-2",
                        "sourceEntityId": "entity-1",
                        "targetEntityId": "entity-2",
                        "relationType": "depends_on",
                        "supportCount": 4,
                    },
                ],
                "documentLinks": [
                    {
                        "documentId": "doc-1",
                        "targetNodeId": "entity-1",
                        "targetNodeType": "entity",
                        "relationType": "supports",
                        "supportCount": 3,
                    },
                    {
                        "documentId": "doc-2",
                        "targetNodeId": "rel-1",
                        "targetNodeType": "relation",
                        "relationType": "supports",
                        "supportCount": 1,
                    },
                    {
                        "documentId": "doc-missing",
                        "targetNodeId": "entity-1",
                        "targetNodeType": "entity",
                        "relationType": "supports",
                        "supportCount": 1,
                    },
                ],
            }
        )

        self.assertEqual(summary.orphan_document_count, 1)
        self.assertTrue(summary.document_rank_monotonic)
        self.assertEqual(summary.duplicate_entity_label_count, 1)
        self.assertEqual(summary.duplicate_relation_signature_count, 1)
        self.assertEqual(summary.quality_status, "broken")

    def test_summarize_relation_list_detects_unknown_and_duplicate_signatures(self) -> None:
        summary = MODULE.summarize_relation_list(
            [
                {
                    "relationId": "rel-1",
                    "sourceLabel": "Orion",
                    "targetLabel": "Atlas",
                    "relationType": "depends_on",
                },
                {
                    "relationId": "rel-2",
                    "sourceLabel": "orion",
                    "targetLabel": "atlas",
                    "relationType": "depends_on",
                },
                {
                    "relationId": "rel-3",
                    "sourceLabel": "unknown",
                    "targetLabel": "Atlas",
                    "relationType": "mentions",
                },
            ]
        )

        self.assertEqual(summary.row_count, 3)
        self.assertEqual(summary.unknown_label_count, 1)
        self.assertEqual(summary.duplicate_signature_count, 1)

    def test_summarize_relation_list_accepts_structured_content_wrapper(self) -> None:
        summary = MODULE.summarize_relation_list(
            {
                "relations": [
                    {
                        "relationId": "rel-1",
                        "sourceLabel": "System Information Endpoint",
                        "targetLabel": "GET",
                        "relationType": "configures",
                    }
                ]
            }
        )

        self.assertEqual(summary.row_count, 1)
        self.assertEqual(summary.unknown_label_count, 0)
        self.assertEqual(summary.duplicate_signature_count, 0)

    def test_summarize_grounded_answer_extracts_core_fields(self) -> None:
        summary = MODULE.summarize_grounded_answer(
            {
                "executionDetail": {
                    "responseTurn": {
                        "contentText": "Orion connects to Atlas using JSON-RPC.",
                    },
                    "verificationState": "verified",
                    "execution": {
                        "runtimeExecutionId": "runtime-grounded-1",
                    },
                    "chunkReferences": [{"chunkId": "chunk-1"}],
                    "preparedSegmentReferences": [{"segmentId": "segment-2"}],
                    "technicalFactReferences": [{"factId": "fact-2"}],
                    "entityReferences": [{"nodeId": "node-1"}],
                    "relationReferences": [{"edgeId": "rel-3"}],
                }
            }
        )

        self.assertEqual(summary.answer_text, "Orion connects to Atlas using JSON-RPC.")
        self.assertEqual(summary.verifier_level, "verified")
        self.assertEqual(summary.runtime_execution_id, "runtime-grounded-1")
        self.assertEqual(
            summary.references,
            (
                "chunk|chunk-1",
                "entity|node-1",
                "fact|fact-2",
                "relation|rel-3",
                "segment|segment-2",
            ),
        )

    def test_summarize_grounded_answer_understands_compact_profile(self) -> None:
        summary = MODULE.summarize_grounded_answer(
            {
                "responseProfile": "compact",
                "answerBody": "Orion uses the documented recovery sequence.",
                "verifier": {"state": "verified"},
                "runtimeExecutionId": "runtime-compact-1",
                "referenceSummary": {
                    "totalCount": 12,
                    "returnedCount": 5,
                    "truncated": True,
                    "references": [
                        {"kind": "chunk", "chunkId": "chunk-1"},
                        {"kind": "prepared_segment", "segmentId": "segment-2"},
                        {"kind": "technical_fact", "factId": "fact-3"},
                        {"kind": "entity", "nodeId": "node-4"},
                        {"kind": "relation", "edgeId": "edge-5"},
                    ],
                },
                "executionDetail": {
                    "execution": {"lifecycleState": "completed"},
                    "verificationState": "verified",
                },
            }
        )

        self.assertEqual(summary.answer_text, "Orion uses the documented recovery sequence.")
        self.assertEqual(summary.verifier_level, "verified")
        self.assertEqual(summary.runtime_execution_id, "runtime-compact-1")
        self.assertEqual(
            summary.references,
            (
                "chunk|chunk-1",
                "entity|node-4",
                "fact|fact-3",
                "relation|edge-5",
                "segment|segment-2",
            ),
        )

    def test_grounded_answer_probe_arguments_request_bounded_compact_profile(self) -> None:
        arguments = MODULE.build_grounded_answer_probe_arguments(
            "workspace/library",
            "What changed?",
            24,
        )

        self.assertEqual(arguments["responseProfile"], "compact")
        self.assertGreaterEqual(arguments["maxReferences"], 1)
        self.assertLessEqual(arguments["maxReferences"], 8)

    def test_summarize_assistant_turn_artifacts_captures_text_and_references(self) -> None:
        summary = MODULE.summarize_assistant_turn_artifacts(
            {
                "responseTurn": {
                    "contentText": "System reports Orion status and Atlas state.",
                },
                "chunkReferences": [
                    {"chunkId": "chunk-1"},
                    {"chunkId": "chunk-2"},
                ],
                "entityReferences": [
                    {"nodeId": "node-1"},
                ],
                "relationReferences": [
                    {"edgeId": "rel-1"},
                ],
                "preparedSegmentReferences": [
                    {"segmentId": "segment-1"},
                ],
                "technicalFactReferences": [
                    {"factId": "fact-1"},
                ],
                "verificationState": "verified",
                "execution": {
                    "runtimeExecutionId": "runtime-ui-1",
                },
            }
        )

        self.assertEqual(summary.answer_text, "System reports Orion status and Atlas state.")
        self.assertEqual(summary.verifier_level, "verified")
        self.assertEqual(summary.runtime_execution_id, "runtime-ui-1")
        self.assertEqual(
            summary.references,
            (
                "chunk|chunk-1",
                "chunk|chunk-2",
                "entity|node-1",
                "fact|fact-1",
                "relation|rel-1",
                "segment|segment-1",
            ),
        )

    def test_gate_checks_fail_on_graph_and_document_alignment_regressions(self) -> None:
        checks = MODULE.build_gate_checks(
            MODULE.GateInputs(
                entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=2,
                top_label="Orion",
                top_score=10.0,
            ),
                document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=128,
                top_excerpt_length=240,
                top_chunk_reference_count=2,
                top_score=5.0,
            ),
                document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-2",
                document_title="Secondary",
                readability_state="readable",
                content_length=512,
                total_reference_count=3,
                has_more=False,
                slice_start_offset=64,
                slice_end_offset=576,
            ),
                graph_quality=MODULE.McpQualitySummary(
                entity_count=2,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=1,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=False,
                duplicate_entity_label_count=1,
                duplicate_relation_signature_count=0,
                top_entity_label="Orion",
                probe_entity_label=None,
                visible_entity_labels_normalized=("orion",),
            ),
                relation_list_summary=MODULE.RelationListSummary(
                row_count=2,
                unknown_label_count=1,
                duplicate_signature_count=0,
            ),
                community_summary=MODULE.CommunitySummary(
                count=0,
                communities_with_summary=0,
                top_entity_count=0,
            ),
                assistant_summaries=[],
                runtime_execution_summary=None,
                runtime_trace_summary=None,
                legacy_runtime_execution_error=None,
                grounded_answer_summary=None,
                assistant_require_all=[],
                assistant_forbid_any=[],
                expected_search_top_label=None,
                tool_samples=[],
            ),
            MODULE.GateThresholds(
                graph_min_entities=1,
                graph_min_relations=1,
                graph_min_documents=1,
                community_min_count=1,
                entity_search_min_hits=1,
                search_min_hits=1,
                search_min_readable_hits=1,
                read_min_content_chars=100,
                read_min_references=1,
                assistant_min_references=1,
                assistant_expected_verification="verified",
                max_tool_latency_ms=None,
                max_completed_ms=None,
                max_first_frame_ms=None,
                min_answer_overlap_ratio=MODULE.DEFAULT_MIN_ANSWER_OVERLAP_RATIO,
            ),
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["graph.document_links_visible_documents"].status, "fail")
        self.assertEqual(by_label["graph.documents_ranked_by_support"].status, "fail")
        self.assertEqual(by_label["graph.duplicate_entity_labels"].status, "fail")
        self.assertEqual(by_label["graph.list_relations_labels"].status, "fail")
        self.assertEqual(by_label["mcp.read_document_alignment"].status, "fail")
        self.assertEqual(by_label["mcp.read_document_offset_alignment"].status, "fail")

    def test_graph_search_alignment_passes_when_top_hit_is_visible_not_top_ranked(self) -> None:
        checks = MODULE.build_gate_checks(
            MODULE.GateInputs(
                entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=1,
                top_label="checkout server",
                top_score=10.0,
            ),
                document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=0,
                top_excerpt_length=240,
                top_chunk_reference_count=2,
                top_score=5.0,
            ),
                document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-1",
                document_title="Primary",
                readability_state="readable",
                content_length=512,
                total_reference_count=3,
                has_more=False,
                slice_start_offset=0,
                slice_end_offset=512,
            ),
                graph_quality=MODULE.McpQualitySummary(
                entity_count=3,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=0,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=True,
                duplicate_entity_label_count=0,
                duplicate_relation_signature_count=0,
                top_entity_label="HTTP",
                probe_entity_label=None,
                visible_entity_labels_normalized=("http", "checkout server", "system information endpoint"),
            ),
                relation_list_summary=MODULE.RelationListSummary(
                row_count=1,
                unknown_label_count=0,
                duplicate_signature_count=0,
            ),
                community_summary=MODULE.CommunitySummary(
                count=1,
                communities_with_summary=1,
                top_entity_count=2,
            ),
                assistant_summaries=[],
                runtime_execution_summary=None,
                runtime_trace_summary=None,
                legacy_runtime_execution_error=None,
                grounded_answer_summary=None,
                assistant_require_all=[],
                assistant_forbid_any=[],
                expected_search_top_label=None,
                tool_samples=[],
            ),
            MODULE.GateThresholds(
                graph_min_entities=1,
                graph_min_relations=1,
                graph_min_documents=1,
                community_min_count=0,
                entity_search_min_hits=1,
                search_min_hits=1,
                search_min_readable_hits=1,
                read_min_content_chars=100,
                read_min_references=1,
                assistant_min_references=1,
                assistant_expected_verification="verified",
                max_tool_latency_ms=None,
                max_completed_ms=None,
                max_first_frame_ms=None,
                min_answer_overlap_ratio=MODULE.DEFAULT_MIN_ANSWER_OVERLAP_RATIO,
            ),
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["graph.search_alignment"].status, "pass")

    def test_gate_checks_pass_when_grounded_answer_matches_ui_turn(self) -> None:
        checks = MODULE.build_gate_checks(
            MODULE.GateInputs(
                entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=1,
                top_label="Orion",
                top_score=10.0,
            ),
                document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=0,
                top_excerpt_length=120,
                top_chunk_reference_count=1,
                top_score=5.0,
            ),
                document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-1",
                document_title="Primary",
                readability_state="readable",
                content_length=300,
                total_reference_count=2,
                has_more=False,
                slice_start_offset=0,
                slice_end_offset=300,
            ),
                graph_quality=MODULE.McpQualitySummary(
                entity_count=1,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=0,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=True,
                duplicate_entity_label_count=0,
                duplicate_relation_signature_count=0,
                top_entity_label="Orion",
                probe_entity_label=None,
                visible_entity_labels_normalized=("orion",),
            ),
                relation_list_summary=MODULE.RelationListSummary(
                row_count=1,
                unknown_label_count=0,
                duplicate_signature_count=0,
            ),
                community_summary=MODULE.CommunitySummary(
                count=1,
                communities_with_summary=1,
                top_entity_count=1,
            ),
                assistant_summaries=[
                MODULE.AssistantTurnSummary(
                    time_to_completed_s=0.5,
                    answer_length=21,
                    answer_text="System reports Orion",
                    total_reference_count=1,
                    verification_state="verified",
                    completion_state="completed",
                    query_execution_id="query-1",
                    runtime_execution_id="runtime-1",
                    references=("chunk|chunk-1",),
                )
            ],
                runtime_execution_summary=MODULE.RuntimeExecutionProbeSummary(
                runtime_execution_id="runtime-1",
                lifecycle_state="completed",
                active_stage="verification",
            ),
                runtime_trace_summary=MODULE.RuntimeTraceProbeSummary(
                runtime_execution_id="runtime-1",
                stage_count=1,
                action_count=1,
                policy_decision_count=0,
            ),
                legacy_runtime_execution_error=MODULE.ToolErrorSummary(
                error_kind="invalid_mcp_tool_call",
                message="invalid request: expected runtimeExecutionId",
            ),
                grounded_answer_summary=MODULE.GroundedAnswerSummary(
                answer_text="System reports Orion",
                verifier_level="verified",
                runtime_execution_id="runtime-1",
                references=("chunk|chunk-1",),
            ),
                assistant_require_all=[],
                assistant_forbid_any=[],
                expected_search_top_label=None,
                tool_samples=[],
            ),
            MODULE.GateThresholds(
                graph_min_entities=1,
                graph_min_relations=1,
                graph_min_documents=1,
                community_min_count=1,
                entity_search_min_hits=1,
                search_min_hits=1,
                search_min_readable_hits=1,
                read_min_content_chars=100,
                read_min_references=1,
                assistant_min_references=1,
                assistant_expected_verification="verified",
                max_tool_latency_ms=None,
                max_completed_ms=None,
                max_first_frame_ms=None,
                min_answer_overlap_ratio=MODULE.DEFAULT_MIN_ANSWER_OVERLAP_RATIO,
            ),
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["mcp.grounded_answer.verifier"].status, "pass")
        self.assertEqual(by_label["mcp.grounded_answer.references"].status, "pass")
        self.assertEqual(by_label["mcp.grounded_answer.runtime_execution_id"].status, "pass")
        self.assertEqual(by_label["assistant.run_1.mcp_answer_quality_parity"].status, "pass")

    def test_gate_checks_fail_when_grounded_answer_quality_is_degraded(self) -> None:
        checks = MODULE.build_gate_checks(
            MODULE.GateInputs(
                entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=1,
                top_label="Orion",
                top_score=10.0,
            ),
                document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=0,
                top_excerpt_length=120,
                top_chunk_reference_count=1,
                top_score=5.0,
            ),
                document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-1",
                document_title="Primary",
                readability_state="readable",
                content_length=300,
                total_reference_count=2,
                has_more=False,
                slice_start_offset=0,
                slice_end_offset=300,
            ),
                graph_quality=MODULE.McpQualitySummary(
                entity_count=1,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=0,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=True,
                duplicate_entity_label_count=0,
                duplicate_relation_signature_count=0,
                top_entity_label="Orion",
                probe_entity_label=None,
                visible_entity_labels_normalized=("orion",),
            ),
                relation_list_summary=MODULE.RelationListSummary(
                row_count=1,
                unknown_label_count=0,
                duplicate_signature_count=0,
            ),
                community_summary=MODULE.CommunitySummary(
                count=1,
                communities_with_summary=1,
                top_entity_count=1,
            ),
                assistant_summaries=[
                MODULE.AssistantTurnSummary(
                    time_to_completed_s=0.5,
                    answer_length=21,
                    answer_text="System reports Orion",
                    total_reference_count=1,
                    verification_state="verified",
                    completion_state="completed",
                    query_execution_id="query-1",
                    runtime_execution_id="runtime-1",
                    references=("chunk|chunk-1",),
                )
            ],
                runtime_execution_summary=MODULE.RuntimeExecutionProbeSummary(
                runtime_execution_id="runtime-1",
                lifecycle_state="completed",
                active_stage="verification",
            ),
                runtime_trace_summary=MODULE.RuntimeTraceProbeSummary(
                runtime_execution_id="runtime-1",
                stage_count=1,
                action_count=1,
                policy_decision_count=0,
            ),
                legacy_runtime_execution_error=MODULE.ToolErrorSummary(
                error_kind="invalid_mcp_tool_call",
                message="invalid request: expected runtimeExecutionId",
            ),
                grounded_answer_summary=MODULE.GroundedAnswerSummary(
                answer_text="Different text from UI",
                verifier_level="partially_supported",
                runtime_execution_id="runtime-2",
                references=("chunk|chunk-2",),
            ),
                assistant_require_all=[],
                assistant_forbid_any=[],
                expected_search_top_label=None,
                tool_samples=[],
            ),
            MODULE.GateThresholds(
                graph_min_entities=1,
                graph_min_relations=1,
                graph_min_documents=1,
                community_min_count=1,
                entity_search_min_hits=1,
                search_min_hits=1,
                search_min_readable_hits=1,
                read_min_content_chars=100,
                read_min_references=1,
                assistant_min_references=1,
                assistant_expected_verification="verified",
                max_tool_latency_ms=None,
                max_completed_ms=None,
                max_first_frame_ms=None,
                min_answer_overlap_ratio=MODULE.DEFAULT_MIN_ANSWER_OVERLAP_RATIO,
            ),
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["mcp.grounded_answer.verifier"].status, "fail")
        self.assertEqual(by_label["mcp.grounded_answer.references"].status, "pass")
        self.assertEqual(by_label["mcp.grounded_answer.runtime_execution_id"].status, "pass")
        self.assertEqual(by_label["assistant.run_1.mcp_answer_quality_parity"].status, "fail")

    def test_runtime_and_community_summaries_capture_canonical_fields(self) -> None:
        communities = MODULE.summarize_communities(
            {
                "communities": [
                    {
                        "communityId": 1,
                        "summary": "Checkout services",
                        "topEntities": ["Checkout", "Inventory"],
                    }
                ]
            }
        )
        runtime_execution = MODULE.summarize_runtime_execution(
            {
                "runtimeExecutionId": "runtime-1",
                "lifecycleState": "completed",
                "activeStage": "verification",
            }
        )
        runtime_trace = MODULE.summarize_runtime_trace(
            {
                "execution": {"runtimeExecutionId": "runtime-1"},
                "stages": [{"stageKind": "retrieve"}],
                "actions": [{"actionKind": "tool"}],
                "policyDecisions": [{"decisionKind": "allow"}],
            }
        )

        self.assertEqual(communities.count, 1)
        self.assertEqual(communities.communities_with_summary, 1)
        self.assertEqual(communities.top_entity_count, 2)
        self.assertEqual(runtime_execution.runtime_execution_id, "runtime-1")
        self.assertEqual(runtime_execution.lifecycle_state, "completed")
        self.assertEqual(runtime_trace.runtime_execution_id, "runtime-1")
        self.assertEqual(runtime_trace.stage_count, 1)

    def test_gate_checks_require_runtime_alignment_when_assistant_returns_runtime_id(self) -> None:
        checks = MODULE.build_gate_checks(
            MODULE.GateInputs(
                entity_search_summary=MODULE.EntitySearchSummary(
                hit_count=1,
                top_label="Orion",
                top_score=10.0,
            ),
                document_search_summary=MODULE.DocumentSearchSummary(
                hit_count=1,
                readable_hit_count=1,
                top_document_id="doc-1",
                top_document_title="Primary",
                top_suggested_start_offset=0,
                top_excerpt_length=120,
                top_chunk_reference_count=1,
                top_score=5.0,
            ),
                document_read_summary=MODULE.DocumentReadSummary(
                document_id="doc-1",
                document_title="Primary",
                readability_state="readable",
                content_length=256,
                total_reference_count=2,
                has_more=False,
                slice_start_offset=0,
                slice_end_offset=256,
            ),
                graph_quality=MODULE.McpQualitySummary(
                entity_count=1,
                relation_count=1,
                document_count=1,
                document_link_count=1,
                orphan_relation_count=0,
                orphan_link_count=0,
                orphan_document_count=0,
                entity_rank_monotonic=True,
                relation_rank_monotonic=True,
                document_rank_monotonic=True,
                duplicate_entity_label_count=0,
                duplicate_relation_signature_count=0,
                top_entity_label="Orion",
                probe_entity_label=None,
                visible_entity_labels_normalized=("orion",),
            ),
                relation_list_summary=MODULE.RelationListSummary(
                row_count=1,
                unknown_label_count=0,
                duplicate_signature_count=0,
            ),
                community_summary=MODULE.CommunitySummary(
                count=1,
                communities_with_summary=1,
                top_entity_count=1,
            ),
                assistant_summaries=[
                MODULE.AssistantTurnSummary(
                    time_to_completed_s=0.5,
                    answer_length=42,
                    answer_text="GET /system/info",
                    total_reference_count=2,
                    verification_state="verified",
                    completion_state="completed",
                    query_execution_id="query-1",
                    runtime_execution_id="runtime-1",
                )
            ],
                runtime_execution_summary=MODULE.RuntimeExecutionProbeSummary(
                runtime_execution_id="runtime-1",
                lifecycle_state="completed",
                active_stage="verification",
            ),
                runtime_trace_summary=MODULE.RuntimeTraceProbeSummary(
                runtime_execution_id="runtime-1",
                stage_count=2,
                action_count=1,
                policy_decision_count=0,
            ),
                legacy_runtime_execution_error=MODULE.ToolErrorSummary(
                error_kind="invalid_mcp_tool_call",
                message="bad request: invalid MCP tool arguments: unknown field `executionId`, expected `runtimeExecutionId`",
            ),
                grounded_answer_summary=None,
                assistant_require_all=["/system/info"],
                assistant_forbid_any=["/serverinfo"],
                expected_search_top_label=None,
                tool_samples=[],
            ),
            MODULE.GateThresholds(
                graph_min_entities=1,
                graph_min_relations=1,
                graph_min_documents=1,
                community_min_count=1,
                entity_search_min_hits=1,
                search_min_hits=1,
                search_min_readable_hits=1,
                read_min_content_chars=100,
                read_min_references=1,
                assistant_min_references=1,
                assistant_expected_verification="verified",
                max_tool_latency_ms=None,
                max_completed_ms=None,
                max_first_frame_ms=None,
                min_answer_overlap_ratio=MODULE.DEFAULT_MIN_ANSWER_OVERLAP_RATIO,
            ),
        )

        by_label = {check.label: check for check in checks}
        self.assertEqual(by_label["graph.communities"].status, "pass")
        self.assertEqual(by_label["assistant.runtime_execution_id"].status, "pass")
        self.assertEqual(by_label["mcp.get_runtime_execution_alignment"].status, "pass")
        self.assertEqual(by_label["mcp.get_runtime_execution_trace_stages"].status, "pass")
        self.assertEqual(
            by_label["mcp.get_runtime_execution_legacy_field_rejected"].status, "pass"
        )


if __name__ == "__main__":
    unittest.main()
