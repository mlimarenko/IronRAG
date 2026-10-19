from __future__ import annotations

from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any
from unittest.mock import patch
from uuid import UUID

import httpx
import pytest
from sdk_dependent_fixtures import LIB, first_ref, router_to, seed_document
from test_sdk_orchestrator import PolicyFailureIronRag

from ironrag_connector.orchestrator import OrchestrationFailedError, Orchestrator
from ironrag_connector.policy import PushPolicy
from ironrag_connector.routing import PolicyOverrides
from ironrag_connector.source import (
    IncompleteCensusError,
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceItemRef,
    dependents_token_from,
)
from ironrag_connector.sources.confluence.adapter import ConfluenceAdapter
from ironrag_connector.sources.confluence.config import ConfluenceSettings
from ironrag_connector.sources.confluence.confluence import (
    ConfluenceError,
    ConfluenceNotFoundError,
)
from ironrag_connector.sources.confluence.mapping import KIND_ATTACHMENT, KIND_PAGE
from ironrag_connector.sources.confluence.webhook import make_confluence_handler
from ironrag_connector.state import StateStore


def _settings() -> ConfluenceSettings:
    return ConfluenceSettings(
        confluence_base_url="https://wiki.example.com",
        confluence_auth_mode="anonymous",
        ironrag_base_url="http://ironrag.example.com",
        ironrag_api_token="token",
        admin_bearer_token="admin",
    )


class FakeConfluence:
    async def list_pages(self) -> AsyncIterator[dict[str, Any]]:
        yield {
            "id": "42",
            "title": "Runbook",
            "status": "current",
            "space": {"key": "OPS", "name": "Operations"},
            "version": {"when": "2026-05-22T00:00:00.000Z"},
            "ancestors": [{"id": "10", "title": "Runbooks"}],
        }

    async def get_page(self, page_id: str) -> dict[str, Any]:
        return {
            "id": page_id,
            "title": "Runbook",
            "status": "current",
            "space": {"key": "OPS", "name": "Operations"},
            "version": {"when": "2026-05-22T00:00:00.000Z"},
            "ancestors": [{"id": "10", "title": "Runbooks"}],
            "body": {"export_view": {"value": "<p>Hello from Confluence.</p>"}},
            "_links": {"webui": "/display/OPS/Runbook"},
        }

    def __init__(self) -> None:
        self.attachment_index = [
            {
                "id": "77",
                "type": "attachment",
                "container": {"id": "42", "type": "page"},
                "version": {"when": "2026-05-22T01:00:00.000Z"},
            }
        ]

    async def list_attachment_index(self) -> AsyncIterator[dict[str, Any]]:
        for attachment in self.attachment_index:
            yield attachment

    async def list_attachments(self, page_id: str) -> list[dict[str, Any]]:
        return [
            {
                "id": "77",
                "title": "ops-guide.pdf",
                "version": {"when": "2026-05-22T01:00:00.000Z"},
                "_links": {"download": "/download/attachments/42/ops-guide.pdf"},
            }
        ]

    async def download(self, path_or_url: str) -> tuple[bytes, str]:
        return b"%PDF-fake", "application/pdf"

    async def aclose(self) -> None:
        return None


class DisappearingConfluence(FakeConfluence):
    def __init__(self) -> None:
        super().__init__()
        self.missing = True

    async def get_page(self, page_id: str) -> dict[str, Any]:
        if self.missing:
            raise ConfluenceNotFoundError("page disappeared after enumeration")
        return await super().get_page(page_id)


@pytest.mark.asyncio
async def test_terminal_page_failure_settles_actual_attachments_and_webhook_still_fails(
    tmp_path: Path,
) -> None:
    fake = FakeConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
    ref = await first_ref(adapter)
    state = StateStore(tmp_path / "state.sqlite")
    server = PolicyFailureIronRag(ref.external_key)
    seed_document(state, server.documents, ref, change_token="old", document_id=UUID(int=1))
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=server,  # type: ignore[arg-type] -- stateful production-client double
        router=router_to(LIB),
        state=state,
        policies=PolicyOverrides(default=PushPolicy(), by_kind={}),
    )

    with pytest.raises(OrchestrationFailedError) as caught:
        await orchestrator.push_ref(ref)

    assert [out.ref.external_key for out in caught.value.dependent_outcomes] == [
        "confluence:attachment:77"
    ]
    assert state.parent_census("page", "42") is not None
    assert state.has_claims("attachment", "77", str(LIB))
    assert (LIB, "confluence:attachment:77") in server.documents
    assert state.get("page", "42").change_token == "old"
    handler = make_confluence_handler(_settings(), adapter, orchestrator)
    # Verify the webhook consumer forwards this typed receipt as failure. The
    # real adapter/ingest path above uses an enumerated, validated snapshot;
    # webhook snapshot acquisition is a separate contract.
    with patch.object(orchestrator, "push_ref", side_effect=caught.value) as push:
        with pytest.raises(OrchestrationFailedError) as webhook_failure:
            await handler.handler({"event": "page_updated", "page": {"id": "42"}})
        assert webhook_failure.value is caught.value
        push.assert_awaited_once()
    assert all(server._operations[op].status.value == "failed" for op in server.failures)


