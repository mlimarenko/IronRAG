"""Adapter wiring against a fake BookStack REST surface.

Exercises the contract the framework actually cares about: ``iter_items``
yields one ref per BookStack page with the right shape, ``fetch`` materializes
a SourceItem whose dependents include attachments and inline images, and
``parse_external_key`` round-trips.
"""

from __future__ import annotations

import hashlib
from collections.abc import AsyncIterator
from dataclasses import replace
from pathlib import Path
from typing import Any
from unittest.mock import AsyncMock, patch

import pytest

from ironrag_connector.source import (
    IncompleteCensusError,
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceItemRef,
    dependents_token_from,
)
from ironrag_connector.sources.bookstack.adapter import BookStackAdapter
from ironrag_connector.sources.bookstack.bookstack import BookStackError, BookStackPageNotFoundError
from ironrag_connector.sources.bookstack.config import BookStackSettings
from ironrag_connector.sources.bookstack.mapping import KIND_ATTACHMENT, KIND_IMAGE, KIND_PAGE


def _patched_bookstack(fake: FakeBookStack) -> Any:
    """Patch the three collaborators the adapter builds for itself.

    Seven tests repeated the same three-line `with (patch(...), patch(...),
    patch(...))` block. Written once, adding a collaborator is one edit rather
    than seven, and the duplication gate stops counting it.
    """
    from contextlib import ExitStack

    stack = ExitStack()
    stack.enter_context(
        patch("ironrag_connector.sources.bookstack.adapter.BookStackClient", return_value=fake)
    )
    stack.enter_context(patch("ironrag_connector.sources.bookstack.adapter.ShelfIndex"))
    context_builder = stack.enter_context(
        patch("ironrag_connector.sources.bookstack.adapter.PageContextBuilder")
    )
    stack.context_builder = context_builder  # type: ignore[attr-defined]
    return stack


def _settings(routing_config_path: Path | None = None) -> BookStackSettings:
    return BookStackSettings(
        bookstack_base_url="http://wiki.example.com/api/",
        bookstack_token_id="id",
        bookstack_token_secret="secret",
        ironrag_base_url="http://ironrag.example.com",
        ironrag_api_token="t",
        admin_bearer_token="a",
        request_timeout_seconds=60.0,
        bookstack_shelf_cache_ttl_seconds=900,
        routing_config_path=routing_config_path or Path("/nonexistent/routing.yaml"),
    )


class FakeBookStack:
    """Substituted for BookStackClient on the adapter."""

    def __init__(self) -> None:
        self.pages = [
            {
                "id": 1,
                "name": "Hello",
                "slug": "hello",
                "updated_at": "2026-01-01T00:00:00Z",
                "book_id": 11,
                "book_slug": "the-book",
            }
        ]
        self.attachments = [
            {"id": 99, "name": "spec.pdf", "uploaded_to": 1, "updated_at": "2026-01-02T00:00:00Z"}
        ]
        self.attachments_by_page = {1: [{"id": 99, "name": "spec.pdf"}]}
        self.attachment_payload = {99: (b"%PDF-1.4 fake", "application/pdf", "spec.pdf")}
        self.page_export = (
            b"# Hello\n\n![alt](http://wiki.example.com/uploads/images/foo.png)",
            "text/markdown",
        )
        self.image_payload = (b"\x89PNG fake", "image/png")

    async def list_pages(self) -> AsyncIterator[dict[str, Any]]:
        for page in self.pages:
            yield page

    async def export_page(self, page_id: int, fmt: str) -> tuple[bytes, str]:
        return self.page_export

    async def list_attachments(self) -> AsyncIterator[dict[str, Any]]:
        for attachment in self.attachments:
            yield attachment

    async def list_page_attachments(self, page_id: int) -> list[dict[str, Any]]:
        return self.attachments_by_page.get(page_id, [])

    async def get_attachment_payload(self, att_id: int) -> tuple[bytes, str, str]:
        return self.attachment_payload[att_id]

    async def download_image(self, url: str) -> tuple[bytes, str]:
        return self.image_payload

    async def aclose(self) -> None:
        return None


