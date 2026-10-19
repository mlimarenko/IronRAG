"""Build the rich `PageContext` consumed by the routing layer.

A BookStack page's identity for routing purposes spans:

    page → chapter (optional) → book → 0..N shelves → tags

BookStack does not expose a direct `book → shelves` lookup; we derive it by
periodically materialising the `/api/shelves` listing into a reverse map.
The cache TTL is configurable; a stale-cache miss falls through to a fresh
shelf fetch so a freshly-created shelf does not silently break routing.
"""

from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Any

from ironrag_connector.observability import get_logger

from .bookstack import BookStackClient, BookStackPageNotFoundError
from .config import BookStackSettings

log = get_logger(__name__)


@dataclass(frozen=True)
class PageContext:
    """Everything routing rules can match against for a given page event."""

    page_id: int
    page_slug: str | None
    page_name: str | None
    book_id: int | None
    book_slug: str | None
    chapter_id: int | None
    chapter_slug: str | None
    shelf_ids: tuple[int, ...]
    shelf_slugs: tuple[str, ...]
    tags: tuple[str, ...]


@dataclass
class _ShelfRecord:
    shelf_id: int
    shelf_slug: str
    shelf_name: str | None
    book_ids: tuple[int, ...]


class ShelfIndex:
    """Reverse index: `book_id → list[shelves]`, refreshed lazily under TTL."""

    def __init__(self, client: BookStackClient, ttl_seconds: int) -> None:
        self._client = client
        self._ttl = ttl_seconds
        self._refreshed_at: float = 0.0
        self._book_to_shelves: dict[int, list[_ShelfRecord]] = {}

    def is_stale(self, *, now: float | None = None) -> bool:
        timestamp = now if now is not None else time.monotonic()
        return (timestamp - self._refreshed_at) >= self._ttl

    async def refresh(self) -> None:
        log.info("shelf_index.refresh.start")
        new_map: dict[int, list[_ShelfRecord]] = {}
        async for shelf_summary in self._client.list_shelves():
            shelf_id = shelf_summary.get("id")
            if not isinstance(shelf_id, int):
                continue
            try:
                shelf_full = await self._client.get_shelf_metadata(shelf_id)
            except BookStackPageNotFoundError:
                continue
            books = shelf_full.get("books") or []
            book_ids = tuple(b["id"] for b in books if isinstance(b.get("id"), int))
            record = _ShelfRecord(
                shelf_id=shelf_id,
                shelf_slug=shelf_full.get("slug") or "",
                shelf_name=shelf_full.get("name"),
                book_ids=book_ids,
            )
            for book_id in book_ids:
                new_map.setdefault(book_id, []).append(record)
        self._book_to_shelves = new_map
        self._refreshed_at = time.monotonic()
        log.info("shelf_index.refresh.done", shelf_books=len(new_map))

    async def shelves_for_book(self, book_id: int) -> list[_ShelfRecord]:
        if self.is_stale():
            await self.refresh()
        records = self._book_to_shelves.get(book_id)
        if records is not None:
            return records
        # Cache miss for a fresh book: refresh once more, then accept the answer.
        await self.refresh()
        return self._book_to_shelves.get(book_id, [])


class PageContextBuilder:
    """Assembles `PageContext` from BookStack metadata + shelf index."""

    def __init__(
        self,
        settings: BookStackSettings,
        client: BookStackClient,
        shelves: ShelfIndex,
        *,
        needs_shelves: bool,
        needs_tags: bool,
    ) -> None:
        self._settings = settings
        self._client = client
        self._shelves = shelves
        self._needs_shelves = needs_shelves
        self._needs_tags = needs_tags

    async def build(self, page_id: int) -> PageContext:
        page = await self._client.get_page_metadata(page_id)
        page_slug = page.get("slug") if isinstance(page.get("slug"), str) else None
        page_name = page.get("name") if isinstance(page.get("name"), str) else None
        book_id = page.get("book_id") if isinstance(page.get("book_id"), int) else None
        chapter_id = page.get("chapter_id") if isinstance(page.get("chapter_id"), int) else None
        book_slug = page.get("book_slug") if isinstance(page.get("book_slug"), str) else None
        chapter_slug = (
            page.get("chapter_slug") if isinstance(page.get("chapter_slug"), str) else None
        )

        if (book_slug is None or chapter_slug is None) and book_id:
            book_slug, chapter_slug = await self._resolve_book_and_chapter_slugs(
                book_id, chapter_id, book_slug, chapter_slug
            )

        shelf_ids: tuple[int, ...] = ()
        shelf_slugs: tuple[str, ...] = ()
        if self._needs_shelves and book_id:
            shelves = await self._shelves.shelves_for_book(book_id)
            shelf_ids = tuple(s.shelf_id for s in shelves)
            shelf_slugs = tuple(s.shelf_slug for s in shelves if s.shelf_slug)

        tags: tuple[str, ...] = ()
        if self._needs_tags:
            tags = self._collect_tags(page)
            if not tags and book_id:
                tags = await self._fallback_book_tags(book_id)

        return PageContext(
            page_id=page_id,
            page_slug=page_slug,
            page_name=page_name,
            book_id=book_id,
            book_slug=book_slug,
            chapter_id=chapter_id,
            chapter_slug=chapter_slug,
            shelf_ids=shelf_ids,
            shelf_slugs=shelf_slugs,
            tags=tags,
        )

    async def _resolve_book_and_chapter_slugs(
        self,
        book_id: int,
        chapter_id: int | None,
        book_slug: str | None,
        chapter_slug: str | None,
    ) -> tuple[str | None, str | None]:
        try:
            if book_slug is None:
                book = await self._client.get_book_metadata(book_id)
                if isinstance(book.get("slug"), str):
                    book_slug = book["slug"]
            if chapter_slug is None and chapter_id is not None:
                chapter = await self._client.get_chapter_metadata(chapter_id)
                if isinstance(chapter.get("slug"), str):
                    chapter_slug = chapter["slug"]
        except BookStackPageNotFoundError:
            pass
        return book_slug, chapter_slug

    @staticmethod
    def _collect_tags(entity: dict[str, Any]) -> tuple[str, ...]:
        raw = entity.get("tags") or []
        return tuple(t["name"] for t in raw if isinstance(t.get("name"), str))

    async def _fallback_book_tags(self, book_id: int) -> tuple[str, ...]:
        try:
            book = await self._client.get_book_metadata(book_id)
        except BookStackPageNotFoundError:
            return ()
        return self._collect_tags(book)