def test_missing_enforcement_is_explicit_for_every_declared_kind() -> None:
    fake = FakeConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())

    assert {kind: adapter.missing_enforcement(kind) for kind in adapter.kinds} == {
        KIND_PAGE: SourceDeleteEnforcement.POLICY,
        KIND_ATTACHMENT: SourceDeleteEnforcement.POLICY,
    }
    with pytest.raises(ValueError, match="unsupported confluence item kind: future-kind"):
        adapter.missing_enforcement("future-kind")


@pytest.mark.asyncio
async def test_iter_items_and_fetch() -> None:
    fake = FakeConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        refs = [ref async for ref in adapter.iter_items()]
        assert len(refs) == 1
        ref = refs[0]
        assert ref.kind == KIND_PAGE
        assert ref.external_key == "confluence:page:42"
        assert ref.routing_facts["space_key"] == "OPS"
        assert ref.routing_facts["ancestor_title"] == ["Runbooks"]
        assert ref.routing_facts["status"] == "current"

        result = await adapter.fetch(ref)
        assert isinstance(result, SourceFetchMaterialized)
        item = result.item
        assert item.mime_type == "text/html"
        assert item.document_hint == "https://wiki.example.com/display/OPS/Runbook"
        assert len(item.dependents) == 1
        dependent = item.dependents[0]
        assert dependent.ref.kind == KIND_ATTACHMENT
        assert dependent.ref.external_key == "confluence:attachment:77"
        assert dependent.mime_type == "application/pdf"


@pytest.mark.asyncio
async def test_same_token_page_reappearance_after_get_404_is_retried() -> None:
    fake = DisappearingConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())

        missing = await adapter.fetch(ref)
        assert isinstance(missing, SourceFetchDelete)
        assert missing.cacheable is False

        fake.missing = False
        reappeared = await adapter.fetch(ref)
        assert isinstance(reappeared, SourceFetchMaterialized)
        assert reappeared.item.ref.change_token == ref.change_token


class EmptyBodyConfluence(FakeConfluence):
    """Page whose own body is empty but which hosts one attachment."""

    def __init__(self, *, with_attachment: bool) -> None:
        super().__init__()
        self._with_attachment = with_attachment

    def add_attachment(self) -> None:
        self._with_attachment = True

    async def get_page(self, page_id: str) -> dict[str, Any]:
        return {
            "id": page_id,
            "title": "Product",
            "status": "current",
            "space": {"key": "OPS", "name": "Operations"},
            "version": {"when": "2026-05-22T00:00:00.000Z"},
            "ancestors": [],
            "body": {"export_view": {"value": ""}, "storage": {"value": ""}},
            "_links": {"webui": "/display/OPS/Product"},
        }

    async def list_attachments(self, page_id: str) -> list[dict[str, Any]]:
        if not self._with_attachment:
            return []
        return [
            {
                "id": "88",
                "title": "spec.pdf",
                "version": {"when": "2026-05-22T01:00:00.000Z"},
                "_links": {"download": "/download/attachments/42/spec.pdf"},
            }
        ]


