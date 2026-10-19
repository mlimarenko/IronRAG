"""Async BookStack REST client.

Surface used by the connector:

* `get_page_metadata(page_id)`         → page JSON (name, slug, book_id, chapter_id, tags, ...)
* `get_book_metadata(book_id)`         → book JSON (slug, name, tags, ...)
* `get_chapter_metadata(chapter_id)`   → chapter JSON
* `export_page(page_id, fmt)`          → raw bytes + canonical MIME
* `list_pages()`                       → iterable over all pages
* `list_shelves()`                     → iterable over all shelves
* `list_attachments()`                 → iterable over every attachment (the
                                          per-sweep dependent census index)
* `get_shelf_metadata(shelf_id)`       → shelf JSON with `books: [...]` array

Every read, dependent downloads included, goes through the throttled and
retried ``_request`` path; a dependent read that still fails raises
:class:`BookStackError` so the adapter can defer the parent instead of
pretending the dependent does not exist.

Auth and retry semantics match `~/sources/bookstack.exporter`.
"""

from __future__ import annotations

import asyncio
import base64
import mimetypes
from collections.abc import AsyncIterator, Mapping
from typing import Any, cast

import httpx

from ironrag_connector.download_policy import (
    CrossOriginDownloadError,
    DownloadPolicyError,
    fetch_within_origin,
)
from ironrag_connector.http_retry import RequestThrottle, retry_delay
from ironrag_connector.observability import get_logger

from .config import BookStackSettings, ExportFormat

log = get_logger(__name__)

FORMAT_MIME = {
    "markdown": "text/markdown",
    "html": "text/html",
    "plaintext": "text/plain",
    "pdf": "application/pdf",
}

FORMAT_EXTENSION = {
    "markdown": "md",
    "html": "html",
    "plaintext": "txt",
    "pdf": "pdf",
}

RETRYABLE_STATUS = {429, 502, 503, 504}


class BookStackError(RuntimeError):
    """BookStack API surfaced an error we cannot retry past."""


class BookStackPageNotFoundError(BookStackError):
    """A specific page does not exist (HTTP 404)."""


def _collection_payload(response: httpx.Response, *, path: str) -> tuple[list[dict[str, Any]], int]:
    try:
        payload = response.json()
    except ValueError as exc:
        raise BookStackError(f"BookStack {path} returned malformed collection JSON") from exc
    if not isinstance(payload, Mapping) or "data" not in payload or "total" not in payload:
        raise BookStackError(f"BookStack {path} returned malformed collection shape")
    raw_items = payload["data"]
    total = payload["total"]
    if not isinstance(raw_items, list) or any(not isinstance(item, dict) for item in raw_items):
        raise BookStackError(f"BookStack {path} returned malformed collection data")
    if any(
        isinstance(item.get("id"), bool) or not isinstance(item.get("id"), int) or item["id"] <= 0
        for item in raw_items
    ):
        raise BookStackError(f"BookStack {path} returned malformed collection item id")
    if isinstance(total, bool) or not isinstance(total, int) or total < 0:
        raise BookStackError(f"BookStack {path} returned malformed collection total")
    return cast(list[dict[str, Any]], raw_items), total


def _next_collection_offset(
    *,
    path: str,
    offset: int,
    page_size: int,
    item_count: int,
    total: int,
) -> int | None:
    next_offset = offset + item_count
    if next_offset > total:
        raise BookStackError(f"BookStack {path} violated pagination contract: page exceeds total")
    if next_offset < total and item_count < page_size:
        raise BookStackError(
            f"BookStack {path} violated pagination contract: short or empty page before total"
        )
    return None if next_offset == total else next_offset


