import importlib.util
import pathlib
import sys
import unittest
from unittest import mock


SCRIPT_PATH = pathlib.Path(__file__).resolve().parent / "release-check.py"
SPEC = importlib.util.spec_from_file_location("release_check", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class ReleaseCheckTests(unittest.TestCase):
    def test_response_headers_use_last_http_block(self) -> None:
        headers = MODULE.parse_curl_response_headers(
            "HTTP/1.1 100 Continue\r\nX-Discarded: first\r\n\r\n"
            "HTTP/2 200\r\nMcp-Session-Id: session-1\r\n\r\n"
        )

        self.assertEqual(headers, {MODULE.MCP_SESSION_HEADER: "session-1"})

    def test_mcp_check_uses_one_strict_session_and_terminates_it(self) -> None:
        suite = MODULE.Suite("https://example.invalid", "/tmp/cookie")
        initialize_response = (
            200,
            10.0,
            100,
            b'{"jsonrpc":"2.0","result":{"protocolVersion":"2025-11-25"}}',
            {MODULE.MCP_SESSION_HEADER: "session-1"},
        )
        tools_response = (
            200,
            10.0,
            100,
            b'{"jsonrpc":"2.0","result":{"tools":[]}}',
            {},
        )

        with (
            mock.patch.object(MODULE, "simple_get", return_value=b""),
            mock.patch.object(
                MODULE,
                "curl_json_post",
                side_effect=[initialize_response, tools_response],
            ) as post,
            mock.patch.object(
                MODULE, "curl_delete", return_value=(204, 10.0, 0)
            ) as delete,
        ):
            MODULE.check_mcp(suite)

        self.assertEqual(post.call_count, 2)
        self.assertEqual(post.call_args_list[0].kwargs["headers"], {})
        self.assertEqual(
            post.call_args_list[1].kwargs["headers"],
            {
                MODULE.MCP_PROTOCOL_HEADER: MODULE.MCP_PROTOCOL_VERSION,
                MODULE.MCP_SESSION_HEADER: "session-1",
            },
        )
        self.assertEqual(
            delete.call_args.kwargs["headers"],
            {
                MODULE.MCP_PROTOCOL_HEADER: MODULE.MCP_PROTOCOL_VERSION,
                MODULE.MCP_SESSION_HEADER: "session-1",
            },
        )
        self.assertEqual(
            [result.name for result in suite.results],
            ["mcp.initialize", "mcp.tools.list", "mcp.terminate"],
        )

    def test_mcp_check_stops_when_initialize_returns_no_session(self) -> None:
        suite = MODULE.Suite("https://example.invalid", "/tmp/cookie")
        initialize_response = (
            200,
            10.0,
            100,
            b'{"jsonrpc":"2.0","result":{"protocolVersion":"2025-11-25"}}',
            {},
        )

        with (
            mock.patch.object(MODULE, "simple_get", return_value=b""),
            mock.patch.object(
                MODULE, "curl_json_post", return_value=initialize_response
            ) as post,
            mock.patch.object(MODULE, "curl_delete") as delete,
        ):
            MODULE.check_mcp(suite)

        post.assert_called_once()
        delete.assert_not_called()
        self.assertEqual(suite.results[-1].verdict, MODULE.VERDICT_FAIL)

    def test_header_file_rejects_newline_injection(self) -> None:
        with self.assertRaises(ValueError):
            MODULE.safe_curl_header_line("Mcp-Session-Id", "safe\nInjected: value")


if __name__ == "__main__":
    unittest.main()