class FailingAttachmentConfluence(FakeConfluence):
    def __init__(self, *, empty_body: bool = False) -> None:
        super().__init__()
        self.empty_body = empty_body
        self.list_fails = False
        self.failed_downloads: set[str] = set()

    async def get_page(self, page_id: str) -> dict[str, Any]:
        page = await super().get_page(page_id)
        if self.empty_body:
            page["body"] = {"export_view": {"value": ""}, "storage": {"value": ""}}
        return page

    async def list_attachments(self, page_id: str) -> list[dict[str, Any]]:
        if self.list_fails:
            raise ConfluenceError("temporary attachment listing failure")
        return [
            {
                "id": "77",
                "title": "ops-guide.pdf",
                "version": {"when": "2026-05-22T01:00:00.000Z"},
                "_links": {"download": "/download/attachments/42/ops-guide.pdf"},
            },
            {
                "id": "78",
                "title": "network-map.png",
                "version": {"when": "2026-05-22T01:01:00.000Z"},
                "_links": {"download": "/download/attachments/42/network-map.png"},
            },
        ]

    async def download(self, path_or_url: str) -> tuple[bytes, str]:
        if path_or_url in self.failed_downloads:
            raise ConfluenceError("temporary attachment download failure")
        return f"payload:{path_or_url}".encode(), "application/octet-stream"


class MismatchedPageConfluence(FakeConfluence):
    async def get_page(self, page_id: str) -> dict[str, Any]:
        page = await super().get_page(page_id)
        page["id"] = "different-page"
        page["title"] = "Content from a different page"
        page["body"] = {"export_view": {"value": "<p>foreign payload</p>"}}
        return page


@pytest.mark.asyncio
async def test_empty_body_with_attachment_still_ingests() -> None:
    fake = EmptyBodyConfluence(with_attachment=True)
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())
        result = await adapter.fetch(ref)
        assert isinstance(result, SourceFetchMaterialized), (
            "empty body must not drop a page that has attachments"
        )
        item = result.item
        body = item.payload.decode("utf-8")
        assert "spec.pdf" in body, "synthesized index must link the attachment"
        assert len(item.dependents) == 1
        assert item.dependents[0].ref.external_key == "confluence:attachment:88"


@pytest.mark.asyncio
async def test_empty_body_delete_is_retried_when_attachment_can_appear() -> None:
    fake = EmptyBodyConfluence(with_attachment=False)
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())
        initial = await adapter.fetch(ref)
        assert isinstance(initial, SourceFetchDelete)
        assert initial.enforcement is SourceDeleteEnforcement.REQUIRED
        assert initial.cacheable is False, (
            "the parent token cannot cache attachment-dependent absence"
        )

        fake.add_attachment()
        materialized = await adapter.fetch(ref)
        assert isinstance(materialized, SourceFetchMaterialized)
        assert materialized.item.ref.change_token == ref.change_token
        assert materialized.item.dependents[0].ref.external_key == "confluence:attachment:88"


@pytest.mark.asyncio
async def test_attachment_list_failure_defers_whole_parent_materialization() -> None:
    fake = FailingAttachmentConfluence(empty_body=True)
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())

        complete = await adapter.fetch(ref)
        assert isinstance(complete, SourceFetchMaterialized)
        assert len(complete.item.dependents) == 2

        fake.list_fails = True
        failed = await adapter.fetch(ref)
        assert isinstance(failed, SourceFetchDeferred)


@pytest.mark.asyncio
async def test_all_attachment_download_failures_defer_whole_parent_materialization() -> None:
    fake = FailingAttachmentConfluence(empty_body=True)
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())

        complete = await adapter.fetch(ref)
        assert isinstance(complete, SourceFetchMaterialized)
        assert len(complete.item.dependents) == 2

        fake.failed_downloads = {
            "/download/attachments/42/ops-guide.pdf",
            "/download/attachments/42/network-map.png",
        }
        failed = await adapter.fetch(ref)
        assert isinstance(failed, SourceFetchDeferred)


@pytest.mark.asyncio
async def test_partial_attachment_download_failure_defers_whole_parent_materialization() -> None:
    fake = FailingAttachmentConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())

        complete = await adapter.fetch(ref)
        assert isinstance(complete, SourceFetchMaterialized)
        assert {item.ref.item_id for item in complete.item.dependents} == {"77", "78"}

        fake.failed_downloads = {"/download/attachments/42/network-map.png"}
        failed = await adapter.fetch(ref)
        assert isinstance(failed, SourceFetchDeferred)


@pytest.mark.asyncio
async def test_a_transport_timeout_on_a_download_defers_instead_of_escaping() -> None:
    """A read timeout is not the client's own error type, and must still defer.

    Letting it escape would end the fetch outside the census contract, and the
    parent would keep a census that no longer describes what was read.
    """

    class TimingOutConfluence(FakeConfluence):
        async def download(self, path_or_url: str) -> tuple[bytes, str]:
            raise httpx.ReadTimeout("attachment read timed out")

    with patch(
        "ironrag_connector.sources.confluence.adapter.ConfluenceClient",
        return_value=TimingOutConfluence(),
    ):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())
        result = await adapter.fetch(ref)
        assert isinstance(result, SourceFetchDeferred)
        assert "could not be read completely" in result.reason


