import importlib.util
import os
import pathlib
import stat
import subprocess
import sys
import tempfile
import unittest


SCRIPT_DIR = pathlib.Path(__file__).resolve().parent
REPO_ROOT = SCRIPT_DIR.parents[1]
LIBRARY_ID = "00000000-0000-4000-8000-000000000001"
FIXTURE_MCP_TOKEN = "fixture-mcp-token"
FIXTURE_PROBE_PASSWORD = "fixture-probe-password"
LIBRARY_ID_ENV = "IRONRAG_AGENT_PROBE_LIBRARY_ID"
ARGV_FILE_ENV = "IRG_MAKE_ENTRY_ARGV_FILE"
TOKEN_FILE_ENV = "IRG_MAKE_ENTRY_TOKEN_FILE"


def load_script_module(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPT_DIR / filename)
    assert spec is not None
    assert spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


PROBE_MODULE = load_script_module("profile_agent_surfaces", "profile-agent-surfaces.py")
SUITE_MODULE = load_script_module("run_agent_surface_suite", "run-agent-surface-suite.py")

# Stands in for the interpreter the recipe invokes, so the assertions below
# exercise the arguments the make target really builds.
PYTHON_STUB = """#!/bin/sh
: > "$IRG_MAKE_ENTRY_ARGV_FILE"
for argument in "$@"; do
  printf '%s\\n' "$argument" >> "$IRG_MAKE_ENTRY_ARGV_FILE"
done
printf '%s' "${IRONRAG_MCP_TOKEN:-}" > "$IRG_MAKE_ENTRY_TOKEN_FILE"
"""


class AgentPerfMakeEntryTests(unittest.TestCase):
    def make_entry_arguments(self, target: str) -> tuple[list[str], str]:
        """Run one agent-perf target against a recording interpreter stub."""
        with tempfile.TemporaryDirectory() as tempdir:
            bin_dir = pathlib.Path(tempdir) / "bin"
            bin_dir.mkdir()
            stub_path = bin_dir / "python3"
            stub_path.write_text(PYTHON_STUB, encoding="utf-8")
            stub_path.chmod(
                stub_path.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH
            )
            argv_path = pathlib.Path(tempdir) / "argv"
            token_path = pathlib.Path(tempdir) / "token"

            env = {**os.environ}
            env["PATH"] = f"{bin_dir}{os.pathsep}{env.get('PATH', '')}"
            env[LIBRARY_ID_ENV] = LIBRARY_ID
            env[PROBE_MODULE.PROBE_PASSWORD_ENV] = FIXTURE_PROBE_PASSWORD
            env[PROBE_MODULE.MCP_TOKEN_ENV] = FIXTURE_MCP_TOKEN
            env[ARGV_FILE_ENV] = str(argv_path)
            env[TOKEN_FILE_ENV] = str(token_path)

            proc = subprocess.run(
                ["make", target],
                cwd=REPO_ROOT,
                env=env,
                check=False,
                capture_output=True,
                text=True,
                timeout=120,
            )

            self.assertEqual(proc.returncode, 0, proc.stderr[-2000:])
            self.assertTrue(token_path.exists(), "the recipe did not reach the CLI")
            arguments = argv_path.read_text(encoding="utf-8").splitlines()
            return arguments, token_path.read_text(encoding="utf-8")

    def assert_target_reaches_cli_with_environment_token(
        self, target: str, script_name: str, parser
    ) -> None:
        arguments, environment_token = self.make_entry_arguments(target)
        self.assertEqual(arguments[0], f"scripts/ops/{script_name}")

        # The canonical channel is the process environment: the credential
        # must still arrive, and it must never travel as an argument.
        self.assertEqual(environment_token, FIXTURE_MCP_TOKEN)
        self.assertNotIn(FIXTURE_MCP_TOKEN, arguments)
        self.assertNotIn(FIXTURE_PROBE_PASSWORD, arguments)
        self.assertNotIn("--mcp-token", arguments)

        # The entry point has to hand the real parser an argv it accepts.
        parsed = parser(arguments[1:])
        self.assertEqual(parsed.library_id, LIBRARY_ID)

    def test_probe_target_argv_is_accepted_by_the_cli(self) -> None:
        self.assert_target_reaches_cli_with_environment_token(
            "agent-perf-probe",
            "profile-agent-surfaces.py",
            PROBE_MODULE.parse_args,
        )

    def test_suite_target_argv_is_accepted_by_the_cli(self) -> None:
        self.assert_target_reaches_cli_with_environment_token(
            "agent-perf-suite",
            "run-agent-surface-suite.py",
            SUITE_MODULE.parse_args,
        )


if __name__ == "__main__":
    unittest.main()
