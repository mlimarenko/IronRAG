import importlib.util
import io
import json
import pathlib
import stat
import sys
import tempfile
import unittest
from unittest import mock


SCRIPT_PATH = pathlib.Path(__file__).resolve().parent / "run-agent-surface-suite.py"
SPEC = importlib.util.spec_from_file_location("run_agent_surface_suite", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AgentSurfaceSuiteSecurityTests(unittest.TestCase):
    def test_suite_rejects_raw_mcp_token_cli_argument(self) -> None:
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

    def test_case_command_keeps_token_and_question_out_of_argv(self) -> None:
        token = "private-suite-token"
        question = "private suite question"

        command = MODULE.build_case_command(
            base_url="https://example.invalid",
            login="admin",
            library_id="library-1",
            workspace_id="workspace-1",
            case={"id": "case-1", "question": question},
            report_path=pathlib.Path("/secure/report.md"),
        )

        argv = "\0".join(command)
        self.assertNotIn(token, argv)
        self.assertNotIn(question, argv)
        self.assertNotIn("--mcp-token", command)
        self.assertNotIn("--question", command)

    def test_case_runner_passes_secrets_over_stdin_and_sanitizes_environment(self) -> None:
        token = "private-suite-token"
        password = "private-suite-password"
        question = "private suite question"
        with tempfile.TemporaryDirectory() as reports_dir:
            with mock.patch.object(MODULE.subprocess, "run") as run:
                run.return_value = mock.Mock(returncode=0, stdout="", stderr="")

                MODULE.run_case(
                    base_url="https://example.invalid",
                    login="admin",
                    library_id="library-1",
                    workspace_id=None,
                    mcp_token=token,
                    probe_password=password,
                    reports_dir=pathlib.Path(reports_dir),
                    case={"id": "case-1", "question": question},
                )

        command = run.call_args.args[0]
        environment = run.call_args.kwargs["env"]
        control_payload = json.loads(run.call_args.kwargs["input"])
        self.assertNotIn(token, "\0".join(command))
        self.assertNotIn(question, "\0".join(command))
        self.assertNotIn(MODULE.MCP_TOKEN_ENV, environment)
        self.assertNotIn(MODULE.PROBE_PASSWORD_ENV, environment)
        self.assertNotIn(MODULE.PROBE_QUESTION_ENV, environment)
        self.assertEqual(control_payload["mcpToken"], token)
        self.assertEqual(control_payload["password"], password)
        self.assertEqual(control_payload["question"], question)

    def test_case_runner_rejects_unsafe_case_id_before_spawning(self) -> None:
        with tempfile.TemporaryDirectory() as reports_dir:
            with mock.patch.object(MODULE.subprocess, "run") as run:
                with self.assertRaisesRegex(ValueError, "case id"):
                    MODULE.run_case(
                        base_url="https://example.invalid",
                        login="admin",
                        library_id="library-1",
                        workspace_id=None,
                        mcp_token="private-suite-token",
                        probe_password="private-suite-password",
                        reports_dir=pathlib.Path(reports_dir),
                        case={"id": "../../outside", "question": "private question"},
                    )

        run.assert_not_called()

    def test_case_report_path_stays_inside_reports_directory(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_root:
            reports_dir = pathlib.Path(temporary_root) / "reports"
            MODULE.ensure_private_directory(reports_dir)

            report_path = MODULE.case_report_path(reports_dir, "case-1.alpha")

            self.assertEqual(report_path.parent.resolve(), reports_dir.resolve())
            self.assertEqual(report_path.name, "case-1.alpha.md")

    def test_suite_private_writer_enforces_directory_and_file_modes(self) -> None:
        with tempfile.TemporaryDirectory() as temporary_root:
            reports_dir = pathlib.Path(temporary_root) / "reports"
            MODULE.ensure_private_directory(reports_dir)
            report_path = reports_dir / "suite-report.md"
            MODULE.write_private_text(report_path, "private suite report")

            self.assertEqual(stat.S_IMODE(reports_dir.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE(report_path.stat().st_mode), 0o600)
            self.assertEqual(report_path.read_text(encoding="utf-8"), "private suite report")


if __name__ == "__main__":
    unittest.main()
