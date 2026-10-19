import inspect
import io
import itertools
import json
import os
import pathlib
import sys
import tempfile
import threading
import unittest

from apps.api.benchmarks.benchmark_test_support import (
    canonical_execution_detail,
    load_benchmark_module,
)


MODULE = load_benchmark_module("agent_turn_p95", "scripts/bench/agent_turn_p95.py")


def canonical_turn(
    answer: str, verification: str, *, with_chunk: bool, disposition: str = "factual_ready"
) -> dict:
    return canonical_execution_detail(
        answer, verification, with_chunk=with_chunk, answer_disposition=disposition
    )


def turn_execution(
    *,
    total_ms: float = 12.0,
    quality=None,
    failure_category: str | None = None,
    error: str | None = None,
):
    return MODULE.TurnExecution(
        total_ms=total_ms,
        session_create_ms=2.0,
        turn_ms=total_ms - 2.0,
        quality=quality,
        failure_category=failure_category,
        error=error,
        response_status=200 if error is None else None,
        request_id="request-1" if error is None else None,
        trace_id="trace-1" if error is None else None,
        execution_id="execution-1" if error is None else None,
    )


def runtime_version_fixture(*, query_turn_max_concurrent: int = 4) -> dict:
    runtime_resources = {"cpuCores": 4, "memoryBytes": 8_000_000_000}
    return {
        "service": "ironrag-api",
        "version": "1.2.3",
        "environment": "test",
        "role": "api",
        "deploymentId": "deployment-1",
        "binarySha256": "a" * 64,
        "resolvedConfigSha256": "b" * 64,
        "runtimeResources": runtime_resources,
        "runtimeResourcesSha256": MODULE.stable_json_sha256(runtime_resources),
        "redisEndpointSha256": "c" * 64,
        "redisServiceSha256": "d" * 64,
        "databaseTlsLeafSha256": "e" * 64,
        "redisTlsLeafSha256": "f" * 64,
        "queryTurnMaxConcurrent": query_turn_max_concurrent,
    }


def data_state_identity_fixture(
    *, role: str = "baseline", library_id: str = "library-id"
) -> dict:
    return {
        "role": role,
        "libraryId": library_id,
        "databaseIdentity": {"deploymentId": "deployment-1"},
    }


def experiment_args_fixture(*, output_path: str, artifact_role: str = "baseline"):
    return MODULE.build_parser().parse_args(
        [
            "--experiment-id", "experiment-1",
            "--artifact-role", artifact_role,
            "--pair-id", "pair-1",
            "--round-id", "round-1",
            "--sequence-position", "A",
            "--repetitions", "1",
            "--data-state-file", "collector.json",
            "--workspace-id", "00000000-0000-4000-8000-000000000007",
            "--top-k", "8",
            "--output-path", output_path,
        ]
    )


def experiment_result_fixture(*, target_concurrency: int = 100) -> dict:
    return {
        "attempted": 3,
        "successful": 3,
        "failed": 0,
        "duration_seconds": 1.0,
        "duration_ns": 1_000_000_000,
        "started_at": "2026-08-26T00:00:00+00:00",
        "completed_at": "2026-08-26T00:00:01+00:00",
        "attempted_throughput": 3.0,
        "successful_throughput": 3.0,
        "target_concurrency": target_concurrency,
        "observed_max_concurrency": target_concurrency,
        "sustained_concurrency_ratio": 1.0,
        "latency": {
            "sample_count": 3,
            "p50_ms": 10.0,
            "p95_ms": 20.0,
            "p99_ms": 30.0,
        },
        "concurrency_events": [{"offset_ns": 0, "offset_ms": 0.0, "active": 0}],
        "samples": [{"sample_id": "case-1#1", "elapsed_ms": 10.0}],
    }