@pytest.mark.asyncio
async def test_an_empty_download_body_defers_rather_than_proving_an_absence() -> None:
    """A 2xx with no bytes is a read that returned nothing, not a deletion.

    Excluding the attachment would make the parent census prove its absence and
    delete a live attachment on the strength of one empty response.
    """

    class EmptyDownloadConfluence(FakeConfluence):
        async def download(self, path_or_url: str) -> tuple[bytes, str]:
            return b"", "application/pdf"

    with patch(
        "ironrag_connector.sources.confluence.adapter.ConfluenceClient",
        return_value=EmptyDownloadConfluence(),
    ):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())
        result = await adapter.fetch(ref)
        assert isinstance(result, SourceFetchDeferred)
        assert "no bytes" in result.reason


@pytest.mark.asyncio
async def test_mismatched_page_response_identity_is_not_materialized_under_requested_key() -> None:
    fake = MismatchedPageConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())

        result = await adapter.fetch(ref)
        assert isinstance(result, SourceFetchDeferred)


async def _first(it: AsyncIterator[Any]) -> Any:
    async for x in it:
        return x
    raise AssertionError("empty iter")


class MalformedRecordConfluence(FakeConfluence):
    async def list_pages(self) -> AsyncIterator[dict[str, Any]]:
        yield {"id": 42, "title": "Numeric id"}
        async for page in super().list_pages():
            yield page


@pytest.mark.asyncio
async def test_malformed_census_record_still_yields_valid_pages_then_withdraws_proof() -> None:
    with patch(
        "ironrag_connector.sources.confluence.adapter.ConfluenceClient",
        return_value=MalformedRecordConfluence(),
    ):
        adapter = ConfluenceAdapter(_settings())
    refs: list[SourceItemRef] = []
    items = adapter.iter_items()

    async def drain() -> None:
        # Collected one ref at a time on purpose: the assertion below is about
        # what the census had already yielded when it withdrew its proof, and a
        # comprehension would throw that partial result away with the exception.
        async for ref in items:
            refs.append(ref)

    with pytest.raises(IncompleteCensusError, match="1 record"):
        await drain()

    assert [ref.item_id for ref in refs] == ["42"]


# --- dependent census: attachment index token carried from ref to materialized item ---


@pytest.mark.asyncio
async def test_iter_items_dependents_token_tracks_the_attachment_index() -> None:
    fake = FakeConfluence()
    with patch("ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=fake):
        adapter = ConfluenceAdapter(_settings())
        initial = (await _first(adapter.iter_items())).dependents_token
        assert initial is not None
        assert initial != dependents_token_from(())

        fake.attachment_index[0]["version"] = {"when": "2026-05-23T01:00:00.000Z"}
        advanced = (await _first(adapter.iter_items())).dependents_token
        assert advanced != initial

        fake.attachment_index[0]["container"] = {"id": "other-page", "type": "page"}
        moved_away = (await _first(adapter.iter_items())).dependents_token
        assert moved_away == dependents_token_from(())


@pytest.mark.asyncio
async def test_attachment_index_failure_fails_the_enumeration_loud() -> None:
    class BrokenIndex(FakeConfluence):
        async def list_attachment_index(self) -> AsyncIterator[dict[str, Any]]:
            raise ConfluenceError("attachment search 500")
            yield  # pragma: no cover -- makes this an async generator

    with patch(
        "ironrag_connector.sources.confluence.adapter.ConfluenceClient", return_value=BrokenIndex()
    ):
        adapter = ConfluenceAdapter(_settings())
        items = adapter.iter_items()
        with pytest.raises(ConfluenceError, match="attachment search 500"):
            await _first(items)


@pytest.mark.asyncio
async def test_fetch_carries_dependents_token_onto_the_materialized_ref() -> None:
    with patch(
        "ironrag_connector.sources.confluence.adapter.ConfluenceClient",
        return_value=FakeConfluence(),
    ):
        adapter = ConfluenceAdapter(_settings())
        ref = await _first(adapter.iter_items())
        result = await adapter.fetch(ref)
        assert isinstance(result, SourceFetchMaterialized)
        assert result.item.ref.dependents_token == ref.dependents_token
        assert result.item.ref.dependents_token is not None
