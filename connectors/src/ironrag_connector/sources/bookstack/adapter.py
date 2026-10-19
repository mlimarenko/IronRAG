"""SourceAdapter implementation for BookStack.

What the adapter emits
======================

* ``page`` refs for every page returned by ``/api/pages``. Routing facts
  include ``page_id``, ``page_slug``, ``name``, ``book_id``, ``book``,
  ``chapter_id``, ``chapter``, ``shelf_ids``, ``shelf``, and ``tag`` so the
  operator can map a page to a target library using BookStack vocabulary
  without the framework knowing it.
* When the framework asks for a page's full payload, the adapter also
  attaches each of the page's BookStack attachments and each inline
  image referenced in the exported markdown as dependent
  :class:`SourceItem`s. Dependents reuse the same orchestrator code path
  and obey their own kind's policy.

Dependent census
================

A page's ``dependents_token`` is derived from the instance-wide attachment
index (``/api/attachments``: id and ``updated_at`` of every attachment
uploaded to the page), read once per sweep before pages are yielded. It
bounds attachment drift: a re-uploaded, added or removed attachment moves
the token, so the framework refetches the page and re-proves its dependent
set. Inline images are not indexed by BookStack; their drift is bounded by
the page's own ``updated_at`` because the markdown that references them
changes with the page. A failure to read the index fails the enumeration:
pages cannot be yielded with an unknown dependent set.

The materialized dependent tuple is the page's complete dependent set. A
dependent read that fails (attachment list, attachment payload, image
download error, a foreign host behind an external attachment) defers the
whole page; the framework then keeps the previously proven set. Only
BookStack's own verdict excludes a dependent from the set: a 404 for the
image upload, or an attachment record it stores with no content. An
answer that is not the dependent (a 2xx that is not an image) proves
nothing and defers, because the set is what the reaper deletes against.

Reaping
=======

The framework's reaper walks IronRAG documents under the three
``bookstack:`` external-key prefixes and applies the per-kind
``on_missing`` policy. Pages are reaped against the census; attachments
and images against durable membership, once every enumerated page holds a
proven dependent set. Images default to ``ignore`` because they are
content-addressed: a sha256 prefix that no live page references right
now may still be valid for a page that hides it behind a feature flag
or seasonal switch.
"""

from __future__ import annotations

import hashlib
from collections.abc import AsyncIterator
from pathlib import Path
from typing import Any

from ironrag_connector import (
    IncompleteCensusError,
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
    dependents_token_from,
)
from ironrag_connector.observability import get_logger
from ironrag_connector.routing import load_routing_config

from .bookstack import (
    FORMAT_EXTENSION,
    BookStackClient,
    BookStackError,
    BookStackPageNotFoundError,
)
from .config import BookStackSettings
from .mapping import (
    CONNECTOR_NAME,
    KIND_ATTACHMENT,
    KIND_IMAGE,
    KIND_PAGE,
    KINDS,
    attachment_external_key,
    build_external_key,
    image_external_key,
    page_external_key,
    parse_external_key,
)
from .markdown import collect_bookstack_image_urls
from .page_context import PageContextBuilder, ShelfIndex

log = get_logger(__name__)
FULL_CONTEXT_ROUTING_KEYS = {"shelf", "shelf_ids", "tag"}


