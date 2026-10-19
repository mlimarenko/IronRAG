from __future__ import annotations

import asyncio
from pathlib import Path
from typing import Any, ClassVar
from uuid import UUID

import httpx
import pytest
from sdk_dependent_fixtures import DependentPageAdapter, DependentSpec, echo_page, seed_document
from test_sdk_orchestrator import FakeIronRag

from ironrag_connector.config import BaseConnectorSettings, RunMode
from ironrag_connector.ironrag import IronRagCatalogError, IronRagError
from ironrag_connector.routing import PolicyOverrides, ResolvedLibraryTarget, RoutingError
from ironrag_connector.server import build_app
from ironrag_connector.source import SourceFetchDeferred, SourceFetchResult, SourceItemRef
from ironrag_connector.sources.bookstack.config import BookStackSettings
from ironrag_connector.sources.bookstack.webhook import make_bookstack_handler
from ironrag_connector.state import MemberTarget, StateStore
from ironrag_connector.sync import SyncManager, SyncReport

WS = UUID("00000000-0000-0000-0000-000000000099")
LIB = UUID("00000000-0000-0000-0000-000000000000")
LIBRARY_REF = "tests/default-library"


class EmptyAdapter:
    name = "server-test"
    kinds = ("page",)
    primary_kinds = ("page",)

    def __init__(self) -> None:
        self.closed = False
        self.policies: PolicyOverrides | None = None

    def factory(self, settings: BaseConnectorSettings, policies: PolicyOverrides) -> EmptyAdapter:
        """What the registry hands `build_app`: settings and the live policy table."""
        self.policies = policies
        return self

    async def iter_items(self) -> Any:
        if False:
            yield

    async def fetch(self, ref: Any) -> SourceFetchResult:
        return SourceFetchDeferred("server lifecycle test does not materialize items")

    def external_key(self, kind: str, item_id: str) -> str:
        return f"server-test:{kind}:{item_id}"

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return None

    async def close(self) -> None:
        self.closed = True


class FakeIronRagClient:
    fail_resolution = False
    instances: ClassVar[list[FakeIronRagClient]] = []

    def __init__(self, *_: Any, **__: Any) -> None:
        self.closed = False
        self.resolution_calls: list[set[str]] = []
        self.instances.append(self)

    async def resolve_library_refs(self, refs: set[str]) -> dict[str, ResolvedLibraryTarget]:
        self.resolution_calls.append(refs)
        if self.fail_resolution:
            raise IronRagCatalogError("catalog target is not visible")
        return {
            LIBRARY_REF: ResolvedLibraryTarget(
                library_ref=LIBRARY_REF,
                workspace_id=WS,
                library_id=LIB,
            )
        }

    async def aclose(self) -> None:
        self.closed = True


def _settings(tmp_path: Path) -> BaseConnectorSettings:
    routing_path = tmp_path / "routing.yaml"
    routing_path.write_text(f"default: {{ library: {LIBRARY_REF} }}\n", encoding="utf-8")
    return BaseConnectorSettings(
        ironrag_base_url="http://ironrag.invalid",
        ironrag_api_token="test-token",
        request_timeout_seconds=60.0,
        routing_config_path=routing_path,
        state_db_path=tmp_path / "state.sqlite",
        pidfile_path=tmp_path / "connector.pid",
        run_mode=RunMode.WEBHOOK,
    )