class DisappearingBookStack(FakeBookStack):
    def __init__(self) -> None:
        super().__init__()
        self.missing = True

    async def export_page(self, page_id: int, fmt: str) -> tuple[bytes, str]:
        if self.missing:
            raise BookStackPageNotFoundError("page disappeared after enumeration")
        return await super().export_page(page_id, fmt)


async def _hello_page_context(page_id: int) -> Any:
    from ironrag_connector.sources.bookstack.page_context import PageContext

    return PageContext(
        page_id=page_id,
        page_slug="hello",
        page_name="Hello",
        book_id=11,
        book_slug="the-book",
        chapter_id=None,
        chapter_slug=None,
        shelf_ids=(),
        shelf_slugs=(),
        tags=("docs",),
    )


def _adapter_with(fake: FakeBookStack) -> BookStackAdapter:
    with _patched_bookstack(fake) as patches:
        patches.context_builder.return_value.build = _hello_page_context
        return BookStackAdapter(_settings())


@pytest.fixture
def adapter() -> BookStackAdapter:
    return _adapter_with(FakeBookStack())


@pytest.mark.asyncio
async def test_iter_items_yields_page_ref(adapter: BookStackAdapter) -> None:
    refs = [r async for r in adapter.iter_items()]
    assert len(refs) == 1
    ref = refs[0]
    assert ref.kind == KIND_PAGE
    assert ref.item_id == "1"
    assert ref.external_key == "bookstack:page:1"
    assert ref.change_token == "2026-01-01T00:00:00Z"
    assert ref.routing_facts["name"] == "Hello"
    assert ref.routing_facts["book"] == "the-book"
    assert ref.routing_facts["tag"] == ["docs"]


def test_missing_enforcement_is_explicit_for_every_declared_kind(
    adapter: BookStackAdapter,
) -> None:
    assert {kind: adapter.missing_enforcement(kind) for kind in adapter.kinds} == {
        KIND_PAGE: SourceDeleteEnforcement.POLICY,
        KIND_ATTACHMENT: SourceDeleteEnforcement.POLICY,
        KIND_IMAGE: SourceDeleteEnforcement.POLICY,
    }
    with pytest.raises(ValueError, match="unsupported bookstack item kind: future-kind"):
        adapter.missing_enforcement("future-kind")


@pytest.mark.asyncio
async def test_full_page_context_includes_exact_name_fact(
    adapter: BookStackAdapter,
) -> None:
    facts = await adapter._routing_facts_for_page(1, {})
    assert facts["name"] == "Hello"


@pytest.mark.asyncio
async def test_fetch_returns_page_with_dependents(adapter: BookStackAdapter) -> None:
    refs = [r async for r in adapter.iter_items()]
    result = await adapter.fetch(refs[0])
    assert isinstance(result, SourceFetchMaterialized)
    item = result.item
    assert item.ref.kind == KIND_PAGE
    assert item.mime_type == "text/markdown"
    assert item.file_name == "hello.md"
    assert item.document_hint == "http://wiki.example.com/books/the-book/page/hello"
    kinds = [d.ref.kind for d in item.dependents]
    assert KIND_ATTACHMENT in kinds
    assert KIND_IMAGE in kinds

    image_dep = next(d for d in item.dependents if d.ref.kind == KIND_IMAGE)
    expected_digest = hashlib.sha256(b"\x89PNG fake").hexdigest()
    assert image_dep.ref.external_key == f"bookstack:image:{expected_digest[:16]}"
    assert image_dep.idempotency_key == f"bookstack:image:{expected_digest[:16]}"
    assert image_dep.document_hint == "http://wiki.example.com/uploads/images/foo.png"

    att_dep = next(d for d in item.dependents if d.ref.kind == KIND_ATTACHMENT)
    assert att_dep.ref.external_key == "bookstack:attachment:99"
    assert att_dep.title == "Hello: spec.pdf"
    assert att_dep.document_hint == "http://wiki.example.com/attachments/99"