class BookStackAdapter(SourceAdapter):
    name = CONNECTOR_NAME
    kinds = KINDS
    # Pages are enumerated; attachments and images come only as dependents
    # of a page fetch and are reaped against the durable membership those
    # fetches prove.
    primary_kinds = (KIND_PAGE,)

    def __init__(self, settings: BookStackSettings) -> None:
        self._settings = settings
        self._client = BookStackClient(settings)
        # Browser-facing base URL: same canonicalization as the REST
        # client (strip trailing slash, drop /api suffix) but computed
        # inline so tests that mock BookStackClient still get a real string.
        raw = settings.bookstack_base_url.rstrip("/")
        self._browser_base_url = raw[:-4] if raw.endswith("/api") else raw
        self._shelves = ShelfIndex(self._client, settings.bookstack_shelf_cache_ttl_seconds)
        self._context_builder = PageContextBuilder(
            settings,
            self._client,
            self._shelves,
            needs_shelves=True,
            needs_tags=True,
        )
        self._routing_match_keys = _load_routing_match_keys(settings.routing_config_path)

    async def close(self) -> None:
        await self._client.aclose()

    def external_key(self, kind: str, item_id: str) -> str:
        return build_external_key(kind, item_id)

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return parse_external_key(external_key)

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind in (KIND_PAGE, KIND_ATTACHMENT, KIND_IMAGE):
            return SourceDeleteEnforcement.POLICY
        raise ValueError(f"unsupported {self.name} item kind: {kind}")

    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        # Routing is hot-reloaded by the framework. Refresh the fact demand at
        # each sweep as well so a newly introduced shelf/tag rule immediately
        # receives the full BookStack context it needs. Keep demand monotonic:
        # the framework may reject a candidate later during catalog resolution,
        # and dropping facts required by its still-active snapshot would make
        # that snapshot route incorrectly. Extra context is safe and bounded.
        refreshed_keys = _load_routing_match_keys(self._settings.routing_config_path)
        if self._routing_match_keys is None or refreshed_keys is None:
            self._routing_match_keys = None
        else:
            self._routing_match_keys.update(refreshed_keys)
        attachment_index = await self._attachment_index()
        empty_set_token = dependents_token_from(())
        malformed_records = 0
        async for page in self._client.list_pages():
            page_id = page.get("id")
            if not isinstance(page_id, int) or page_id <= 0:
                # A record without an identity cannot be yielded, and dropping
                # it would make its page look deleted to the reaper.
                malformed_records += 1
                continue
            updated_at = page.get("updated_at") if isinstance(page.get("updated_at"), str) else None
            facts = await self._routing_facts_for_summary(page_id, page)
            yield SourceItemRef(
                item_id=str(page_id),
                kind=KIND_PAGE,
                external_key=page_external_key(page_id),
                change_token=updated_at,
                dependents_token=attachment_index.get(page_id, empty_set_token),
                routing_facts=facts,
                raw=page,
            )
        if malformed_records:
            raise IncompleteCensusError(
                f"BookStack page census contained {malformed_records} record(s) without a valid id"
            )

    async def _attachment_index(self) -> dict[int, str]:
        """Per-page dependents token from the instance-wide attachment listing."""
        entries: dict[int, list[str]] = {}
        async for attachment in self._client.list_attachments():
            page_id = attachment.get("uploaded_to")
            attachment_id = attachment.get("id")
            if not isinstance(page_id, int) or not isinstance(attachment_id, int):
                raise BookStackError(
                    "BookStack attachment index record lacks integer id/uploaded_to"
                )
            updated_at = attachment.get("updated_at")
            entries.setdefault(page_id, []).append(f"{attachment_id}:{updated_at}")
        return {page_id: dependents_token_from(lines) for page_id, lines in entries.items()}

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        if ref.kind != KIND_PAGE:
            log.warning("bookstack.fetch.unexpected_kind", kind=ref.kind)
            return SourceFetchDeferred(f"unexpected ref kind: {ref.kind}")
        try:
            page_id = int(ref.item_id)
        except ValueError:
            return SourceFetchDeferred("BookStack page id is not an integer")
        try:
            content_bytes, mime_type = await self._client.export_page(
                page_id, self._settings.bookstack_page_format
            )
        except BookStackPageNotFoundError:
            return SourceFetchDelete(
                SourceDeleteEnforcement.POLICY,
                "BookStack page no longer exists",
                cacheable=False,
            )

        slug = ref.raw.get("slug") if isinstance(ref.raw.get("slug"), str) else f"page-{page_id}"
        page_name = ref.raw.get("name") if isinstance(ref.raw.get("name"), str) else None
        extension = FORMAT_EXTENSION[self._settings.bookstack_page_format]
        file_name = f"{slug}.{extension}"

        # Inline images and attachments are pushed as dependents in the
        # same orchestrator pass. The tuple is the page's complete dependent
        # set, so any dependent that could not be read defers the page.
        dependents: list[SourceItem] = []
        if mime_type == "text/markdown":
            images = await self._build_image_dependents(page_id, page_name, content_bytes, ref)
            if isinstance(images, SourceFetchDeferred):
                return images
            dependents.extend(images)
        attachments = await self._build_attachment_dependents(page_id, page_name, ref)
        if isinstance(attachments, SourceFetchDeferred):
            return attachments
        dependents.extend(attachments)

        return SourceFetchMaterialized(
            SourceItem(
                ref=ref,
                payload=content_bytes,
                mime_type=mime_type,
                file_name=file_name,
                title=page_name,
                document_hint=self._page_document_hint(page_id, ref),
                dependents=tuple(dependents),
            )
        )

    async def _routing_facts_for_page(
        self, page_id: int, summary: dict[str, Any]
    ) -> dict[str, Any]:
        try:
            ctx = await self._context_builder.build(page_id)
        except BookStackPageNotFoundError:
            return {}
        return {
            "page_id": ctx.page_id,
            "page_slug": ctx.page_slug,
            "name": ctx.page_name,
            "book_id": ctx.book_id,
            "book": ctx.book_slug,
            "chapter_id": ctx.chapter_id,
            "chapter": ctx.chapter_slug,
            "shelf_ids": list(ctx.shelf_ids),
            "shelf": list(ctx.shelf_slugs),
            "tag": list(ctx.tags),
        }

    async def _routing_facts_for_summary(
        self, page_id: int, summary: dict[str, Any]
    ) -> dict[str, Any]:
        facts = _summary_routing_facts(page_id, summary)
        if not _needs_full_context(self._routing_match_keys, facts):
            return facts
        full = await self._routing_facts_for_page(page_id, summary)
        return {**facts, **full}

    async def _build_image_dependents(
        self,
        page_id: int,
        page_name: str | None,
        content_bytes: bytes,
        parent: SourceItemRef,
    ) -> list[SourceItem] | SourceFetchDeferred:
        try:
            text = content_bytes.decode("utf-8", errors="replace")
            urls = await collect_bookstack_image_urls(text, self._browser_base_url)
        except Exception as exc:
            log.warning(
                "bookstack.fetch.collect_images_failed",
                page_id=page_id,
                error=str(exc),
            )
            return SourceFetchDeferred(f"inline image references unreadable: {exc}")

        items: list[SourceItem] = []
        for url in urls:
            try:
                img_bytes, mime = await self._client.download_image(url)
            except Exception as exc:
                log.warning(
                    "bookstack.fetch.image_download_failed",
                    page_id=page_id,
                    url=url,
                    error=str(exc),
                )
                return SourceFetchDeferred(f"inline image {url} unreadable: {exc}")
            if img_bytes == b"" and mime == "":
                # BookStack itself answered 404 for the upload: the image is
                # gone from this page, and its absence is proven.
                continue
            if not img_bytes or not mime.startswith("image/"):
                # A 2xx that is not an image (an interstitial, a login page, a
                # proxy error body) says nothing about the image existing, so
                # it must not enter the census as an absence.
                log.warning(
                    "bookstack.fetch.image_not_an_image",
                    page_id=page_id,
                    url=url,
                    mime_type=mime,
                    byte_count=len(img_bytes),
                )
                return SourceFetchDeferred(f"inline image {url} did not resolve to an image")
            digest = hashlib.sha256(img_bytes).hexdigest()
            file_name = url.rsplit("/", 1)[-1] or f"image-{digest[:16]}"
            ext_key = image_external_key(digest)
            ref = SourceItemRef(
                item_id=digest[:16],
                kind=KIND_IMAGE,
                external_key=ext_key,
                change_token=digest,
                routing_facts=parent.routing_facts,
                raw={"page_id": page_id, "url": url},
            )
            items.append(
                SourceItem(
                    ref=ref,
                    payload=img_bytes,
                    mime_type=mime,
                    file_name=file_name,
                    title=f"{page_name or 'page'}: {file_name}",
                    idempotency_key=f"bookstack:image:{digest[:16]}",
                    document_hint=url,
                )
            )
        return items

    async def _build_attachment_dependents(
        self,
        page_id: int,
        page_name: str | None,
        parent: SourceItemRef,
    ) -> list[SourceItem] | SourceFetchDeferred:
        try:
            attachments = await self._client.list_page_attachments(page_id)
        except Exception as exc:
            log.warning(
                "bookstack.fetch.list_attachments_failed",
                page_id=page_id,
                error=str(exc),
            )
            return SourceFetchDeferred(f"attachment list unreadable: {exc}")

        items: list[SourceItem] = []
        for att in attachments:
            att_id = att.get("id")
            if not isinstance(att_id, int) or att_id <= 0:
                return SourceFetchDeferred("attachment record without a valid id")
            try:
                att_bytes, att_mime, att_filename = await self._client.get_attachment_payload(
                    att_id
                )
            except Exception as exc:
                # Includes the attachment vanishing between the list and the
                # detail read: the listed set is no longer this fetch's set.
                log.warning(
                    "bookstack.fetch.attachment_download_failed",
                    page_id=page_id,
                    attachment_id=att_id,
                    error=str(exc),
                )
                return SourceFetchDeferred(f"attachment {att_id} unreadable: {exc}")
            if not att_bytes:
                continue
            updated_at = att.get("updated_at") if isinstance(att.get("updated_at"), str) else None
            ref = SourceItemRef(
                item_id=str(att_id),
                kind=KIND_ATTACHMENT,
                external_key=attachment_external_key(att_id),
                change_token=updated_at,
                routing_facts=parent.routing_facts,
                raw={"page_id": page_id, "name": att.get("name")},
            )
            att_name = att.get("name") or f"attachment-{att_id}"
            items.append(
                SourceItem(
                    ref=ref,
                    payload=att_bytes,
                    mime_type=att_mime,
                    file_name=att_filename,
                    title=f"{page_name or 'Page'}: {att_name}",
                    document_hint=f"{self._browser_base_url}/attachments/{att_id}",
                )
            )
        return items

    def _page_document_hint(self, page_id: int, ref: SourceItemRef) -> str:
        page_slug = _string_fact(ref.raw, "page_slug", "slug") or _string_fact(
            ref.routing_facts, "page_slug"
        )
        book_slug = _string_fact(ref.raw, "book_slug", "book") or _string_fact(
            ref.routing_facts, "book_slug", "book"
        )
        if page_slug and book_slug:
            return f"{self._browser_base_url}/books/{book_slug}/page/{page_slug}"
        return f"{self._browser_base_url}/pages/{page_id}"