@pytest.mark.asyncio
async def test_lifespan_resolves_routing_before_serving(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeIronRagClient.fail_resolution = False
    FakeIronRagClient.instances.clear()
    monkeypatch.setattr("ironrag_connector.server.IronRagClient", FakeIronRagClient)
    adapter = EmptyAdapter()
    settings = _settings(tmp_path)
    app = build_app(settings, adapter.factory)

    async with app.router.lifespan_context(app):
        client = FakeIronRagClient.instances[-1]
        assert client.resolution_calls == [{LIBRARY_REF}]
        assert settings.pidfile_path is not None and settings.pidfile_path.exists()
        assert not client.closed
        assert not adapter.closed

    assert client.closed
    assert adapter.closed
    assert settings.pidfile_path is not None and not settings.pidfile_path.exists()


@pytest.mark.asyncio
async def test_lifespan_resolution_failure_aborts_and_cleans_resources(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    FakeIronRagClient.fail_resolution = True
    FakeIronRagClient.instances.clear()
    monkeypatch.setattr("ironrag_connector.server.IronRagClient", FakeIronRagClient)
    adapter = EmptyAdapter()
    settings = _settings(tmp_path)
    app = build_app(settings, adapter.factory)

    with pytest.raises(IronRagCatalogError, match="not visible"):
        async with app.router.lifespan_context(app):
            raise AssertionError("lifespan must not start")

    client = FakeIronRagClient.instances[-1]
    assert client.resolution_calls == [{LIBRARY_REF}]
    assert client.closed
    assert adapter.closed
    assert settings.pidfile_path is not None and not settings.pidfile_path.exists()


def test_build_app_hands_the_live_policy_table_to_the_adapter_factory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("ironrag_connector.server.IronRagClient", FakeIronRagClient)
    settings = _settings(tmp_path)
    settings.routing_config_path.write_text(
        f"default: {{ library: {LIBRARY_REF} }}\npolicies:\n  page:\n    include_comments: false\n",
        encoding="utf-8",
    )
    adapter = EmptyAdapter()

    build_app(settings, adapter.factory)

    assert adapter.policies is not None
    assert adapter.policies.for_kind("page").include_comments is False
    assert adapter.policies.for_kind("other").include_comments is True


def test_build_app_rejects_policies_for_kinds_the_adapter_never_emits(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr("ironrag_connector.server.IronRagClient", FakeIronRagClient)
    settings = _settings(tmp_path)
    settings.routing_config_path.write_text(
        f"default: {{ library: {LIBRARY_REF} }}\npolicies:\n  pgae:\n    include_comments: false\n",
        encoding="utf-8",
    )

    factory = EmptyAdapter().factory
    with pytest.raises(RoutingError, match="pgae"):
        build_app(settings, factory)


@pytest.mark.asyncio
@pytest.mark.parametrize("legacy_owner", [False, True])
@pytest.mark.parametrize("handler_failure", [False, True])
async def test_webhook_only_delete_automatically_requests_gated_cleanup(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, legacy_owner: bool, handler_failure: bool
) -> None:
    class BookStackPages(DependentPageAdapter):
        name = "bookstack"
        enumerations = 0

        async def iter_items(self) -> Any:
            self.enumerations += 1
            async for ref in super().iter_items():
                yield ref

        async def _routing_facts_for_page(self, page_id: int, item: Any) -> dict[str, Any]:
            return {}

    class ServerIronRag(FakeIronRag):
        async def resolve_library_refs(self, refs: set[str]) -> dict[str, ResolvedLibraryTarget]:
            return {
                ref: ResolvedLibraryTarget(library_ref=ref, workspace_id=WS, library_id=LIB)
                for ref in refs
            }

        async def aclose(self) -> None:
            return None

    started = asyncio.Event()
    allow_scan = asyncio.Event()
    reports: asyncio.Queue[SyncReport] = asyncio.Queue()

    class ObservedSyncManager(SyncManager):
        async def _run_once_unlocked(self, *, reason: str) -> SyncReport:
            started.set()
            await allow_scan.wait()
            report = await super()._run_once_unlocked(reason=reason)
            reports.put_nowait(report)
            return report

    adapter = BookStackPages(
        {"2": echo_page("2", "t1")} if legacy_owner else {},
        {"2": (DependentSpec("attachment", "shared", "a1"),)} if legacy_owner else {},
    )
    if legacy_owner:
        adapter.deferred_pages.add("2")
    base = _settings(tmp_path)
    settings = BookStackSettings(
        **base.model_dump(),
        bookstack_base_url="https://bookstack.invalid",
        bookstack_token_id="test-id",
        bookstack_token_secret="test-secret",
    )
    settings.admin_bearer_token = "test-admin"
    server = ServerIronRag()
    state = StateStore(settings.state_db_path)
    seed_document(
        state,
        server.documents,
        SourceItemRef(item_id="1", kind="page", external_key="bookstack:page:1"),
        change_token="t1",
        document_id=UUID(int=1),
    )
    seed_document(
        state,
        server.documents,
        SourceItemRef(
            item_id="shared", kind="attachment", external_key="bookstack:attachment:shared"
        ),
        change_token="a1",
        document_id=UUID(int=5),
    )
    state.claim_member(
        parent_kind="page",
        parent_item_id="1",
        member=MemberTarget("attachment", "shared", "bookstack:attachment:shared", str(LIB)),
    )
    if legacy_owner:
        seed_document(
            state,
            server.documents,
            SourceItemRef(item_id="2", kind="page", external_key="bookstack:page:2"),
            change_token="t1",
            document_id=UUID(int=2),
        )
    state.close()
    monkeypatch.setattr("ironrag_connector.server.IronRagClient", lambda _: server)
    monkeypatch.setattr("ironrag_connector.server.SyncManager", ObservedSyncManager)

    def webhook_factory(source: Any, orchestrator: Any) -> Any:
        handler = make_bookstack_handler(settings, source, orchestrator)
        handle = handler.handler

        async def deliver(payload: dict[str, Any]) -> Any:
            result = await handle(payload)
            if handler_failure and payload.get("event") == "page_delete":
                raise IronRagError("delivery failed after membership release")
            return result

        handler.handler = deliver
        return [handler]

    app = build_app(
        settings,
        lambda _settings, _policies: adapter,
        webhook_factory=webhook_factory,
    )

    async with (
        app.router.lifespan_context(app),
        httpx.AsyncClient(
            transport=httpx.ASGITransport(app=app, raise_app_exceptions=False),
            base_url="http://connector.invalid",
            headers={"Authorization": "Bearer test-admin"},
        ) as client,
    ):
        response = await client.post(
            "/webhook/bookstack", json={"event": "page_delete", "related_item": {"id": 1}}
        )
        assert response.status_code == (500 if handler_failure else 202)
        if not handler_failure:
            assert response.json()["result"]["action"] == "deleted"
        # The response is already delivered although the canonical sweep
        # cannot proceed yet: cleanup is not an inline full synchronization.
        await asyncio.wait_for(started.wait(), timeout=1)
        assert (LIB, "bookstack:attachment:shared") in server.documents
        assert adapter.enumerations == 0
        allow_scan.set()
        report = await asyncio.wait_for(reports.get(), timeout=1)
        assert report.dependent_census_complete is not legacy_owner
        assert ((LIB, "bookstack:attachment:shared") in server.documents) is legacy_owner
        if legacy_owner:
            assert str(UUID(int=5)) not in server.deletes
            adapter.deferred_pages.clear()
            await client.post("/webhook/bookstack", json={"event": "non-page-event"})
            proven = await asyncio.wait_for(reports.get(), timeout=1)
            assert proven.dependent_census_complete is True
            assert (LIB, "bookstack:attachment:shared") in server.documents
            adapter.set_dependents("2")
            await client.post("/webhook/bookstack", json={"event": "non-page-event"})
            orphaned = await asyncio.wait_for(reports.get(), timeout=1)
            assert orphaned.dependent_census_complete is True
            assert (LIB, "bookstack:attachment:shared") not in server.documents
        # A sweep does not request its successor, even if it released claims.
        with pytest.raises(TimeoutError):
            await asyncio.wait_for(reports.get(), timeout=0.05)
        assert adapter.enumerations == (3 if legacy_owner else 1)


def test_health_reports_the_build_stamp_over_the_packaged_version(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The image's build stamp is the answer to "which build is this".

    Both are present in an image and they can disagree: the packaged version is
    whatever pyproject said at build time, the stamp is the release tag the
    image was published under. Reporting the packaged one made the Dockerfile's
    own blank-version guard a lie — a blank stamp still reported a version.
    """
    from ironrag_connector import version as version_module

    monkeypatch.setenv(version_module.VERSION_ENV, "v0.6.0")
    assert version_module.connector_version() == "v0.6.0"

    # A blank stamp is not a version: fall through to the packaged one rather
    # than report an empty string.
    monkeypatch.setenv(version_module.VERSION_ENV, "   ")
    assert version_module.connector_version() == version_module.distribution_version(
        version_module.DISTRIBUTION
    )

    monkeypatch.delenv(version_module.VERSION_ENV)
    # Nothing installed and nothing stamped is the honest "unknown".
    monkeypatch.setattr(version_module, "distribution_version", _raise_not_found)
    assert version_module.connector_version() == version_module.UNKNOWN


def _raise_not_found(_name: str) -> str:
    from importlib.metadata import PackageNotFoundError

    raise PackageNotFoundError(_name)