def stubbed_sustained_result(
    *,
    concurrency: int,
    repetitions: int,
    runtime_query_turn_max_concurrent: int | None = None,
) -> dict:
    questions = [
        MODULE.BenchmarkQuestion(
            case_id=f"case-{index}", question=f"Question {index}?"
        )
        for index in range(concurrency)
    ]
    schedule, _, _ = MODULE.build_expanded_schedule(questions, repetitions, "pair-1")
    execution_barrier = threading.Barrier(concurrency)
    original_run_question = MODULE._run_question
    original_post = MODULE._post
    session_counter = itertools.count()

    def fake_run_question(spec, *_args, **_kwargs):
        execution_barrier.wait(timeout=2)
        return spec, turn_execution(
            quality=MODULE.TurnQuality(24, "verified", "factual_ready", 2)
        )

    runner_kwargs = {}
    if runtime_query_turn_max_concurrent is not None:
        runner_kwargs["runtime_query_turn_max_concurrent"] = (
            runtime_query_turn_max_concurrent
        )
    MODULE._run_question = fake_run_question
    MODULE._post = lambda *_args, **_kwargs: MODULE.HttpJsonResponse(
        payload={"id": f"session-{next(session_counter)}"},
        status=201,
        request_id="setup-request",
        trace_id="setup-trace",
    )
    try:
        return MODULE.run_sustained_benchmark(
            base_url="http://127.0.0.1",
            auth_headers={},
            library_id="library-id",
            schedule=schedule,
            concurrency=concurrency,
            expectations=MODULE.TurnExpectations(
                top_k=8,
                expected_verification="verified",
                min_references=1,
                min_answer_chars=1,
                require_all=[],
                forbid_any=[],
            ),
            **runner_kwargs,
        )
    finally:
        MODULE._run_question = original_run_question
        MODULE._post = original_post