def _string_fact(source: dict[str, Any], *keys: str) -> str | None:
    for key in keys:
        value = source.get(key)
        if isinstance(value, str) and value:
            return value
    return None


def _load_routing_match_keys(path: Path) -> set[str] | None:
    try:
        config = load_routing_config(path)
    except Exception as exc:
        log.warning(
            "bookstack.routing.inspect_failed",
            path=str(path),
            error_type=type(exc).__name__,
            error=str(exc) or repr(exc),
        )
        # Preserve the pre-0.0.8 behaviour when the adapter cannot inspect
        # routing before the framework loads it.
        return None
    return {key for rule in config.rules for key in rule.match}


def _summary_routing_facts(page_id: int, summary: dict[str, Any]) -> dict[str, Any]:
    facts: dict[str, Any] = {"page_id": page_id}
    page_slug = _string_fact(summary, "page_slug", "slug")
    if page_slug:
        facts["page_slug"] = page_slug
    page_name = _string_fact(summary, "name")
    if page_name:
        facts["name"] = page_name
    book_id = summary.get("book_id")
    if isinstance(book_id, int):
        facts["book_id"] = book_id
    book_slug = _string_fact(summary, "book_slug", "book")
    if book_slug:
        facts["book"] = book_slug
    chapter_id = summary.get("chapter_id")
    if isinstance(chapter_id, int):
        facts["chapter_id"] = chapter_id
    chapter_slug = _string_fact(summary, "chapter_slug", "chapter")
    if chapter_slug:
        facts["chapter"] = chapter_slug
    tags = _collect_tag_names(summary.get("tags"))
    if tags:
        facts["tag"] = tags
    return facts


def _needs_full_context(
    routing_match_keys: set[str] | None,
    facts: dict[str, Any],
) -> bool:
    if routing_match_keys is None:
        return True
    if routing_match_keys & FULL_CONTEXT_ROUTING_KEYS:
        return True
    for key in routing_match_keys:
        value = facts.get(key)
        if value is None or value == [] or value == ():
            return True
    return False


def _collect_tag_names(raw: Any) -> list[str]:
    if not isinstance(raw, list):
        return []
    names: list[str] = []
    for item in raw:
        if isinstance(item, dict) and isinstance(item.get("name"), str):
            names.append(item["name"])
        elif isinstance(item, str):
            names.append(item)
    return names
