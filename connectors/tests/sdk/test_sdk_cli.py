"""The dispatcher that replaced five per-connector entry points."""

from __future__ import annotations

import asyncio
import json
from datetime import UTC, datetime
from types import SimpleNamespace
from uuid import UUID

import pytest

from ironrag_connector.cli import _admission_json, _parse, _run_one, main
from ironrag_connector.ironrag import (
    IngestAttempt,
    IngestJob,
    IngestJobExecution,
    IngestReadiness,
)
from ironrag_connector.policy import PushPolicy
from ironrag_connector.registry import (
    SourcePlugin,
    UnknownSourceError,
    available_sources,
    load_source,
)
from ironrag_connector.routing import PolicyOverrides


def test_every_shipped_source_resolves_to_a_complete_plugin() -> None:
    names = available_sources()
    assert names == ("bookstack", "confluence", "echo", "gitrepos", "youtrack")
    for name in names:
        plugin = load_source(name)
        assert isinstance(plugin, SourcePlugin)
        assert plugin.name == name
        # A registration that resolves but cannot build anything is the failure
        # mode a string-keyed registry hides until runtime.
        assert callable(plugin.adapter_factory)
        assert plugin.settings_cls is not None


def test_unknown_source_names_the_ones_that_exist() -> None:
    with pytest.raises(UnknownSourceError) as excinfo:
        load_source("sharepoint")
    message = str(excinfo.value)
    assert "sharepoint" in message
    for name in available_sources():
        assert name in message


def test_list_prints_every_source(capsys: pytest.CaptureFixture[str]) -> None:
    main(["--list"])
    printed = capsys.readouterr().out.split()
    assert printed == list(available_sources())


def test_no_source_is_an_error_not_a_default() -> None:
    with pytest.raises(SystemExit) as excinfo:
        main([])
    assert "source" in str(excinfo.value)


def test_cli_validates_only_the_selected_source(monkeypatch: pytest.MonkeyPatch) -> None:
    """Settings are built after dispatch, and only for the chosen source.

    Every source reads the same unprefixed RUN_MODE, and YouTrack rejects
    anything but `poll`. If the CLI validated all five, a BookStack deployment
    running in webhook mode could not start at all.
    """
    for variable, value in (
        ("IRONRAG_BASE_URL", "http://ironrag.invalid"),
        ("IRONRAG_API_TOKEN", "token"),
        ("ADMIN_BEARER_TOKEN", "admin"),
        ("RUN_MODE", "webhook"),
        ("BOOKSTACK_BASE_URL", "http://bookstack.invalid"),
        ("BOOKSTACK_TOKEN_ID", "id"),
        ("BOOKSTACK_TOKEN_SECRET", "secret"),
        ("YOUTRACK_BASE_URL", "http://youtrack.invalid"),
        ("YOUTRACK_TOKEN", "token"),
    ):
        monkeypatch.setenv(variable, value)

    bookstack = load_source("bookstack")
    settings = bookstack.settings_cls()
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    assert bookstack.adapter_factory(settings, policies) is not None

    youtrack = load_source("youtrack")
    with pytest.raises(ValueError, match="poll"):
        youtrack.settings_cls()


def test_parse_accepts_a_source_argument() -> None:
    assert _parse(["bookstack"]).source == "bookstack"
    assert _parse(["--list"]).list is True


def test_enrichment_scope_requires_once_and_exact_item() -> None:
    with pytest.raises(SystemExit, match="--enrichment-scope requires --once"):
        main(["youtrack", "--enrichment-scope", "text_and_embedding"])
    with pytest.raises(SystemExit, match="--once requires source youtrack"):
        main(["youtrack", "--once", "--enrichment-scope", "text_and_embedding"])


def test_enrichment_scope_parses_only_known_literals() -> None:
    parsed = _parse(
        [
            "youtrack",
            "--once",
            "--kind",
            "issue",
            "--item-id",
            "3-3269",
            "--enrichment-scope",
            "text_and_embedding",
        ]
    )
    assert parsed.enrichment_scope == "text_and_embedding"
    unset = _parse(["youtrack", "--once", "--kind", "issue", "--item-id", "3-3269"])
    assert unset.enrichment_scope is None
    with pytest.raises(SystemExit):
        _parse(["youtrack", "--once", "--enrichment-scope", "video"])