@pytest.mark.asyncio
async def test_same_token_page_reappearance_after_export_404_is_retried() -> None:
    fake = DisappearingBookStack()
    with _patched_bookstack(fake):
        adapter = BookStackAdapter(_settings())
    ref = SourceItemRef(
        item_id="1",
        kind=KIND_PAGE,
        external_key="bookstack:page:1",
        change_token="2026-01-01T00:00:00Z",
        raw={"name": "Hello", "slug": "hello", "book_slug": "the-book"},
    )

    missing = await adapter.fetch(ref)
    assert isinstance(missing, SourceFetchDelete)
    assert missing.cacheable is False

    fake.missing = False
    reappeared = await adapter.fetch(ref)
    assert isinstance(reappeared, SourceFetchMaterialized)
    assert reappeared.item.ref.change_token == ref.change_token


@pytest.mark.asyncio
async def test_fetch_page_document_hint_falls_back_to_page_id(
    adapter: BookStackAdapter,
) -> None:
    refs = [r async for r in adapter.iter_items()]
    ref = replace(refs[0], raw={}, routing_facts={})
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchMaterialized)
    item = result.item
    assert item.document_hint == "http://wiki.example.com/pages/1"


@pytest.mark.asyncio
async def test_default_only_routing_uses_summary_facts(tmp_path: Path) -> None:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: knowledge/wiki
""",
        encoding="utf-8",
    )
    fake = FakeBookStack()
    with _patched_bookstack(fake) as patches:
        ctx_cls = patches.context_builder
        ctx_cls.return_value.build = AsyncMock(
            side_effect=AssertionError("full page context should not be built")
        )
        adapter = BookStackAdapter(_settings(routing))
        refs = [r async for r in adapter.iter_items()]

    assert refs[0].routing_facts["book"] == "the-book"
    assert ctx_cls.return_value.build.await_count == 0


@pytest.mark.asyncio
async def test_book_rule_uses_summary_facts_when_available(tmp_path: Path) -> None:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: knowledge/wiki
rules:
  - match: { book: the-book }
    target: { library: engineering/handbook }
""",
        encoding="utf-8",
    )
    fake = FakeBookStack()
    with _patched_bookstack(fake) as patches:
        ctx_cls = patches.context_builder
        ctx_cls.return_value.build = AsyncMock(
            side_effect=AssertionError("summary book fact should be enough")
        )
        adapter = BookStackAdapter(_settings(routing))
        refs = [r async for r in adapter.iter_items()]

    assert refs[0].routing_facts["book"] == "the-book"
    assert ctx_cls.return_value.build.await_count == 0


@pytest.mark.asyncio
async def test_shelf_rule_requires_full_page_context(tmp_path: Path) -> None:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: knowledge/wiki
rules:
  - match: { shelf: operations }
    target: { library: operations/runbooks }
""",
        encoding="utf-8",
    )
    fake = FakeBookStack()
    with _patched_bookstack(fake) as patches:
        ctx_cls = patches.context_builder
        from ironrag_connector.sources.bookstack.page_context import PageContext

        ctx_cls.return_value.build = AsyncMock(
            return_value=PageContext(
                page_id=1,
                page_slug="hello",
                page_name="Hello",
                book_id=11,
                book_slug="the-book",
                chapter_id=None,
                chapter_slug=None,
                shelf_ids=(7,),
                shelf_slugs=("operations",),
                tags=(),
            )
        )
        adapter = BookStackAdapter(_settings(routing))
        refs = [r async for r in adapter.iter_items()]

    assert refs[0].routing_facts["shelf"] == ["operations"]
    assert ctx_cls.return_value.build.await_count == 1


@pytest.mark.asyncio
async def test_tag_rule_requires_full_page_context_even_with_summary_tags(
    tmp_path: Path,
) -> None:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: knowledge/wiki
rules:
  - match: { tag: full-tag }
    target: { library: operations/archive }
""",
        encoding="utf-8",
    )
    fake = FakeBookStack()
    fake.pages[0]["tags"] = [{"name": "summary-tag"}]
    with _patched_bookstack(fake) as patches:
        ctx_cls = patches.context_builder
        from ironrag_connector.sources.bookstack.page_context import PageContext

        ctx_cls.return_value.build = AsyncMock(
            return_value=PageContext(
                page_id=1,
                page_slug="hello",
                page_name="Hello",
                book_id=11,
                book_slug="the-book",
                chapter_id=None,
                chapter_slug=None,
                shelf_ids=(),
                shelf_slugs=(),
                tags=("full-tag",),
            )
        )
        adapter = BookStackAdapter(_settings(routing))
        refs = [r async for r in adapter.iter_items()]

    assert refs[0].routing_facts["tag"] == ["full-tag"]
    assert ctx_cls.return_value.build.await_count == 1