class BookStackClient:
    def __init__(
        self, settings: BookStackSettings, client: httpx.AsyncClient | None = None
    ) -> None:
        self._settings = settings
        self._owns_client = client is None
        self._base_url = self._normalise_base_url(settings.bookstack_base_url)
        self._client = client or httpx.AsyncClient(
            base_url=self._base_url,
            timeout=settings.request_timeout_seconds,
            headers={
                "Authorization": (
                    f"Token {settings.bookstack_token_id}:{settings.bookstack_token_secret}"
                ),
                "Accept": "application/json",
            },
        )
        # Rate-limit gate: a minimum interval between outbound request starts;
        # concurrent callers queue for distinct start times, then overlap.
        self._throttle = RequestThrottle(settings.bookstack_min_request_interval_seconds)

    @staticmethod
    def _normalise_base_url(base_url: str) -> str:
        base_url = base_url.rstrip("/")
        if base_url.endswith("/api"):
            return base_url[:-4]
        return base_url

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()

    async def __aenter__(self) -> BookStackClient:
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.aclose()

    async def _request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        accept: str = "application/json",
        follow_redirects: bool | None = None,
    ) -> httpx.Response:
        attempt = 0
        while True:
            # Rate-limit gate: the throttle spaces request starts; the exchange
            # itself runs outside the lock, so concurrent callers overlap.
            async with self._throttle:
                response = await self._client.request(
                    method,
                    path,
                    params=params,
                    headers={"Accept": accept},
                    follow_redirects=(
                        httpx.USE_CLIENT_DEFAULT if follow_redirects is None else follow_redirects
                    ),
                )

            retryable = (
                response.status_code in RETRYABLE_STATUS
                and attempt < self._settings.bookstack_retry_max
            )
            if retryable:
                delay = self._retry_delay(response.headers, attempt)
                log.warning(
                    "bookstack.retry",
                    status=response.status_code,
                    attempt=attempt,
                    delay_seconds=delay,
                    path=path,
                )
                await asyncio.sleep(delay)
                attempt += 1
                continue
            return response

    def _retry_delay(self, headers: httpx.Headers, attempt: int) -> float:
        return retry_delay(
            headers,
            attempt,
            backoff_seconds=self._settings.bookstack_retry_backoff_seconds,
            max_sleep_seconds=self._settings.bookstack_retry_max_sleep_seconds,
        )

    async def get_page_metadata(self, page_id: int) -> dict[str, Any]:
        return await self._get_json(f"/api/pages/{page_id}", entity=f"page {page_id}")

    async def get_book_metadata(self, book_id: int) -> dict[str, Any]:
        return await self._get_json(f"/api/books/{book_id}", entity=f"book {book_id}")

    async def get_chapter_metadata(self, chapter_id: int) -> dict[str, Any]:
        return await self._get_json(f"/api/chapters/{chapter_id}", entity=f"chapter {chapter_id}")

    async def get_shelf_metadata(self, shelf_id: int) -> dict[str, Any]:
        return await self._get_json(f"/api/shelves/{shelf_id}", entity=f"shelf {shelf_id}")

    async def _get_json(self, path: str, *, entity: str) -> dict[str, Any]:
        response = await self._request("GET", path)
        if response.status_code == 404:
            raise BookStackPageNotFoundError(f"BookStack {entity} not found")
        if response.status_code >= 400:
            raise BookStackError(
                f"BookStack {path} → {response.status_code}: {response.text[:400]}"
            )
        return cast(dict[str, Any], response.json())

    async def export_page(self, page_id: int, fmt: ExportFormat) -> tuple[bytes, str]:
        response = await self._request(
            "GET",
            f"/api/pages/{page_id}/export/{fmt}",
            accept=FORMAT_MIME[fmt],
        )
        if response.status_code == 404:
            raise BookStackPageNotFoundError(f"BookStack page {page_id} not found during export")
        if response.status_code >= 400:
            raise BookStackError(
                f"BookStack export {fmt} for page {page_id} → {response.status_code}: "
                f"{response.text[:400]}"
            )
        return response.content, FORMAT_MIME[fmt]

    async def list_pages(self, page_size: int = 100) -> AsyncIterator[dict[str, Any]]:
        async for item in self._iterate("/api/pages", page_size):
            yield item

    async def list_shelves(self, page_size: int = 100) -> AsyncIterator[dict[str, Any]]:
        async for item in self._iterate("/api/shelves", page_size):
            yield item

    async def list_attachments(self, page_size: int = 100) -> AsyncIterator[dict[str, Any]]:
        """Every attachment in the instance; ``uploaded_to`` names the owning page."""
        async for item in self._iterate("/api/attachments", page_size):
            yield item

    async def list_page_attachments(self, page_id: int) -> list[dict[str, Any]]:
        """All attachments belonging to one page via /api/attachments?filter[uploaded_to]=<id>."""
        results: list[dict[str, Any]] = []
        async for item in self._iterate(
            "/api/attachments", 100, extra_params={"filter[uploaded_to]": str(page_id)}
        ):
            results.append(item)
        return results

    async def get_attachment_payload(self, attachment_id: int) -> tuple[bytes, str, str]:
        """Return (bytes, mime_type, file_name) for a single attachment.

        BookStack returns base64 in ``content`` for non-external attachments.
        For external attachments (URL pointer), download from the URL.
        MIME type is guessed from the file extension.
        """
        detail = await self._get_json(
            f"/api/attachments/{attachment_id}",
            entity=f"attachment {attachment_id}",
        )
        name: str = detail.get("name") or f"attachment-{attachment_id}"
        extension: str = detail.get("extension") or ""
        file_name = name if "." in name else (f"{name}.{extension}" if extension else name)

        mime_type, _ = mimetypes.guess_type(file_name)
        if not mime_type:
            mime_type = "application/octet-stream"

        is_external: bool = bool(detail.get("external", False))
        content_field = detail.get("content") or ""

        if is_external:
            # External attachments store a URL in content. It is a link an
            # author pasted, so it is fetched only when it stays on the
            # BookStack origin; anything else is a link to the outside world,
            # not source content, and must not receive this client's
            # credentials (IRG-96). Whatever the link's target answers is a
            # fact about that URL, never BookStack's verdict that the
            # attachment is gone: a 404 or a 5xx there is a read failure, so
            # it defers the page instead of proving an absence that would
            # delete the stored copy (IRG-95).
            url: str = content_field if isinstance(content_field, str) else ""
            if not url:
                return b"", mime_type, file_name
            try:
                response = await self._download(url)
            except CrossOriginDownloadError as exc:
                log.info(
                    "bookstack.attachment.external_link_skipped",
                    attachment_id=attachment_id,
                    reason=str(exc),
                )
                return b"", mime_type, file_name
            except DownloadPolicyError as exc:
                raise BookStackError(
                    f"External attachment {attachment_id} download refused: {exc}"
                ) from exc
            if response.status_code >= 400:
                raise BookStackError(
                    f"External attachment {attachment_id} download → "
                    f"{response.status_code}: {response.text[:400]}"
                )
            return response.content, mime_type, file_name

        # Non-external: content is base64-encoded file bytes. Undecodable
        # content is a read failure, not an empty attachment.
        if not isinstance(content_field, str) or not content_field:
            return b"", mime_type, file_name
        try:
            raw_bytes = base64.b64decode(content_field, validate=True)
        except (ValueError, TypeError) as exc:
            raise BookStackError(
                f"BookStack attachment {attachment_id} content is not valid base64: {exc}"
            ) from exc
        return raw_bytes, mime_type, file_name

    async def download_image(self, image_url: str) -> tuple[bytes, str]:
        """Fetch a BookStack /uploads/... image using the same Authorization header.

        Returns (bytes, mime_type).  On 404 returns (b"", "") as a soft miss.
        """
        try:
            response = await self._download(image_url)
        except DownloadPolicyError as exc:
            raise BookStackError(f"BookStack image download {image_url} refused: {exc}") from exc
        if response.status_code == 404:
            log.debug("bookstack.download_image.not_found", url=image_url)
            return b"", ""
        if response.status_code >= 400:
            raise BookStackError(
                f"BookStack image download {image_url} → {response.status_code}: "
                f"{response.text[:200]}"
            )
        content_type = response.headers.get("content-type", "")
        mime_type = content_type.split(";")[0].strip() or "application/octet-stream"
        return response.content, mime_type

    async def _download(self, url: str) -> httpx.Response:
        """GET a URL taken from source data, held to the BookStack origin.

        Goes through `_request` so downloads share the throttle and retry of
        every other call; redirects are followed by the policy, hop by hop.
        """
        return await fetch_within_origin(
            self._client,
            self._base_url,
            url,
            send=lambda target: self._request("GET", target, accept="*/*", follow_redirects=False),
        )

    async def _iterate(
        self,
        path: str,
        page_size: int,
        *,
        extra_params: dict[str, str] | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        if page_size < 1:
            raise ValueError("BookStack collection page_size must be positive")
        offset = 0
        expected_total: int | None = None
        while True:
            params: dict[str, Any] = {"count": page_size, "offset": offset}
            if extra_params:
                params.update(extra_params)
            response = await self._request("GET", path, params=params)
            if response.status_code >= 400:
                raise BookStackError(
                    f"BookStack {path} → {response.status_code}: {response.text[:400]}"
                )
            items, total = _collection_payload(response, path=path)
            if expected_total is None:
                expected_total = total
            elif total != expected_total:
                raise BookStackError(
                    f"BookStack {path} violated pagination contract: total changed "
                    f"from {expected_total} to {total}"
                )
            next_offset = _next_collection_offset(
                path=path,
                offset=offset,
                page_size=page_size,
                item_count=len(items),
                total=total,
            )
            for item in items:
                yield item
            if next_offset is None:
                return
            offset = next_offset