def test_cli_admission_output_uses_real_identifiers() -> None:
    from uuid import UUID, uuid4

    from ironrag_connector.ironrag import OperationHandle

    handle = OperationHandle(
        operation_id=uuid4(),
        document_id=uuid4(),
        visibility_intent_id=uuid4(),
        mutation_item_id=uuid4(),
        mutation_id=uuid4(),
        job_id=uuid4(),
        result_revision_id=uuid4(),
        result_checksum="sha256:abc",
        request_fingerprint="opaque",
    )
    payload = _admission_json(handle)
    assert payload is not None
    assert UUID(payload["mutationId"]) == handle.mutation_id
    assert UUID(payload["jobId"]) == handle.job_id
    assert UUID(payload["asyncOperationId"]) == handle.operation_id
    assert payload["requestFingerprint"] == "opaque"
    assert _admission_json(None) is None


EXECUTE_JOB = UUID("00000000-0000-0000-0000-0000000000c7")
EXECUTE_DOC = UUID("00000000-0000-0000-0000-0000000000d1")
EXECUTE_REV = UUID("00000000-0000-0000-0000-0000000000d2")
EXECUTE_ATTEMPT = UUID("00000000-0000-0000-0000-0000000000f2")
EXECUTE_AT = datetime(2026, 11, 17, 20, 38, 24, tzinfo=UTC)


def test_execute_ingest_job_refuses_a_non_uuid(monkeypatch: pytest.MonkeyPatch) -> None:
    def no_plugin(_name: str) -> None:
        raise AssertionError("an invalid job id must not resolve a source plugin")

    monkeypatch.setattr("ironrag_connector.cli.load_source", no_plugin)
    with pytest.raises(SystemExit) as excinfo:
        main(["echo", "--execute-ingest-job", "not-a-uuid"])
    assert "job UUID" in str(excinfo.value)


def test_execute_ingest_job_requires_the_executing_source() -> None:
    with pytest.raises(SystemExit) as excinfo:
        main(["--execute-ingest-job", str(EXECUTE_JOB)])
    assert "configured credential" in str(excinfo.value)


def test_execute_ingest_job_refuses_source_selection_flags() -> None:
    with pytest.raises(SystemExit) as excinfo:
        main(
            [
                "youtrack",
                "--once",
                "--kind",
                "issue",
                "--item-id",
                "IRG-1",
                "--execute-ingest-job",
                str(EXECUTE_JOB),
            ]
        )
    assert "cannot be combined" in str(excinfo.value)