class AgentTurnP95Tests(unittest.TestCase):
    def test_entry_points_stay_within_sonar_parameter_budget(self) -> None:
        # python:S107 authorizes at most 13 parameters per function; a wider
        # signature fails the quality gate and blocks every release job.
        for name in (
            "run_sustained_benchmark",
            "run_benchmark",
            "run_turn",
            "_run_question",
        ):
            with self.subTest(entry_point=name):
                parameters = inspect.signature(getattr(MODULE, name)).parameters
                self.assertLessEqual(
                    len(parameters),
                    13,
                    f"python:S107: {name} has {len(parameters)} parameters",
                )

    def test_api_url_keeps_exact_single_v1_prefix(self) -> None:
        self.assertEqual(
            MODULE.api_url("https://ironrag.invalid/v1", "/v1/version"),
            "https://ironrag.invalid/v1/version",
        )

    def test_configured_http_timeout_is_positive_and_bounded(self) -> None:
        original = os.environ.get("IRONRAG_HTTP_TIMEOUT")
        try:
            os.environ["IRONRAG_HTTP_TIMEOUT"] = "17"
            self.assertEqual(MODULE._resolve_http_timeout_seconds(180), 17)
            for invalid in ("0", "-1", "not-a-number"):
                os.environ["IRONRAG_HTTP_TIMEOUT"] = invalid
                self.assertEqual(MODULE._resolve_http_timeout_seconds(180), 180)
        finally:
            if original is None:
                os.environ.pop("IRONRAG_HTTP_TIMEOUT", None)
            else:
                os.environ["IRONRAG_HTTP_TIMEOUT"] = original
        self.assertEqual(
            MODULE.api_url(
                "https://ironrag.invalid",
                "/v1/query/libraries/00000000-0000-4000-8000-000000000008/sessions",
            ),
            "https://ironrag.invalid/v1/query/libraries/"
            "00000000-0000-4000-8000-000000000008/sessions",
        )

    def test_percentile_uses_conservative_nearest_rank(self) -> None:
        samples = [100.0, 200.0, 300.0, 400.0, 500.0]

        self.assertEqual(MODULE.percentile(samples, 50), 300.0)
        self.assertEqual(MODULE.percentile(samples, 95), 500.0)
        self.assertEqual(MODULE.percentile(samples, 99), 500.0)

    def test_validate_turn_quality_accepts_grounded_verified_answer(self) -> None:
        quality, error = MODULE.validate_turn_quality(
            canonical_turn("Use GET /system/info.", "verified", with_chunk=True),
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
            canonical_turn("   ", "insufficient_evidence", with_chunk=False),
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
            canonical_turn(
                "Document list is available.", "insufficient_evidence", with_chunk=True
            ),
            expected_verification=None,
            min_references=1,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        self.assertIsNone(error)
        self.assertEqual(quality.verification_state, "insufficient_evidence")

    def test_validate_turn_quality_rejects_non_terminal_disposition(self) -> None:
        # answerDisposition, not the raw verificationState, is the policy's
        # terminal-readiness contract (verification_policy.rs). A turn whose
        # disposition is still non_terminal did not produce a usable answer,
        # regardless of what verificationState says.
        quality, error = MODULE.validate_turn_quality(
            canonical_turn(
                "Candidate body.",
                "insufficient_evidence",
                with_chunk=True,
                disposition="non_terminal",
            ),
            expected_verification=None,
            min_references=1,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        self.assertEqual(quality.disposition, "non_terminal")
        self.assertIsNotNone(error)
        assert error is not None
        self.assertIn("disposition='non_terminal' is non_terminal", error)

    def test_validate_turn_quality_accepts_not_run_ordinary_prose_as_terminal(self) -> None:
        # semantic_verification_not_run: ordinary prose has no typed semantic
        # verification result. verification_policy.rs marks this
        # FactualReady under Moderate/Lenient levels, so verificationState
        # "not_run" alone must not fail the gate.
        quality, error = MODULE.validate_turn_quality(
            canonical_turn(
                "Grounded prose explanation.",
                "not_run",
                with_chunk=True,
                disposition="factual_ready",
            ),
            expected_verification=None,
            min_references=1,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        self.assertEqual(quality.verification_state, "not_run")
        self.assertEqual(quality.disposition, "factual_ready")
        self.assertIsNone(error)

    def test_validate_turn_quality_accepts_strict_safe_fallback_as_terminal(self) -> None:
        # Strict level replaces an unverified candidate with a safe-fallback
        # body: SafeFallback is a deliberate terminal disposition, not a
        # gate failure.
        quality, error = MODULE.validate_turn_quality(
            canonical_turn(
                "Safe fallback response.",
                "insufficient_evidence",
                with_chunk=False,
                disposition="safe_fallback",
            ),
            expected_verification=None,
            min_references=0,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        self.assertEqual(quality.disposition, "safe_fallback")
        self.assertIsNone(error)

    def test_validate_turn_quality_rejects_noncanonical_execution_and_relation_fields(self) -> None:
        execution_alias = canonical_turn("answer", "verified", with_chunk=False)
        execution_alias["execution"]["execution_state"] = execution_alias["execution"].pop(
            "lifecycleState"
        )
        with self.assertRaisesRegex(ValueError, "canonical execution.lifecycleState"):
            MODULE.validate_turn_quality(
                execution_alias,
                expected_verification=None,
                min_references=0,
                min_answer_chars=1,
                require_all=[],
                forbid_any=[],
            )

        relation_alias = canonical_turn("answer", "verified", with_chunk=False)
        relation_alias["relationReferences"] = [
            {
                "executionId": "execution-1",
                "edgeId": "edge-1",
                "rank": 1,
                "score": 1.0,
                "predicate": "relates",
                "normalized_assertion": "A relates to B",
            }
        ]
        with self.assertRaisesRegex(ValueError, "noncanonical normalized_assertion"):
            MODULE.validate_turn_quality(
                relation_alias,
                expected_verification=None,
                min_references=1,
                min_answer_chars=1,
                require_all=[],
                forbid_any=[],
            )

    def test_validate_turn_quality_rejects_structurally_invalid_reference(self) -> None:
        payload = canonical_turn("answer", "verified", with_chunk=True)
        payload["chunkReferences"][0]["chunkId"] = None

        with self.assertRaisesRegex(ValueError, "chunkReferences.chunkId must be a UUID"):
            MODULE.validate_turn_quality(
                payload,
                expected_verification=None,
                min_references=1,
                min_answer_chars=1,
                require_all=[],
                forbid_any=[],
            )

    def test_benchmark_turn_is_bound_to_question_library_and_workspace(self) -> None:
        payload = canonical_execution_detail(
            "answer",
            "verified",
            question="submitted question",
            library_id="00000000-0000-4000-8000-000000000008",
            workspace_id="00000000-0000-4000-8000-000000000007",
        )
        common = {
            "expected_verification": None,
            "min_references": 0,
            "min_answer_chars": 1,
            "require_all": [],
            "forbid_any": [],
        }
        with self.assertRaisesRegex(ValueError, "submitted question"):
            MODULE.validate_turn_quality(
                payload,
                question="another question",
                library_id=payload["execution"]["libraryId"],
                workspace_id=payload["execution"]["workspaceId"],
                **common,
            )
        with self.assertRaisesRegex(ValueError, "selected library"):
            MODULE.validate_turn_quality(
                payload,
                question="submitted question",
                library_id="10000000-0000-4000-8000-000000000008",
                workspace_id=payload["execution"]["workspaceId"],
                **common,
            )
        with self.assertRaisesRegex(ValueError, "selected workspace"):
            MODULE.validate_turn_quality(
                payload,
                question="submitted question",
                library_id=payload["execution"]["libraryId"],
                workspace_id="10000000-0000-4000-8000-000000000007",
                **common,
            )

    def test_validate_turn_quality_rejects_missing_and_forbidden_artifacts(self) -> None:
        quality, error = MODULE.validate_turn_quality(
            canonical_turn("Use /serverinfo instead.", "verified", with_chunk=True),
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
        parser = MODULE.build_parser()
        for flag in ("--min-references", "--min-answer-chars", "--p95-gate-ms"):
            with self.subTest(flag=flag):
                with self.assertRaises(SystemExit):
                    parser.parse_args([flag, "-1"])

    def test_parser_rejects_non_positive_top_k(self) -> None:
        parser = MODULE.build_parser()
        for value in ("0", "-1"):
            with self.subTest(value=value):
                with self.assertRaises(SystemExit):
                    parser.parse_args(["--top-k", value])

    def test_parser_rejects_non_positive_question_limit(self) -> None:
        parser = MODULE.build_parser()
        for value in ("0", "-1"):
            with self.subTest(value=value):
                with self.assertRaises(SystemExit):
                    parser.parse_args(["--question-limit", value])

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

    def test_expanded_schedule_is_deterministic_and_rejects_ambiguous_cases(self) -> None:
        questions = [
            MODULE.BenchmarkQuestion(case_id="a", question="Question A?"),
            MODULE.BenchmarkQuestion(case_id="b", question="Question B?"),
        ]
        first, first_questions, first_schedule = MODULE.build_expanded_schedule(
            questions, 2, "pair-1"
        )
        second, second_questions, second_schedule = MODULE.build_expanded_schedule(
            questions, 2, "pair-1"
        )

        self.assertEqual([item.sample_id for item in first], [item.sample_id for item in second])
        self.assertEqual(first_questions, second_questions)
        self.assertEqual(first_schedule, second_schedule)
        self.assertEqual(len(first), 4)

        with self.assertRaisesRegex(ValueError, "unique"):
            MODULE.build_expanded_schedule([questions[0], questions[0]], 1, "pair-1")

    def test_sustained_runner_reaches_real_concurrency_and_records_raw_samples(self) -> None:
        result = stubbed_sustained_result(concurrency=4, repetitions=2)

        self.assertEqual(result["attempted"], 8)
        self.assertEqual(result["successful"], 8)
        self.assertEqual(result["failed"], 0)
        self.assertEqual(result["observed_max_concurrency"], 4)
        self.assertEqual(len({sample["sample_id"] for sample in result["samples"]}), 8)
        self.assertTrue(all("question" in sample for sample in result["samples"]))
        self.assertTrue(all(sample["trace_id"] == "trace-1" for sample in result["samples"]))

    def test_sustained_runner_records_runtime_ceiling_in_samples(self) -> None:
        result = stubbed_sustained_result(
            concurrency=4, repetitions=2, runtime_query_turn_max_concurrent=12
        )

        self.assertEqual(len(result["samples"]), 8)
        self.assertTrue(
            all(
                sample["queryTurnMaxConcurrent"] == 12 for sample in result["samples"]
            )
        )

    def test_sustained_runner_records_absent_runtime_ceiling_as_null(self) -> None:
        result = stubbed_sustained_result(concurrency=4, repetitions=2)

        self.assertEqual(len(result["samples"]), 8)
        self.assertTrue(
            all(
                sample["queryTurnMaxConcurrent"] is None for sample in result["samples"]
            )
        )

    def test_sustained_runner_rejects_fake_n_larger_than_schedule(self) -> None:
        schedule, _, _ = MODULE.build_expanded_schedule(
            [MODULE.BenchmarkQuestion(case_id="case", question="Question?")],
            1,
            "pair-1",
        )

        expectations = MODULE.TurnExpectations(
            top_k=8,
            expected_verification=None,
            min_references=1,
            min_answer_chars=1,
            require_all=[],
            forbid_any=[],
        )

        with self.assertRaisesRegex(ValueError, "at least requested concurrency"):
            MODULE.run_sustained_benchmark(
                base_url="http://127.0.0.1",
                auth_headers={},
                library_id="library-id",
                schedule=schedule,
                concurrency=100,
                expectations=expectations,
            )

    def test_load_questions_accepts_json_cases_envelope(self) -> None:
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

    def test_default_question_fixture_loads_twenty_cases(self) -> None:
        """The shipped corpus is a real 20-case file, not a placeholder.

        The default `scripts/bench/grounded-queries.json` is referenced by the
        CLI default and by the retained agent-turn benchmark; a regression that
        empties or truncates it must fail here rather than at run time.
        """
        fixture_path = MODULE.REPOSITORY_ROOT / MODULE.DEFAULT_QUESTIONS_PATH
        self.assertTrue(fixture_path.is_file(), f"missing {fixture_path}")

        questions = MODULE.load_questions(str(fixture_path))

        self.assertEqual(len(questions), 20)
        self.assertEqual(
            len({question.case_id for question in questions}), 20
        )
        for question in questions:
            self.assertTrue(question.case_id)
            self.assertTrue(question.question)

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

    def test_load_questions_rejects_legacy_fields(self) -> None:
        payload = {
            "cases": [
                {
                    "question": "Which endpoint is documented?",
                    "requireAll": ["/system/info"],
                }
            ]
        }
        with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
            json.dump(payload, fh)
            fh.flush()

            with self.assertRaisesRegex(ValueError, "unknown fields: requireAll"):
                MODULE.load_questions(fh.name)

    def test_load_questions_rejects_non_json_input(self) -> None:
        with tempfile.NamedTemporaryFile("w", suffix=".md", encoding="utf-8") as fh:
            fh.write("First question?")
            fh.flush()

            with self.assertRaisesRegex(ValueError, "must be a JSON file"):
                MODULE.load_questions(fh.name)

    def test_load_questions_rejects_non_envelope_json(self) -> None:
        with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
            json.dump([{"question": "Which endpoint is documented?"}], fh)
            fh.flush()

            with self.assertRaisesRegex(ValueError, "must be an object with a cases list"):
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

    def test_read_expectation_values_parses_defined_field(self) -> None:
        case = {"assistantRequireAll": ["Alpha", " ", "Beta"]}

        values = MODULE.read_expectation_values(case, "assistantRequireAll")

        self.assertEqual(values, ["Alpha", "Beta"])

    def test_run_benchmark_uses_case_quality_overrides(self) -> None:
        original_run_turn = MODULE.run_turn
        captured_calls: list[tuple[object, ...]] = []

        def fake_run_turn(*args: object):
            captured_calls.append(args)
            return turn_execution(
                quality=MODULE.TurnQuality(24, "verified", "factual_ready", 2)
            )

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
                expectations=MODULE.TurnExpectations(
                    top_k=3,
                    expected_verification=None,
                    min_references=1,
                    min_answer_chars=1,
                    require_all=["Global"],
                    forbid_any=["Forbidden"],
                ),
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
                    MODULE.TurnExpectations(
                        top_k=3,
                        expected_verification="verified",
                        min_references=2,
                        min_answer_chars=20,
                        require_all=["Global", "Alpha"],
                        forbid_any=["Forbidden", "Beta"],
                    ),
                )
            ],
        )

    def test_run_benchmark_records_empty_failures(self) -> None:
        original_run_turn = MODULE.run_turn

        def fake_run_turn(*_args: object):
            return turn_execution(
                total_ms=10.0,
                failure_category="transport",
                error="network failure",
            )

        MODULE.run_turn = fake_run_turn
        try:
            result = MODULE.run_benchmark(
                base_url="http://127.0.0.1",
                auth_headers={},
                library_id="library-id",
                questions=[MODULE.BenchmarkQuestion(question="Question?")],
                concurrency=1,
                expectations=MODULE.TurnExpectations(
                    top_k=None,
                    expected_verification=None,
                    min_references=1,
                    min_answer_chars=1,
                    require_all=[],
                    forbid_any=[],
                ),
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
            return turn_execution(
                quality=MODULE.TurnQuality(
                    answer_chars=24,
                    verification_state="verified",
                    disposition="factual_ready",
                    reference_count=2,
                ),
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
                expectations=MODULE.TurnExpectations(
                    top_k=None,
                    expected_verification=None,
                    min_references=1,
                    min_answer_chars=1,
                    require_all=[],
                    forbid_any=[],
                ),
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
            execution = MODULE.run_turn(
                "http://127.0.0.1:1",
                {},
                "library-id",
                "Question?",
                MODULE.TurnExpectations(
                    top_k=None,
                    expected_verification=None,
                    min_references=1,
                    min_answer_chars=1,
                    require_all=[],
                    forbid_any=[],
                ),
            )
        finally:
            MODULE._post = original_post

        self.assertGreaterEqual(execution.total_ms, 0.0)
        self.assertIsNone(execution.quality)
        self.assertEqual(execution.failure_category, "timeout")
        self.assertEqual(execution.error, "timed out")

    def test_session_creation_posts_to_library_scoped_route(self) -> None:
        library_id = "00000000-0000-4000-8000-000000000008"
        canonical_session_path = f"/v1/query/libraries/{library_id}/sessions"
        recorded_posts: list[tuple[str, dict]] = []
        original_post = MODULE._post

        def recording_post(base_url, path, auth_headers, body):
            recorded_posts.append((path, body))
            if path.endswith("/sessions"):
                return MODULE.HttpJsonResponse(
                    payload={"id": "session-1"},
                    status=201,
                    request_id="create-request",
                    trace_id="create-trace",
                )
            if path.endswith("/turns"):
                return MODULE.HttpJsonResponse(
                    payload=canonical_execution_detail(
                        "Grounded answer.",
                        "verified",
                        with_chunk=True,
                        question="Question?",
                    ),
                    status=200,
                    request_id="turn-request",
                    trace_id="turn-trace",
                )
            raise AssertionError(f"unexpected request path: {path}")

        schedule, _, _ = MODULE.build_expanded_schedule(
            [MODULE.BenchmarkQuestion(case_id="case-1", question="Question?")],
            1,
            "session-route-seed",
        )

        MODULE._post = recording_post
        try:
            MODULE.run_turn(
                "http://127.0.0.1",
                {},
                library_id,
                "Question?",
                MODULE.TurnExpectations(
                    top_k=None,
                    expected_verification=None,
                    min_references=1,
                    min_answer_chars=1,
                    require_all=[],
                    forbid_any=[],
                ),
            )
            MODULE.precreated_schedule(
                base_url="http://127.0.0.1",
                auth_headers={},
                library_id=library_id,
                schedule=schedule,
            )
        finally:
            MODULE._post = original_post

        session_paths = [
            path for path, _ in recorded_posts if path.endswith("/sessions")
        ]
        self.assertEqual(
            session_paths, [canonical_session_path, canonical_session_path]
        )
        self.assertNotIn("/v1/query/sessions", session_paths)
        self.assertTrue(
            all(body == {} for path, body in recorded_posts if path.endswith("/sessions"))
        )

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
            with tempfile.NamedTemporaryFile("w", suffix=".json", encoding="utf-8") as fh:
                json.dump({"cases": [{"question": "Which endpoint is documented?"}]}, fh)
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

    def test_binding_error_still_rejects_other_identity_mismatches(self) -> None:
        role_error = MODULE.experiment_binding_error(
            runtime_version=runtime_version_fixture(),
            data_state_identity=data_state_identity_fixture(role="candidate"),
            library_id="library-id",
            artifact_role="baseline",
        )
        self.assertEqual(role_error, "data-state role does not match --artifact-role")

        stale_runtime = runtime_version_fixture()
        stale_runtime["runtimeResourcesSha256"] = "0" * 64
        stale_error = MODULE.experiment_binding_error(
            runtime_version=stale_runtime,
            data_state_identity=data_state_identity_fixture(),
            library_id="library-id",
            artifact_role="baseline",
        )
        self.assertEqual(stale_error, "runtime resource attestation is stale")

    def test_experiment_artifact_records_runtime_ceiling(self) -> None:
        artifact = MODULE.build_experiment_artifact(
            args=experiment_args_fixture(output_path="/unused/artifact.json"),
            questions_path=MODULE.REPOSITORY_ROOT
            / "scripts"
            / "bench"
            / "grounded-queries.json",
            question_set_sha256="0" * 64,
            schedule_sha256="1" * 64,
            result=experiment_result_fixture(),
            data_state_identity=data_state_identity_fixture(),
            runtime_version=runtime_version_fixture(query_turn_max_concurrent=4),
        )

        self.assertEqual(artifact["runtimeIdentity"]["queryTurnMaxConcurrent"], 4)

    def test_experiment_artifact_records_absent_runtime_ceiling_as_null(self) -> None:
        runtime_version = runtime_version_fixture()
        runtime_version.pop("queryTurnMaxConcurrent")

        artifact = MODULE.build_experiment_artifact(
            args=experiment_args_fixture(output_path="/unused/artifact.json"),
            questions_path=MODULE.REPOSITORY_ROOT
            / "scripts"
            / "bench"
            / "grounded-queries.json",
            question_set_sha256="0" * 64,
            schedule_sha256="1" * 64,
            result=experiment_result_fixture(),
            data_state_identity=data_state_identity_fixture(),
            runtime_version=runtime_version,
        )

        self.assertIsNone(artifact["runtimeIdentity"]["queryTurnMaxConcurrent"])

    def test_experiment_mode_starts_when_runtime_ceiling_is_below_client_concurrency(
        self,
    ) -> None:
        runtime_version = runtime_version_fixture(query_turn_max_concurrent=4)
        data_state_identity = data_state_identity_fixture()
        captured_calls: list[dict] = []
        original_load = MODULE.load_collector_document
        original_get = MODULE._get
        original_runner = MODULE.run_sustained_benchmark
        stderr = io.StringIO()
        original_stderr = sys.stderr

        def fake_runner(**kwargs):
            captured_calls.append(kwargs)
            return experiment_result_fixture()

        MODULE.load_collector_document = lambda _path: (
            {"dataStateIdentity": data_state_identity},
            data_state_identity,
        )
        MODULE._get = lambda _base_url, _path, _headers: MODULE.HttpJsonResponse(
            payload=runtime_version,
            status=200,
            request_id="version-request",
            trace_id="version-trace",
        )
        MODULE.run_sustained_benchmark = fake_runner
        try:
            with tempfile.TemporaryDirectory() as tempdir:
                output_path = pathlib.Path(tempdir) / "artifact.json"
                sys.stderr = stderr
                try:
                    exit_code = MODULE.run_experiment_mode(
                        args=experiment_args_fixture(output_path=str(output_path)),
                        base_url="http://127.0.0.1",
                        auth_headers={},
                        library_id="library-id",
                        questions=[
                            MODULE.BenchmarkQuestion(
                                case_id="case-1", question="Question?"
                            )
                        ],
                        questions_path=str(
                            MODULE.REPOSITORY_ROOT
                            / "scripts"
                            / "bench"
                            / "grounded-queries.json"
                        ),
                        concurrency=100,
                    )
                finally:
                    sys.stderr = original_stderr
                artifact = (
                    json.loads(output_path.read_text(encoding="utf-8"))
                    if output_path.exists()
                    else None
                )
        finally:
            MODULE.load_collector_document = original_load
            MODULE._get = original_get
            MODULE.run_sustained_benchmark = original_runner

        self.assertEqual(exit_code, 0, stderr.getvalue().strip())
        self.assertEqual(len(captured_calls), 1)
        self.assertEqual(captured_calls[0]["concurrency"], 100)
        self.assertEqual(captured_calls[0]["runtime_query_turn_max_concurrent"], 4)
        self.assertNotIn("below requested concurrency", stderr.getvalue())
        self.assertIsNotNone(artifact)
        self.assertEqual(artifact["runtimeIdentity"]["queryTurnMaxConcurrent"], 4)


if __name__ == "__main__":
    unittest.main()
