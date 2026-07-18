import importlib.util
import json
import os
import pathlib
import sys
import tempfile
import unittest


SCRIPT_PATH = pathlib.Path(__file__).resolve().parent / "agent_turn_p95.py"
SPEC = importlib.util.spec_from_file_location("agent_turn_p95", SCRIPT_PATH)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)


class AgentTurnP95Tests(unittest.TestCase):
    def test_percentile_uses_conservative_nearest_rank(self) -> None:
        samples = [100.0, 200.0, 300.0, 400.0, 500.0]

        self.assertEqual(MODULE.percentile(samples, 50), 300.0)
        self.assertEqual(MODULE.percentile(samples, 95), 500.0)
        self.assertEqual(MODULE.percentile(samples, 99), 500.0)

    def test_validate_turn_quality_accepts_grounded_verified_answer(self) -> None:
        quality, error = MODULE.validate_turn_quality(
            {
                "responseTurn": {"contentText": "Use GET /system/info."},
                "verificationState": "verified",
                "chunkReferences": [{"chunkId": "chunk-1"}],
                "preparedSegmentReferences": [],
                "technicalFactReferences": [],
                "entityReferences": [],
                "relationReferences": [],
            },
            expected_verification="verified",
            min_references=1,
            min_answer_chars=1,
            require_all=["GET /system/info"],
            forbid_any=["/serverinfo"],
        )

        self.assertIsNone(error)
        self.assertEqual(quality.answer_chars, len("Use GET /system/info."))
        self.assertEqual(quality.verification_state, "verified")
        self.assertEqual(quality.reference_count, 1)
        self.assertEqual(quality.missing_required, ())
        self.assertEqual(quality.forbidden_found, ())

    def test_validate_turn_quality_rejects_empty_ungrounded_http_success(self) -> None:
        quality, error = MODULE.validate_turn_quality(
            {
                "responseTurn": {"contentText": "   "},
                "verificationState": "insufficient_evidence",
                "chunkReferences": [],
            },
            expected_verification="verified",
            min_references=1,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        self.assertEqual(quality.answer_chars, 0)
        self.assertEqual(quality.reference_count, 0)
        self.assertIsNotNone(error)
        assert error is not None
        self.assertIn("answer_chars=0<min=1", error)
        self.assertIn("verification='insufficient_evidence' expected='verified'", error)
        self.assertIn("references=0<min=1", error)

    def test_validate_turn_quality_can_leave_verifier_gate_disabled(self) -> None:
        quality, error = MODULE.validate_turn_quality(
            {
                "responseTurn": {"contentText": "Document list is available."},
                "verificationState": "insufficient_evidence",
                "chunkReferences": [{"chunkId": "chunk-1"}],
            },
            expected_verification=None,
            min_references=1,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        self.assertIsNone(error)
        self.assertEqual(quality.verification_state, "insufficient_evidence")

    def test_validate_turn_quality_rejects_missing_and_forbidden_artifacts(self) -> None:
        quality, error = MODULE.validate_turn_quality(
            {
                "responseTurn": {"contentText": "Use /serverinfo instead."},
                "verificationState": "verified",
                "chunkReferences": [{"chunkId": "chunk-1"}],
            },
            expected_verification="verified",
            min_references=1,
            min_answer_chars=1,
            require_all=["/system/info"],
            forbid_any=["/serverinfo"],
        )

        self.assertEqual(quality.missing_required, ("/system/info",))
        self.assertEqual(quality.forbidden_found, ("/serverinfo",))
        self.assertIsNotNone(error)
        assert error is not None
        self.assertIn("missing_required=['/system/info']", error)
        self.assertIn("forbidden_found=['/serverinfo']", error)

    def test_split_expectation_list_trims_empty_values(self) -> None:
        self.assertEqual(
            MODULE.split_expectation_list(" /system/info, ,GET "),
            ["/system/info", "GET"],
        )

    def test_parser_accepts_custom_p95_gate(self) -> None:
        args = MODULE.build_parser().parse_args(["--p95-gate-ms", "25000"])

        self.assertEqual(args.p95_gate_ms, 25000)

    def test_parser_accepts_output_path(self) -> None:
        args = MODULE.build_parser().parse_args(["--output-path", "/tmp/bench.json"])

        self.assertEqual(args.output_path, "/tmp/bench.json")

    def test_parser_rejects_negative_cli_gates(self) -> None:
        for flag in ("--min-references", "--min-answer-chars", "--p95-gate-ms"):
            with self.subTest(flag=flag):
                with self.assertRaises(SystemExit):
                    MODULE.build_parser().parse_args([flag, "-1"])

    def test_parser_rejects_non_positive_top_k(self) -> None:
        for value in ("0", "-1"):
            with self.subTest(value=value):
                with self.assertRaises(SystemExit):
                    MODULE.build_parser().parse_args(["--top-k", value])

    def test_parser_rejects_non_positive_question_limit(self) -> None:
        for value in ("0", "-1"):
            with self.subTest(value=value):
                with self.assertRaises(SystemExit):
                    MODULE.build_parser().parse_args(["--question-limit", value])

    def test_write_summary_json_persists_artifact(self) -> None:
        result = {"runs": 1, "successes": 1, "failures": 0, "p95_ms": 12.3}

        with tempfile.TemporaryDirectory() as tempdir:
            output_path = pathlib.Path(tempdir) / "nested" / "bench.json"
            summary_json = MODULE.write_summary_json(result, str(output_path))

            self.assertEqual(json.loads(summary_json), result)
            self.assertEqual(json.loads(output_path.read_text(encoding="utf-8")), result)

    def test_add_gate_summary_matches_exit_gate(self) -> None:
        passing = MODULE.add_gate_summary({"p95_ms": 12.3, "successes": 1, "failures": 0}, 100)
        failing_quality = MODULE.add_gate_summary(
            {"p95_ms": 12.3, "successes": 1, "failures": 1},
            100,
        )
        failing_latency = MODULE.add_gate_summary(
            {"p95_ms": 120.0, "successes": 1, "failures": 0},
            100,
        )
        failing_empty = MODULE.add_gate_summary(
            {"p95_ms": None, "successes": 0, "failures": 0},
            100,
        )
        failing_missing_counters = MODULE.add_gate_summary({"p95_ms": 12.3}, 100)
        failing_wrong_typed_counters = MODULE.add_gate_summary(
            {"p95_ms": 12.3, "successes": True, "failures": 0},
            100,
        )

        self.assertEqual(passing["p95_gate_ms"], 100)
        self.assertTrue(passing["gate_passed"])
        self.assertFalse(failing_quality["gate_passed"])
        self.assertFalse(failing_latency["gate_passed"])
        self.assertFalse(failing_empty["gate_passed"])
        self.assertFalse(failing_missing_counters["gate_passed"])
        self.assertFalse(failing_wrong_typed_counters["gate_passed"])

    def test_normalize_concurrency_keeps_executor_worker_count_valid(self) -> None:
        self.assertEqual(MODULE.normalize_concurrency(-5), 1)
        self.assertEqual(MODULE.normalize_concurrency(0), 1)
        self.assertEqual(MODULE.normalize_concurrency(3), 3)

    def test_load_questions_accepts_json_case_specs(self) -> None:
        payload = {
            "cases": [
                {
                    "id": "endpoint_case",
                    "question": "Which endpoint is documented?",
                    "assistantRequireAll": ["/system/info", "GET"],
                    "assistantForbidAny": ["/serverinfo"],
                    "assistantExpectedVerification": "verified",
                    "assistantMinReferences": 2,
                    "assistantMinAnswerChars": 20,
                }
            ]
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
            json.dump(payload, fh)
            fh.flush()

            questions = MODULE.load_questions(fh.name)

        self.assertEqual(len(questions), 1)
        self.assertEqual(questions[0].case_id, "endpoint_case")
        self.assertEqual(questions[0].question, "Which endpoint is documented?")
        self.assertEqual(questions[0].require_all, ("/system/info", "GET"))
        self.assertEqual(questions[0].forbid_any, ("/serverinfo",))
        self.assertEqual(questions[0].expected_verification, "verified")
        self.assertEqual(questions[0].min_references, 2)
        self.assertEqual(questions[0].min_answer_chars, 20)

    def test_load_questions_keeps_plain_markdown_compatibility(self) -> None:
        with tempfile.NamedTemporaryFile("w", suffix=".md", encoding="utf-8") as fh:
            fh.write("# skipped\n\nFirst question?\nSecond question?\n")
            fh.flush()

            questions = MODULE.load_questions(fh.name)

        self.assertEqual(
            [question.question for question in questions],
            ["First question?", "Second question?"],
        )
        self.assertEqual(questions[0].require_all, ())

    def test_load_questions_rejects_non_string_expectation_entries(self) -> None:
        payload = {
            "cases": [
                {
                    "question": "Which endpoint is documented?",
                    "assistantRequireAll": ["/system/info", 200],
                }
            ]
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
            json.dump(payload, fh)
            fh.flush()

            with self.assertRaisesRegex(ValueError, "assistantRequireAll entries"):
                MODULE.load_questions(fh.name)

    def test_load_questions_rejects_boolean_numeric_gates(self) -> None:
        payload = {
            "cases": [
                {
                    "question": "Which endpoint is documented?",
                    "assistantMinReferences": True,
                }
            ]
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
            json.dump(payload, fh)
            fh.flush()

            with self.assertRaisesRegex(ValueError, "assistantMinReferences must be an integer"):
                MODULE.load_questions(fh.name)

    def test_load_questions_rejects_negative_numeric_gates(self) -> None:
        payload = {
            "cases": [
                {
                    "question": "Which endpoint is documented?",
                    "assistantMinReferences": -1,
                }
            ]
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
            json.dump(payload, fh)
            fh.flush()

            with self.assertRaisesRegex(
                ValueError,
                "assistantMinReferences must be non-negative",
            ):
                MODULE.load_questions(fh.name)

    def test_read_expectation_values_uses_first_configured_value(self) -> None:
        case = {"requireAll": None, "assistantRequireAll": ["Alpha", " ", "Beta"]}

        values = MODULE.read_expectation_values(case, "requireAll", "assistantRequireAll")

        self.assertEqual(values, ["Alpha", "Beta"])

    def test_run_benchmark_uses_case_quality_overrides(self) -> None:
        original_run_turn = MODULE.run_turn
        captured_calls: list[tuple[object, ...]] = []

        def fake_run_turn(*args: object) -> tuple[float, object, None]:
            captured_calls.append(args)
            return 12.0, MODULE.TurnQuality(24, "verified", 2), None

        MODULE.run_turn = fake_run_turn
        try:
            result = MODULE.run_benchmark(
                base_url="http://127.0.0.1",
                auth_headers={},
                library_id="library-id",
                questions=[
                    MODULE.BenchmarkQuestion(
                        question="Question?",
                        expected_verification="verified",
                        min_references=2,
                        min_answer_chars=20,
                        require_all=("Alpha",),
                        forbid_any=("Beta",),
                    )
                ],
                concurrency=1,
                top_k=3,
                expected_verification=None,
                min_references=1,
                min_answer_chars=1,
                require_all=["Global"],
                forbid_any=["Forbidden"],
            )
        finally:
            MODULE.run_turn = original_run_turn

        self.assertEqual(result["successes"], 1)
        self.assertEqual(
            captured_calls,
            [
                (
                    "http://127.0.0.1",
                    {},
                    "library-id",
                    "Question?",
                    3,
                    "verified",
                    2,
                    20,
                    ["Global", "Alpha"],
                    ["Forbidden", "Beta"],
                )
            ],
        )

    def test_run_benchmark_records_empty_failures(self) -> None:
        original_run_turn = MODULE.run_turn

        def fake_run_turn(*_args: object) -> tuple[float, None, str]:
            return 10.0, None, "network failure"

        MODULE.run_turn = fake_run_turn
        try:
            result = MODULE.run_benchmark(
                base_url="http://127.0.0.1",
                auth_headers={},
                library_id="library-id",
                questions=[MODULE.BenchmarkQuestion(question="Question?")],
                concurrency=1,
                top_k=None,
                expected_verification=None,
                min_references=1,
                min_answer_chars=1,
                require_all=[],
                forbid_any=[],
            )
        finally:
            MODULE.run_turn = original_run_turn

        self.assertEqual(result["successes"], 0)
        self.assertEqual(result["failures"], 1)
        self.assertIsNone(result["p95_ms"])
        self.assertEqual(result["per_question"][0]["error"], "network failure")

    def test_run_benchmark_includes_case_id_in_question_result(self) -> None:
        original_run_turn = MODULE.run_turn

        def fake_run_turn(*args, **kwargs):
            return (
                12.0,
                MODULE.TurnQuality(
                    answer_chars=24,
                    verification_state="verified",
                    reference_count=2,
                ),
                None,
            )

        MODULE.run_turn = fake_run_turn
        try:
            result = MODULE.run_benchmark(
                base_url="http://127.0.0.1",
                auth_headers={},
                library_id="library-id",
                questions=[
                    MODULE.BenchmarkQuestion(
                        case_id="case-a",
                        question="Question A?",
                    )
                ],
                concurrency=1,
                top_k=None,
                expected_verification=None,
                min_references=1,
                min_answer_chars=1,
                require_all=[],
                forbid_any=[],
            )
        finally:
            MODULE.run_turn = original_run_turn

        self.assertEqual(result["per_question"][0]["case_id"], "case-a")

    def test_run_turn_reports_timeout_as_question_failure(self) -> None:
        original_post = MODULE._post

        def timeout_post(*args, **kwargs):
            raise TimeoutError("timed out")

        MODULE._post = timeout_post
        try:
            elapsed_ms, quality, error = MODULE.run_turn(
                "http://127.0.0.1:1",
                {},
                "library-id",
                "Question?",
                None,
                None,
                1,
                1,
                [],
                [],
            )
        finally:
            MODULE._post = original_post

        self.assertGreaterEqual(elapsed_ms, 0.0)
        self.assertIsNone(quality)
        self.assertEqual(error, "timed out")

    def test_main_validates_question_file_before_auth(self) -> None:
        original_resolve_auth_headers = MODULE.resolve_auth_headers
        original_argv = sys.argv[:]
        original_env = os.environ.copy()
        auth_calls: list[tuple[str, str]] = []

        def fake_resolve_auth_headers(base_url: str, login: str) -> dict[str, str]:
            auth_calls.append((base_url, login))
            raise RuntimeError("auth should not be reached")

        MODULE.resolve_auth_headers = fake_resolve_auth_headers
        try:
            with tempfile.TemporaryDirectory() as tempdir:
                missing_path = pathlib.Path(tempdir) / "missing.md"
                os.environ.clear()
                os.environ.update(
                    {
                        "IRONRAG_LIBRARY_ID": "00000000-0000-7000-8000-000000000001",
                        "IRONRAG_BENCH_QUESTIONS": str(missing_path),
                    }
                )
                sys.argv = ["agent_turn_p95.py"]

                exit_code = MODULE.main()
        finally:
            MODULE.resolve_auth_headers = original_resolve_auth_headers
            sys.argv = original_argv
            os.environ.clear()
            os.environ.update(original_env)

        self.assertEqual(exit_code, 1)
        self.assertEqual(auth_calls, [])

    def test_main_rejects_invalid_library_id_before_auth(self) -> None:
        original_resolve_auth_headers = MODULE.resolve_auth_headers
        original_argv = sys.argv[:]
        original_env = os.environ.copy()
        auth_calls: list[tuple[str, str]] = []

        def fake_resolve_auth_headers(base_url: str, login: str) -> dict[str, str]:
            auth_calls.append((base_url, login))
            raise RuntimeError("auth should not be reached")

        MODULE.resolve_auth_headers = fake_resolve_auth_headers
        try:
            with tempfile.NamedTemporaryFile("w", suffix=".md", encoding="utf-8") as fh:
                fh.write("Which endpoint is documented?\n")
                fh.flush()
                os.environ.clear()
                os.environ.update(
                    {
                        "IRONRAG_LIBRARY_ID": 'not-a-uuid"bad',
                        "IRONRAG_BENCH_QUESTIONS": fh.name,
                    }
                )
                sys.argv = ["agent_turn_p95.py"]

                exit_code = MODULE.main()
        finally:
            MODULE.resolve_auth_headers = original_resolve_auth_headers
            sys.argv = original_argv
            os.environ.clear()
            os.environ.update(original_env)

        self.assertEqual(exit_code, 1)
        self.assertEqual(auth_calls, [])


if __name__ == "__main__":
    unittest.main()