def test_execute_ingest_job_runs_the_exact_id_once(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    executed: list[UUID] = []
    settings = object()
    job = IngestJob(
        id=EXECUTE_JOB,
        workspace_id=EXECUTE_JOB,
        library_id=EXECUTE_JOB,
        job_kind="content_mutation",
        queue_state="completed",
        priority=100,
        knowledge_document_id=EXECUTE_DOC,
        knowledge_revision_id=EXECUTE_REV,
        queued_at=EXECUTE_AT,
        available_at=EXECUTE_AT,
        completed_at=EXECUTE_AT,
    )
    readiness = IngestReadiness(
        knowledge_document_id=EXECUTE_DOC,
        knowledge_revision_id=EXECUTE_REV,
        text_state="ready",
        vector_state="ready",
        graph_state="ready",
        text_ready=True,
        vector_ready=True,
        graph_ready=True,
    )
    execution = IngestJobExecution(
        job=job,
        readiness=readiness,
        latest_attempt=IngestAttempt(
            id=EXECUTE_ATTEMPT,
            job_id=EXECUTE_JOB,
            attempt_number=1,
            attempt_state="succeeded",
            current_stage="publish",
            progress_percent=100,
            retryable=False,
        ),
    )

    class StubClient:
        def __init__(self, _settings: object) -> None: ...

        async def __aenter__(self) -> StubClient:
            return self

        async def __aexit__(self, *_: object) -> None:
            return None

        async def execute_ingest_queue_job(self, job_id: UUID) -> IngestJobExecution:
            executed.append(job_id)
            return execution

    monkeypatch.setattr(
        "ironrag_connector.cli.load_source",
        lambda _name: SimpleNamespace(settings_cls=lambda: settings),
    )
    monkeypatch.setattr("ironrag_connector.cli.IronRagClient", StubClient)

    main(["echo", "--execute-ingest-job", str(EXECUTE_JOB)])

    assert executed == [EXECUTE_JOB]
    payload = json.loads(capsys.readouterr().out)
    # The printed record is the executed job the server returned, not a queue
    # snapshot and not a publication claim.
    assert payload["jobId"] == str(EXECUTE_JOB)
    assert payload["action"] == "requested"
    assert payload["queueState"] == "completed"
    assert payload["completedAt"] == EXECUTE_AT.isoformat()
    assert payload["readiness"] == {
        "text": "ready",
        "vector": "ready",
        "graph": "ready",
        "textReady": True,
        "vectorReady": True,
        "graphReady": True,
    }
    assert payload["latestAttempt"] == {
        "attemptNumber": 1,
        "attemptState": "succeeded",
        "currentStage": "publish",
        "failureClass": None,
        "failureCode": None,
        "failureMessage": None,
        "retryable": False,
    }


def test_one_item_run_refuses_a_source_without_item_observation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Only an adapter that can observe a single item may serve ``--once``."""

    class Adapter:
        async def close(self) -> None:
            return None

    class IronRag:
        async def aclose(self) -> None:
            return None

    class Pidfile:
        def acquire(self) -> None:
            return None

        def release(self) -> None:
            return None

    components = SimpleNamespace(
        adapter=Adapter(),
        ironrag=IronRag(),
        router=SimpleNamespace(),
        pidfile=Pidfile(),
        state=SimpleNamespace(close=lambda: None),
        orchestrator=SimpleNamespace(),
    )
    monkeypatch.setattr("ironrag_connector.cli.build_components", lambda *_: components)

    coroutine = _run_one(
        object(),  # type: ignore[arg-type]
        lambda *_: components.adapter,
        "page",
        "1",
    )
    with pytest.raises(ValueError, match="only supported by YouTrack"):
        asyncio.run(coroutine)


def test_kind_and_item_flags_require_once() -> None:
    with pytest.raises(SystemExit, match="require --once"):
        main(["youtrack", "--kind", "issue"])


def test_once_mode_prints_the_operation_json(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    """``--once`` streams framework chatter to stderr and the result to stdout."""

    seen: list[tuple[object, str, str, str | None]] = []

    async def run_one(
        settings: object,
        adapter_factory: object,
        kind: str,
        item_id: str,
        *,
        enrichment_scope: str | None = None,
    ) -> dict[str, object]:
        seen.append((settings, kind, item_id, enrichment_scope))
        return {"action": "created", "kind": kind, "item_id": item_id}

    plugin = SimpleNamespace(
        settings_cls=lambda: "settings",
        adapter_factory=lambda *_: "adapter",
        webhook_factory=None,
    )
    monkeypatch.setattr("ironrag_connector.cli.load_source", lambda _name: plugin)
    monkeypatch.setattr("ironrag_connector.cli._run_one", run_one)

    main(
        [
            "youtrack",
            "--once",
            "--kind",
            "issue",
            "--item-id",
            "3-3269",
            "--enrichment-scope",
            "text_and_embedding",
        ]
    )

    assert seen == [("settings", "issue", "3-3269", "text_and_embedding")]
    assert json.loads(capsys.readouterr().out) == {
        "action": "created",
        "kind": "issue",
        "item_id": "3-3269",
    }


def test_server_mode_builds_the_plugins_webhook_handlers(monkeypatch: pytest.MonkeyPatch) -> None:
    """The plugin's handler factory is bound to its settings before serving."""

    built: list[object] = []
    served: list[object] = []
    adapter = object()
    orchestrator = object()
    handler = object()

    def make_handler(settings: object, bound_adapter: object, bound_orchestrator: object) -> object:
        assert bound_adapter is adapter
        assert bound_orchestrator is orchestrator
        return handler

    plugin = SimpleNamespace(
        settings_cls=lambda: SimpleNamespace(host="0.0.0.0", port=8080, log_level="INFO"),
        adapter_factory=lambda *_: adapter,
        webhook_factory=make_handler,
    )
    monkeypatch.setattr("ironrag_connector.cli.load_source", lambda _name: plugin)
    monkeypatch.setattr(
        "ironrag_connector.cli.build_app",
        lambda settings, factory, *, webhook_factory=None: built.append(webhook_factory) or "app",
    )
    monkeypatch.setattr(
        "ironrag_connector.cli.uvicorn.run", lambda app, **kwargs: served.append((app, kwargs))
    )

    main(["youtrack"])

    assert len(built) == 1
    factory = built[0]
    assert callable(factory)
    assert factory(adapter, orchestrator) == [handler]
    assert served
    assert served[0][0] == "app"
