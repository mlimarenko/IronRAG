"""`ironrag-connector <source>` — the one entry point every image runs.

The source is an argument rather than a script name because PEP 621 cannot
bake an argument into a console script: five scripts would mean five wrapper
functions, which is the duplication this distribution exists to remove.

Order matters. The plugin is resolved *first*, and only the resolved plugin's
settings class is instantiated. Every source reads the same unprefixed
`RUN_MODE`, and YouTrack rejects `webhook`/`both` outright — validating all
five would make one source's environment reject another source's start-up.
The adapter itself is built inside :func:`build_app`, after routing has been
loaded, because the plugin factory receives the live per-kind policy table.

`--execute-ingest-job <UUID>` is the second phase of the same two-phase
contract: it runs exactly one already-admitted, queued job with this source's
configured credential and exits. It selects no source item and starts no
server, so it lives on the same entry point rather than in a second binary.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import sys
from collections.abc import Sequence
from contextlib import AsyncExitStack, redirect_stdout
from uuid import UUID

import uvicorn

from .config import BaseConnectorSettings
from .ironrag import IngestJobExecution, IronRagClient, OperationHandle
from .orchestrator import OrchestrationOutcome, Orchestrator
from .registry import available_sources, load_source
from .routing import RoutingError
from .server import AdapterFactory, WebhookHandler, build_app, build_components
from .source import SourceAdapter


def _parse(argv: Sequence[str] | None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        prog="ironrag-connector",
        description=(
            "Run one IronRAG source connector, or execute one already-admitted "
            "ingest job with the configured connector credential."
        ),
    )
    parser.add_argument(
        "source",
        nargs="?",
        help=f"source to run; one of: {', '.join(available_sources())}",
    )
    parser.add_argument(
        "--list",
        action="store_true",
        help="print the sources this build ships and exit",
    )
    parser.add_argument("--once", action="store_true", help="observe and process one source item")
    parser.add_argument("--kind", choices=("issue", "article"))
    parser.add_argument("--item-id", help="canonical source item ID")
    parser.add_argument(
        "--enrichment-scope",
        choices=("full", "text_and_embedding"),
        help=(
            "explicit admission plan for this exact one-item run; unset sends no field "
            "and keeps the server default (full)"
        ),
    )
    parser.add_argument(
        "--execute-ingest-job",
        metavar="JOB_ID",
        help=(
            "execute exactly one already-admitted, queued ingest job by UUID and exit, "
            "using this source's configured IronRAG credential"
        ),
    )
    return parser.parse_args(argv)


def _outcome_json(outcome: OrchestrationOutcome) -> dict[str, object]:
    return {
        "action": outcome.action,
        "kind": outcome.ref.kind,
        "item_id": outcome.ref.item_id,
        "external_key": outcome.ref.external_key,
        "workspace_id": str(outcome.workspace_id) if outcome.workspace_id else None,
        "library_id": str(outcome.library_id) if outcome.library_id else None,
        "rule_description": outcome.rule_description,
        "ironrag_document_id": outcome.ironrag_document_id,
        "detail": outcome.detail,
        "deferred": outcome.deferred,
        "exclusion": outcome.exclusion.value,
        "admission": _admission_json(outcome.admission),
        "dependent_outcomes": [_outcome_json(child) for child in outcome.dependent_outcomes],
    }


def _admission_json(admission: OperationHandle | None) -> dict[str, str | None] | None:
    if admission is None:
        return None
    return {
        "mutationId": str(admission.mutation_id) if admission.mutation_id else None,
        "jobId": str(admission.job_id) if admission.job_id else None,
        "documentId": str(admission.document_id) if admission.document_id else None,
        "resultRevisionId": (
            str(admission.result_revision_id) if admission.result_revision_id else None
        ),
        "asyncOperationId": str(admission.operation_id),
        "requestFingerprint": admission.request_fingerprint,
        "visibilityIntentId": (
            str(admission.visibility_intent_id) if admission.visibility_intent_id else None
        ),
    }


async def _run_one(
    settings: BaseConnectorSettings,
    adapter_factory: AdapterFactory,
    kind: str,
    item_id: str,
    *,
    enrichment_scope: str | None = None,
) -> dict[str, object]:
    from .sources.youtrack.adapter import ItemObservationSkipped, YouTrackAdapter

    components = build_components(settings, adapter_factory)
    async with AsyncExitStack() as resources:
        resources.callback(components.pidfile.release)
        resources.callback(components.state.close)
        resources.push_async_callback(components.adapter.close)
        resources.push_async_callback(components.ironrag.aclose)
        components.pidfile.acquire()
        if not isinstance(components.adapter, YouTrackAdapter):
            raise ValueError("one-item observation is only supported by YouTrack")
        await components.router.initialize(components.ironrag.resolve_library_refs)
        observed = await components.adapter.observe_item(kind, item_id)
        if isinstance(observed, ItemObservationSkipped):
            return {
                "action": "skipped",
                "kind": kind,
                "item_id": item_id,
                "detail": observed.reason,
            }
        try:
            components.router.resolve(observed)
        except RoutingError as exc:
            return {
                "action": "skipped",
                "kind": kind,
                "item_id": item_id,
                "detail": f"route unavailable: {exc}",
            }
        outcome = await components.orchestrator.push_ref(
            observed, enrichment_scope=enrichment_scope
        )
        return _outcome_json(outcome)


async def _execute_ingest_job(settings: BaseConnectorSettings, job_id: UUID) -> IngestJobExecution:
    async with IronRagClient(settings) as client:
        return await client.execute_ingest_queue_job(job_id)


def _execution_json(execution: IngestJobExecution) -> dict[str, object]:
    """The executed job's own record: no queue, no other job, no claim."""
    job = execution.job
    readiness = execution.readiness
    attempt = execution.latest_attempt
    return {
        "jobId": str(job.id),
        "action": "requested",
        "queueState": job.queue_state,
        "jobKind": job.job_kind,
        "knowledgeDocumentId": (
            str(job.knowledge_document_id) if job.knowledge_document_id else None
        ),
        "knowledgeRevisionId": (
            str(job.knowledge_revision_id) if job.knowledge_revision_id else None
        ),
        "completedAt": job.completed_at.isoformat() if job.completed_at else None,
        "readiness": {
            "text": readiness.text_state,
            "vector": readiness.vector_state,
            "graph": readiness.graph_state,
            "textReady": readiness.text_ready,
            "vectorReady": readiness.vector_ready,
            "graphReady": readiness.graph_ready,
        },
        "latestAttempt": (
            {
                "attemptNumber": attempt.attempt_number,
                "attemptState": attempt.attempt_state,
                "currentStage": attempt.current_stage,
                "failureClass": attempt.failure_class,
                "failureCode": attempt.failure_code,
                "failureMessage": attempt.failure_message,
                "retryable": attempt.retryable,
            }
            if attempt is not None
            else None
        ),
    }


