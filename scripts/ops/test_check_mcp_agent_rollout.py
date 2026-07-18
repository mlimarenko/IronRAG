import json
import os
import pathlib
import shutil
import subprocess
import threading
import tempfile
import unittest
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


SCRIPT_PATH = pathlib.Path(__file__).resolve().parent / "check_mcp_agent_rollout.sh"
WORKSPACE_ID = "00000000-0000-7000-8000-000000000010"
LIBRARY_ID = "00000000-0000-7000-8000-000000000001"


class RolloutCheckHandler(BaseHTTPRequestHandler):
    query_ready = True
    missing_binding_purposes: list[str] = []
    direct_bindings: list[dict[str, str]] = []
    saw_login = False
    query_turn_count = 0
    login_status = 200
    libraries_status = 200
    workspace_ids: list[str] = []
    active_library_ids: list[str] = []
    shell_payload: object | None = None
    workspaces_payload: object | None = None
    libraries_payload: object | None = None
    bindings_payload: object | None = None

    def log_message(self, _format: str, *_args: object) -> None:
        return

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        if length:
            self.rfile.read(length)
        if self.path == "/v1/iam/session/login":
            self.__class__.saw_login = True
            if self.login_status != 200:
                self.send_error(self.login_status)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Set-Cookie", "ironrag_session=test-session; Path=/")
            self.end_headers()
            self.wfile.write(json.dumps({"sessionId": "test-session"}).encode())
            return

        if self.path == "/v1/query/sessions":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(json.dumps({"id": "session-1"}).encode())
            return

        if self.path == "/v1/query/sessions/session-1/turns":
            self.__class__.query_turn_count += 1
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            self.wfile.write(
                json.dumps(
                    {
                        "responseTurn": {
                            "contentText": "Grounded answer with documented evidence."
                        },
                        "verificationState": "verified",
                        "chunkReferences": [{"chunkId": "chunk-1"}],
                    }
                ).encode()
            )
            return

        self.send_error(404)

    def do_GET(self) -> None:
        if self.path == "/v1/iam/session/resolve":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            if self.shell_payload is not None:
                self.wfile.write(json.dumps(self.shell_payload).encode())
                return
            self.wfile.write(
                json.dumps(
                    {
                        "shellBootstrap": {
                            "libraries": [
                                {
                                    "id": LIBRARY_ID,
                                    "queryReady": self.query_ready,
                                    "missingBindingPurposes": self.missing_binding_purposes,
                                }
                            ]
                        }
                    }
                ).encode()
            )
            return

        if self.path == "/v1/catalog/workspaces":
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            if self.workspaces_payload is not None:
                self.wfile.write(json.dumps(self.workspaces_payload).encode())
                return
            self.wfile.write(
                json.dumps([{"id": workspace_id} for workspace_id in self.workspace_ids]).encode()
            )
            return

        if self.path.startswith("/v1/catalog/workspaces/") and self.path.endswith("/libraries"):
            if self.libraries_status != 200:
                self.send_error(self.libraries_status)
                return
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            if self.libraries_payload is not None:
                self.wfile.write(json.dumps(self.libraries_payload).encode())
                return
            self.wfile.write(
                json.dumps(
                    [
                        {"id": library_id, "lifecycleState": "active"}
                        for library_id in self.active_library_ids
                    ]
                ).encode()
            )
            return

        if self.path.startswith("/v1/ai/bindings"):
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.end_headers()
            if self.bindings_payload is not None:
                self.wfile.write(json.dumps(self.bindings_payload).encode())
                return
            self.wfile.write(json.dumps(self.direct_bindings).encode())
            return

        self.send_error(404)