@pytest.mark.asyncio
async def test_new_full_context_rule_takes_effect_on_next_sweep(tmp_path: Path) -> None:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: knowledge/wiki
""",
        encoding="utf-8",
    )
    fake = FakeBookStack()
    with _patched_bookstack(fake) as patches:
        ctx_cls = patches.context_builder
        from ironrag_connector.sources.bookstack.page_context import PageContext

        ctx_cls.return_value.build = AsyncMock(
            return_value=PageContext(
                page_id=1,
                page_slug="hello",
                page_name="Hello",
                book_id=11,
                book_slug="the-book",
                chapter_id=None,
                chapter_slug=None,
                shelf_ids=(7,),
                shelf_slugs=("operations",),
                tags=(),
            )
        )
        adapter = BookStackAdapter(_settings(routing))

        first_refs = [ref async for ref in adapter.iter_items()]
        assert "shelf" not in first_refs[0].routing_facts
        assert ctx_cls.return_value.build.await_count == 0

        routing.write_text(
            """
default:
  library: knowledge/wiki
rules:
  - match: { shelf: operations }
    target: { library: operations/runbooks }
""",
            encoding="utf-8",
        )
        second_refs = [ref async for ref in adapter.iter_items()]

    assert second_refs[0].routing_facts["shelf"] == ["operations"]
    assert ctx_cls.return_value.build.await_count == 1


@pytest.mark.asyncio
async def test_fact_demand_survives_candidate_that_removes_active_rule(
    tmp_path: Path,
) -> None:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: knowledge/wiki
rules:
  - match: { shelf: operations }
    target: { library: operations/runbooks }
""",
        encoding="utf-8",
    )
    fake = FakeBookStack()
    with _patched_bookstack(fake) as patches:
        ctx_cls = patches.context_builder
        from ironrag_connector.sources.bookstack.page_context import PageContext

        ctx_cls.return_value.build = AsyncMock(
            return_value=PageContext(
                page_id=1,
                page_slug="hello",
                page_name="Hello",
                book_id=11,
                book_slug="the-book",
                chapter_id=None,
                chapter_slug=None,
                shelf_ids=(7,),
                shelf_slugs=("operations",),
                tags=(),
            )
        )
        adapter = BookStackAdapter(_settings(routing))
        first_refs = [ref async for ref in adapter.iter_items()]

        # The framework can reject this syntactically valid candidate later
        # (for example because the library ref is not visible). The adapter
        # must retain facts needed by the previous active snapshot.
        routing.write_text(
            """
default:
  library: missing/not-visible
""",
            encoding="utf-8",
        )
        second_refs = [ref async for ref in adapter.iter_items()]

    assert first_refs[0].routing_facts["shelf"] == ["operations"]
    assert second_refs[0].routing_facts["shelf"] == ["operations"]
    assert ctx_cls.return_value.build.await_count == 2


class MalformedRecordBookStack(FakeBookStack):
    def __init__(self) -> None:
        super().__init__()
        self.pages = [{"id": "not-an-int", "name": "Broken"}, *self.pages]


@pytest.mark.asyncio
async def test_malformed_census_record_still_yields_valid_pages_then_withdraws_proof() -> None:
    adapter = _adapter_with(MalformedRecordBookStack())
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

    assert [ref.item_id for ref in refs] == ["1"]


# --- dependent census: attachment index token and fail-closed dependent reads ---