def _run_execute_ingest_job(args: argparse.Namespace, raw_job_id: str) -> None:
    if args.once or args.list or args.kind or args.item_id or args.enrichment_scope:
        raise SystemExit(
            "--execute-ingest-job cannot be combined with --once, --list, --kind, "
            "--item-id or --enrichment-scope"
        )
    if not args.source:
        raise SystemExit(
            "--execute-ingest-job needs the source whose configured credential executes the job"
        )
    try:
        job_id = UUID(raw_job_id)
    except ValueError as exc:
        raise SystemExit(f"--execute-ingest-job needs one job UUID, got {raw_job_id!r}") from exc
    plugin = load_source(args.source)
    settings = plugin.settings_cls()
    with redirect_stdout(sys.stderr):
        execution = asyncio.run(_execute_ingest_job(settings, job_id))
    print(json.dumps(_execution_json(execution)))


def _validate_selection(args: argparse.Namespace) -> None:
    if args.once and (args.source != "youtrack" or not args.kind or not args.item_id or args.list):
        raise SystemExit("--once requires source youtrack, --kind and --item-id")
    if args.enrichment_scope and not args.once:
        raise SystemExit("--enrichment-scope requires --once and an exact item selector")
    if not args.once and (args.kind or args.item_id):
        raise SystemExit("--kind and --item-id require --once")


def main(argv: Sequence[str] | None = None) -> None:
    args = _parse(argv)
    if args.execute_ingest_job is not None:
        _run_execute_ingest_job(args, args.execute_ingest_job)
        return
    _validate_selection(args)
    if args.list:
        for name in available_sources():
            print(name)
        return
    if not args.source:
        raise SystemExit(
            "ironrag-connector needs a source; one of: "
            f"{', '.join(available_sources())} (or --list)"
        )

    plugin = load_source(args.source)
    settings = plugin.settings_cls()
    if args.once:
        assert args.kind is not None and args.item_id is not None
        with redirect_stdout(sys.stderr):
            result = asyncio.run(
                _run_one(
                    settings,
                    plugin.adapter_factory,
                    args.kind,
                    args.item_id,
                    enrichment_scope=args.enrichment_scope,
                )
            )
        print(json.dumps(result, ensure_ascii=False))
        return

    make_handler = plugin.webhook_factory
    webhook_factory = None
    if make_handler is not None:

        def webhook_factory(
            adapter: SourceAdapter, orchestrator: Orchestrator
        ) -> list[WebhookHandler]:
            return [make_handler(settings, adapter, orchestrator)]

    app = build_app(settings, plugin.adapter_factory, webhook_factory=webhook_factory)
    uvicorn.run(
        app,
        host=settings.host,
        port=settings.port,
        log_level=settings.log_level,
    )