class CheckMcpAgentRolloutTests(unittest.TestCase):
    def make_path_with_tools(self, tempdir: str, tools: tuple[str, ...]) -> str:
        bin_dir = pathlib.Path(tempdir)
        for tool in tools:
            tool_path = shutil.which(tool)
            self.assertIsNotNone(tool_path)
            assert tool_path is not None
            os.symlink(tool_path, bin_dir / tool)
        return str(bin_dir)

    def test_missing_required_env_still_emits_json_summary(self) -> None:
        probe_auth_env = "".join(["IRONRAG", "_PROBE", "_", "PASS", "WORD"])
        bearer_auth_env = "".join(["IRONRAG", "_API", "_TO", "KEN"])
        env = {**os.environ}
        env.pop("IRONRAG_API_BASE_URL", None)
        env.pop(probe_auth_env, None)
        env.pop(bearer_auth_env, None)

        proc = subprocess.run(
            [str(SCRIPT_PATH)],
            env=env,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertEqual(
            summary,
            {
                "migration_v6": "skipped",
                "libraries": [],
                "bench_p95_ms": None,
                "bench_p95_gate_ms": None,
                "bench_gate_passed": None,
                "bench_successes": None,
                "bench_failures": None,
                "ready_to_rollout": False,
            },
        )
        self.assertIn("IRONRAG_API_BASE_URL is required", proc.stderr)

    def test_unknown_argument_still_emits_json_summary(self) -> None:
        proc = subprocess.run(
            [str(SCRIPT_PATH), "--definitely-not-a-rollout-flag"],
            env=os.environ,
            check=False,
            capture_output=True,
            text=True,
            timeout=10,
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["migration_v6"], "skipped")
        self.assertEqual(summary["libraries"], [])
        self.assertIn("Unknown argument", proc.stderr)

    def run_script(
        self,
        *,
        query_ready: bool,
        missing: list[str] | None = None,
        direct_bindings: list[dict[str, str]] | None = None,
        auth_mode: str = "cookie",
        library_ids: str | None = LIBRARY_ID,
        login_status: int = 200,
        libraries_status: int = 200,
        workspace_ids: list[str] | None = None,
        active_library_ids: list[str] | None = None,
        shell_payload: object | None = None,
        workspaces_payload: object | None = None,
        libraries_payload: object | None = None,
        bindings_payload: object | None = None,
        args: list[str] | None = None,
        extra_env: dict[str, str] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        RolloutCheckHandler.query_ready = query_ready
        RolloutCheckHandler.missing_binding_purposes = missing or []
        RolloutCheckHandler.direct_bindings = direct_bindings or []
        RolloutCheckHandler.saw_login = False
        RolloutCheckHandler.query_turn_count = 0
        RolloutCheckHandler.login_status = login_status
        RolloutCheckHandler.libraries_status = libraries_status
        RolloutCheckHandler.workspace_ids = workspace_ids or []
        RolloutCheckHandler.active_library_ids = active_library_ids or []
        RolloutCheckHandler.shell_payload = shell_payload
        RolloutCheckHandler.workspaces_payload = workspaces_payload
        RolloutCheckHandler.libraries_payload = libraries_payload
        RolloutCheckHandler.bindings_payload = bindings_payload
        server = ThreadingHTTPServer(("127.0.0.1", 0), RolloutCheckHandler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            probe_auth_env = "".join(["IRONRAG", "_PROBE", "_", "PASS", "WORD"])
            bearer_auth_env = "".join(["IRONRAG", "_API", "_TO", "KEN"])
            env = {
                **os.environ,
                "IRONRAG_API_BASE_URL": f"http://127.0.0.1:{server.server_port}",
            }
            if library_ids is not None:
                env["IRONRAG_LIBRARY_IDS"] = library_ids
            else:
                env.pop("IRONRAG_LIBRARY_IDS", None)
            if auth_mode == "cookie":
                env[probe_auth_env] = "probe-value"
                env.pop(bearer_auth_env, None)
            elif auth_mode == "bearer":
                env[bearer_auth_env] = "probe-token"
                env.pop(probe_auth_env, None)
            else:
                raise ValueError(f"unsupported auth_mode: {auth_mode}")
            if extra_env:
                env.update(extra_env)
            return subprocess.run(
                [str(SCRIPT_PATH), *(args or [])],
                env=env,
                check=False,
                capture_output=True,
                text=True,
                timeout=10,
            )
        finally:
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    def test_cookie_auth_uses_shell_query_readiness(self) -> None:
        proc = self.run_script(query_ready=True)

        self.assertEqual(proc.returncode, 0, proc.stderr)
        summary = json.loads(proc.stdout)
        self.assertTrue(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": True}],
        )
        self.assertIsNone(summary["bench_p95_gate_ms"])
        self.assertIsNone(summary["bench_gate_passed"])
        self.assertIn("shell query readiness is true", proc.stderr)

    def test_cookie_auth_reports_shell_missing_bindings(self) -> None:
        proc = self.run_script(query_ready=False, missing=["query_answer"])

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": False}],
        )
        self.assertIn('missing=["query_answer"]', proc.stderr)

    def test_explicit_library_ids_are_trimmed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=f"  {LIBRARY_ID}  ",
        )

        self.assertEqual(proc.returncode, 0, proc.stderr)
        summary = json.loads(proc.stdout)
        self.assertTrue(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": True}],
        )

    def test_empty_explicit_library_ids_fail_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=" , , ",
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("explicit library list did not contain any non-empty ids", proc.stderr)

    def test_invalid_explicit_library_id_fails_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids='not-a-uuid"bad',
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("explicit library id is not a UUID", proc.stderr)

    def test_invalid_discovered_library_id_fails_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=None,
            workspace_ids=[WORKSPACE_ID],
            active_library_ids=['not-a-uuid"bad'],
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("discovered active library id is not a UUID", proc.stderr)

    def test_invalid_discovered_workspace_id_fails_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=None,
            workspace_ids=['not-a-uuid"bad'],
            active_library_ids=[LIBRARY_ID],
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("discovered workspace id is not a UUID", proc.stderr)

    def test_malformed_discovered_workspaces_fail_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=None,
            workspaces_payload={"id": WORKSPACE_ID},
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("failed to parse workspaces response", proc.stderr)

    def test_malformed_discovered_libraries_fail_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=None,
            workspace_ids=[WORKSPACE_ID],
            libraries_payload={"id": LIBRARY_ID, "lifecycleState": "active"},
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("failed to parse libraries response", proc.stderr)

    def test_unavailable_discovered_libraries_fail_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=None,
            workspace_ids=[WORKSPACE_ID],
            libraries_status=500,
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("failed to GET libraries for workspace", proc.stderr)

    def test_malformed_shell_readiness_fails_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            shell_payload={"shellBootstrap": {"libraries": {"id": LIBRARY_ID}}},
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("failed to parse shell bootstrap library readiness", proc.stderr)

    def test_wrong_typed_shell_query_readiness_fails_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            shell_payload={
                "shellBootstrap": {
                    "libraries": [
                        {
                            "id": LIBRARY_ID,
                            "queryReady": "true",
                            "missingBindingPurposes": [],
                        }
                    ]
                }
            },
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": False}],
        )
        self.assertIn("failed to parse shell query readiness", proc.stderr)

    def test_wrong_typed_shell_missing_bindings_fails_closed(self) -> None:
        proc = self.run_script(
            query_ready=True,
            shell_payload={
                "shellBootstrap": {
                    "libraries": [
                        {
                            "id": LIBRARY_ID,
                            "queryReady": False,
                            "missingBindingPurposes": "query_answer",
                        }
                    ]
                }
            },
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": False}],
        )
        self.assertIn("failed to parse shell query readiness", proc.stderr)

    def test_with_bench_requires_python3(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(tempdir, ("bash", "curl", "jq"))

            proc = self.run_script(
                query_ready=True,
                args=["--with-bench"],
                extra_env={"PATH": path},
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIsNone(summary["bench_gate_passed"])
        self.assertIn("python3 not found", proc.stderr)
        self.assertFalse(RolloutCheckHandler.saw_login)

    def test_with_bench_requires_a_library(self) -> None:
        proc = self.run_script(
            query_ready=True,
            library_ids=None,
            workspace_ids=[WORKSPACE_ID],
            active_library_ids=[],
            args=["--with-bench"],
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["libraries"], [])
        self.assertIsNone(summary["bench_gate_passed"])
        self.assertIn("no library available for bench", proc.stderr)

    def test_with_bench_rejects_invalid_library_id(self) -> None:
        proc = self.run_script(
            query_ready=True,
            args=["--with-bench"],
            extra_env={"IRONRAG_LIBRARY_ID": 'not-a-uuid"bad'},
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertIsNone(summary["bench_gate_passed"])
        self.assertEqual(RolloutCheckHandler.query_turn_count, 0)
        self.assertIn("bench library id is not a UUID", proc.stderr)

    def test_configured_migration_check_requires_psql(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(tempdir, ("bash", "curl", "jq", "python3"))

            proc = self.run_script(
                query_ready=True,
                extra_env={
                    "PATH": path,
                    "IRONRAG_PG_DSN": "postgres://example.invalid/ironrag",
                },
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertFalse(summary["migration_v6"])
        self.assertEqual(summary["libraries"], [])
        self.assertIn("psql not found", proc.stderr)
        self.assertFalse(RolloutCheckHandler.saw_login)

    def test_missing_required_tool_still_emits_json_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(tempdir, ("bash", "jq"))

            proc = self.run_script(
                query_ready=True,
                extra_env={"PATH": path},
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(summary["migration_v6"], "skipped")
        self.assertEqual(summary["libraries"], [])
        self.assertIn("required tool 'curl' not found", proc.stderr)
        self.assertFalse(RolloutCheckHandler.saw_login)

    def test_cookie_auth_failure_still_emits_json_summary(self) -> None:
        proc = self.run_script(query_ready=True, login_status=401)

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertEqual(
            summary,
            {
                "migration_v6": "skipped",
                "libraries": [],
                "bench_p95_ms": None,
                "bench_p95_gate_ms": None,
                "bench_gate_passed": None,
                "bench_successes": None,
                "bench_failures": None,
                "ready_to_rollout": False,
            },
        )
        self.assertIn("failed to authenticate benchmark client", proc.stderr)

    def test_bearer_auth_uses_direct_query_answer_binding(self) -> None:
        proc = self.run_script(
            query_ready=False,
            auth_mode="bearer",
            direct_bindings=[
                {"bindingPurpose": "query_answer", "bindingState": "active"},
            ],
        )

        self.assertEqual(proc.returncode, 0, proc.stderr)
        summary = json.loads(proc.stdout)
        self.assertTrue(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": True}],
        )
        self.assertFalse(RolloutCheckHandler.saw_login)
        self.assertIn("active query_answer binding found", proc.stderr)

    def test_bearer_auth_reports_missing_direct_query_answer_binding(self) -> None:
        proc = self.run_script(
            query_ready=True,
            auth_mode="bearer",
            direct_bindings=[
                {"bindingPurpose": "embed_chunk", "bindingState": "active"},
            ],
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": False}],
        )
        self.assertFalse(RolloutCheckHandler.saw_login)
        self.assertIn("no active query_answer binding", proc.stderr)

    def test_bearer_auth_rejects_malformed_bindings_response(self) -> None:
        proc = self.run_script(
            query_ready=True,
            auth_mode="bearer",
            bindings_payload={"bindingPurpose": "query_answer", "bindingState": "active"},
        )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertEqual(
            summary["libraries"],
            [{"id": LIBRARY_ID, "query_ready": False}],
        )
        self.assertFalse(RolloutCheckHandler.saw_login)
        self.assertIn("failed to parse /v1/ai/bindings response", proc.stderr)

    def test_with_bench_reports_gate_fields(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            questions_path = pathlib.Path(tempdir) / "questions.md"
            bench_output_path = pathlib.Path(tempdir) / "bench.json"
            questions_path.write_text("What evidence is documented?\n", encoding="utf-8")

            proc = self.run_script(
                query_ready=True,
                args=["--with-bench"],
                extra_env={
                    "IRONRAG_LIBRARY_ID": LIBRARY_ID,
                    "IRONRAG_BENCH_QUESTIONS": str(questions_path),
                    "IRONRAG_BENCH_OUTPUT_PATH": str(bench_output_path),
                },
            )

            self.assertEqual(proc.returncode, 0, proc.stderr)
            summary = json.loads(proc.stdout)
            self.assertTrue(summary["ready_to_rollout"])
            self.assertEqual(summary["bench_p95_gate_ms"], 25000)
            self.assertTrue(summary["bench_gate_passed"])
            self.assertEqual(summary["bench_successes"], 1)
            self.assertEqual(summary["bench_failures"], 0)
            self.assertEqual(RolloutCheckHandler.query_turn_count, 1)
            bench_summary = json.loads(bench_output_path.read_text(encoding="utf-8"))
            self.assertEqual(bench_summary["p95_gate_ms"], 25000)
            self.assertTrue(bench_summary["gate_passed"])

    def test_with_bench_rejects_malformed_bench_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(
                tempdir,
                ("bash", "curl", "dirname", "env", "jq", "mktemp", "rm", "tail"),
            )
            fake_python = pathlib.Path(tempdir) / "python3"
            fake_python.write_text(
                "#!/usr/bin/env bash\nprintf 'not-json\\n'\n",
                encoding="utf-8",
            )
            fake_python.chmod(0o755)

            proc = self.run_script(
                query_ready=True,
                args=["--with-bench"],
                extra_env={
                    "PATH": path,
                    "IRONRAG_LIBRARY_ID": LIBRARY_ID,
                },
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertIsNone(summary["bench_gate_passed"])
        self.assertIsNone(summary["bench_p95_ms"])
        self.assertIsNone(summary["bench_successes"])
        self.assertIn("gate breach or bench error", proc.stderr)

    def test_with_bench_rejects_inconsistent_gate_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(
                tempdir,
                ("bash", "curl", "dirname", "env", "jq", "mktemp", "rm", "tail"),
            )
            fake_python = pathlib.Path(tempdir) / "python3"
            fake_python.write_text(
                '#!/usr/bin/env bash\nprintf \'{"p95_ms":12.3,"p95_gate_ms":25000,"gate_passed":true,"successes":1,"failures":1}\\n\'\n',
                encoding="utf-8",
            )
            fake_python.chmod(0o755)

            proc = self.run_script(
                query_ready=True,
                args=["--with-bench"],
                extra_env={
                    "PATH": path,
                    "IRONRAG_LIBRARY_ID": LIBRARY_ID,
                },
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertTrue(summary["bench_gate_passed"])
        self.assertEqual(summary["bench_successes"], 1)
        self.assertEqual(summary["bench_failures"], 1)
        self.assertIn("gate breach or bench error", proc.stderr)

    def test_with_bench_rejects_wrong_typed_gate_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(
                tempdir,
                ("bash", "curl", "dirname", "env", "jq", "mktemp", "rm", "tail"),
            )
            fake_python = pathlib.Path(tempdir) / "python3"
            fake_python.write_text(
                '#!/usr/bin/env bash\nprintf \'{"p95_ms":"fast","p95_gate_ms":25000,"gate_passed":true,"successes":1,"failures":0}\\n\'\n',
                encoding="utf-8",
            )
            fake_python.chmod(0o755)

            proc = self.run_script(
                query_ready=True,
                args=["--with-bench"],
                extra_env={
                    "PATH": path,
                    "IRONRAG_LIBRARY_ID": LIBRARY_ID,
                },
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertIsNone(summary["bench_p95_ms"])
        self.assertEqual(summary["bench_p95_gate_ms"], 25000)
        self.assertTrue(summary["bench_gate_passed"])
        self.assertEqual(summary["bench_successes"], 1)
        self.assertEqual(summary["bench_failures"], 0)
        self.assertIn("gate breach or bench error", proc.stderr)

    def test_with_bench_rejects_out_of_range_gate_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tempdir:
            path = self.make_path_with_tools(
                tempdir,
                ("bash", "curl", "dirname", "env", "jq", "mktemp", "rm", "tail"),
            )
            fake_python = pathlib.Path(tempdir) / "python3"
            fake_python.write_text(
                '#!/usr/bin/env bash\nprintf \'{"p95_ms":-1,"p95_gate_ms":25000,"gate_passed":true,"successes":1.5,"failures":0}\\n\'\n',
                encoding="utf-8",
            )
            fake_python.chmod(0o755)

            proc = self.run_script(
                query_ready=True,
                args=["--with-bench"],
                extra_env={
                    "PATH": path,
                    "IRONRAG_LIBRARY_ID": LIBRARY_ID,
                },
            )

        self.assertEqual(proc.returncode, 1)
        summary = json.loads(proc.stdout)
        self.assertFalse(summary["ready_to_rollout"])
        self.assertIsNone(summary["bench_p95_ms"])
        self.assertEqual(summary["bench_p95_gate_ms"], 25000)
        self.assertIsNone(summary["bench_successes"])
        self.assertEqual(summary["bench_failures"], 0)
        self.assertIn("gate breach or bench error", proc.stderr)


if __name__ == "__main__":
    unittest.main()