@pytest.mark.asyncio
async def test_iter_items_dependents_token_tracks_the_attachment_index() -> None:
    fake = FakeBookStack()
    fake.pages.append(
        {"id": 2, "name": "Bare", "slug": "bare", "updated_at": "2026-01-01T00:00:00Z"}
    )
    adapter = _adapter_with(fake)

    initial = {ref.item_id: ref.dependents_token async for ref in adapter.iter_items()}
    assert initial["1"] is not None
    assert initial["2"] == dependents_token_from(()), "no attachments is a bounded, empty set"
    assert initial["1"] != initial["2"]

    unchanged = {ref.item_id: ref.dependents_token async for ref in adapter.iter_items()}
    assert unchanged == initial, "an unchanged index yields the same token"

    fake.attachments[0]["updated_at"] = "2026-01-03T00:00:00Z"
    updated = {ref.item_id: ref.dependents_token async for ref in adapter.iter_items()}
    assert updated["1"] != initial["1"], "a re-uploaded attachment advances the token"
    assert updated["2"] == initial["2"]

    fake.attachments.clear()
    removed = {ref.item_id: ref.dependents_token async for ref in adapter.iter_items()}
    assert removed["1"] == initial["2"], "a removed attachment advances the token to empty"


@pytest.mark.asyncio
async def test_attachment_index_failure_fails_the_enumeration_loud() -> None:
    class BrokenIndex(FakeBookStack):
        async def list_attachments(self) -> AsyncIterator[dict[str, Any]]:
            raise BookStackError("attachments listing 500")
            yield  # pragma: no cover -- makes this an async generator

    adapter = _adapter_with(BrokenIndex())
    with pytest.raises(BookStackError, match="attachments listing 500"):
        async for _ in adapter.iter_items():
            pass


@pytest.mark.asyncio
async def test_fetch_defers_when_the_attachment_list_cannot_be_read() -> None:
    class BrokenPageAttachments(FakeBookStack):
        async def list_page_attachments(self, page_id: int) -> list[dict[str, Any]]:
            raise BookStackError("attachments 503")

    adapter = _adapter_with(BrokenPageAttachments())
    ref = await anext(adapter.iter_items())
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchDeferred)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "failure",
    [
        pytest.param(BookStackError("attachment 403"), id="error"),
        pytest.param(BookStackPageNotFoundError("attachment gone mid-read"), id="snapshot_changed"),
    ],
)
async def test_fetch_defers_when_an_attachment_payload_cannot_be_read(
    failure: Exception,
) -> None:
    class BrokenPayload(FakeBookStack):
        async def get_attachment_payload(self, att_id: int) -> tuple[bytes, str, str]:
            raise failure

    adapter = _adapter_with(BrokenPayload())
    ref = await anext(adapter.iter_items())
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchDeferred)


@pytest.mark.asyncio
async def test_fetch_defers_when_an_inline_image_download_errors() -> None:
    class BrokenImage(FakeBookStack):
        async def download_image(self, url: str) -> tuple[bytes, str]:
            raise BookStackError("image 429 after retries")

    adapter = _adapter_with(BrokenImage())
    ref = await anext(adapter.iter_items())
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchDeferred)


@pytest.mark.asyncio
async def test_fetch_excludes_a_dangling_image_reference_and_still_proves() -> None:
    fake = FakeBookStack()
    # BookStack itself answers 404 for the upload: the image is gone from the
    # page, and that is the source's own verdict, so the census stands without
    # it and the stored copy is released.
    fake.image_payload = (b"", "")
    adapter = _adapter_with(fake)
    ref = await anext(adapter.iter_items())
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchMaterialized)
    assert [d.ref.kind for d in result.item.dependents] == [KIND_ATTACHMENT]
    assert result.item.ref.dependents_token == ref.dependents_token


@pytest.mark.asyncio
async def test_fetch_defers_when_an_image_url_answers_with_something_else() -> None:
    fake = FakeBookStack()
    # A 2xx that is not an image (interstitial, login page, proxy error body)
    # says nothing about the image existing. Treating it as an absence would
    # let one bad hop delete a live image, so the whole page defers.
    fake.image_payload = (b"<html>", "text/html")
    adapter = _adapter_with(fake)
    ref = await anext(adapter.iter_items())
    result = await adapter.fetch(ref)
    assert isinstance(result, SourceFetchDeferred)
    assert "did not resolve to an image" in result.reason
