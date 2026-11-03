from __future__ import annotations

import contextlib
import io
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import run_live_benchmark as benchmark  # noqa: E402


class BenchmarkEntrypointTests(unittest.TestCase):
    def test_make_and_cli_select_every_registered_suite(self) -> None:
        result = subprocess.run(
            [
                "make", "--no-print-directory", "--eval",
                'entrypoint-registry:;@printf "%s\\n" "$(IRONRAG_GOLDEN_SUITES)"',
                "entrypoint-registry",
            ],
            cwd=benchmark.REPOSITORY_ROOT,
            check=True,
            capture_output=True,
            text=True,
            timeout=15,
        )
        make_paths = [benchmark.REPOSITORY_ROOT / item for item in result.stdout.split()]
        cli_paths = benchmark.default_suite_paths()
        registry = sorted(Path(benchmark.__file__).parent.glob("golden_*_suite.json"))
        self.assertTrue(registry)
        self.assertEqual(cli_paths, registry)
        self.assertEqual(make_paths, registry)
        for path in cli_paths:
            documents, cases, _ = benchmark.load_suite(path)
            self.assertTrue(documents, path.name)
            self.assertTrue(cases, path.name)

    def test_registry_discovery_is_stable_and_includes_added_suites(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name in ("golden_z_suite.json", "golden_a_suite.json", "rank_relevance.json"):
                (root / name).write_text("{}", encoding="utf-8")
            with mock.patch.object(benchmark, "__file__", str(root / "run_live_benchmark.py")):
                self.assertEqual(
                    [path.name for path in benchmark.default_suite_paths()],
                    ["golden_a_suite.json", "golden_z_suite.json"],
                )
                (root / "golden_b_suite.json").write_text("{}", encoding="utf-8")
                self.assertEqual(
                    [path.name for path in benchmark.default_suite_paths()],
                    ["golden_a_suite.json", "golden_b_suite.json", "golden_z_suite.json"],
                )

    def test_empty_registry_is_refused(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(benchmark, "__file__", str(Path(directory) / "runner.py")):
                with self.assertRaises(ValueError):
                    benchmark.default_suite_paths()

    def test_explicit_suite_does_not_expand_to_registry(self) -> None:
        args = benchmark.parse_args([
            "--base-url", "https://api.invalid/v1",
            "--tls-certificate-sha256", "1" * 64,
            "--workspace-id", "workspace",
            "--suite", "selected_suite.json",
        ])
        with (
            mock.patch.object(benchmark, "parse_args", return_value=args),
            mock.patch.object(benchmark, "resolve_session_cookie", return_value="session"),
            mock.patch.object(benchmark, "capture_host_activity_sample", return_value=({}, {})),
            mock.patch.object(benchmark, "evaluate_host_snapshot", return_value={"passed": True}),
            mock.patch.object(benchmark, "default_suite_paths") as registry,
            mock.patch.object(benchmark, "load_suite", side_effect=RuntimeError("selected")) as load,
            mock.patch.object(benchmark, "BenchmarkClient") as client,
        ):
            with self.assertRaisesRegex(RuntimeError, "selected"):
                benchmark.main()
            registry.assert_not_called()
            load.assert_called_once_with(Path("selected_suite.json").resolve())
            client.assert_not_called()

    def test_missing_target_is_refused_before_network(self) -> None:
        with (
            mock.patch.dict(os.environ, {}, clear=True),
            mock.patch.object(sys, "argv", ["run_live_benchmark.py"]),
            mock.patch.object(benchmark, "BenchmarkClient") as client,
            contextlib.redirect_stderr(io.StringIO()),
        ):
            self.assertEqual(benchmark.main(), 2)
            client.assert_not_called()

    def test_make_missing_target_refuses_before_creating_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "not-created"
            result = subprocess.run(
                [
                    "make", "--no-print-directory", "benchmark-grounded",
                    "IRONRAG_BENCHMARK_BASE_URL=",
                    "IRONRAG_BENCHMARK_WORKSPACE_ID=workspace",
                    f"IRONRAG_BENCHMARK_OUTPUT_DIR={output}",
                ],
                cwd=benchmark.REPOSITORY_ROOT,
                env={**os.environ, "IRONRAG_SESSION_COOKIE": "test-only-session"},
                capture_output=True,
                timeout=15,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
