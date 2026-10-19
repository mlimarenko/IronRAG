from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from dataclasses import dataclass
from typing import Any

import httpx

from ironrag_connector.download_policy import DownloadPolicyError, fetch_within_origin
from ironrag_connector.http_retry import RequestThrottle, retry_delay
from ironrag_connector.observability import get_logger

from .config import ConfluenceSettings

log = get_logger(__name__)

RETRYABLE_STATUS = {429, 502, 503, 504}


class ConfluenceError(RuntimeError):
    pass


class ConfluenceNotFoundError(ConfluenceError):
    pass


class ConfluenceClient:
    def __init__(
        self, settings: ConfluenceSettings, client: httpx.AsyncClient | None = None
    ) -> None:
        self._settings = settings
        self._owns_client = client is None
        headers = {"Accept": "application/json"}
        auth, auth_headers = _build_auth(settings)
        headers.update(auth_headers)
        if client is None:
            self._client = httpx.AsyncClient(
                base_url=settings.confluence_base_url.rstrip("/"),
                timeout=settings.request_timeout_seconds,
                headers=headers,
                auth=auth,
                follow_redirects=True,
            )
        else:
            client.headers.update(headers)
            if auth is not None:
                client.auth = auth
            self._client = client
        self._throttle = RequestThrottle(settings.confluence_min_request_interval_seconds)

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()

    async def _request(
        self,
        method: str,
        path: str,
        *,
        params: dict[str, Any] | None = None,
        headers: dict[str, str] | None = None,
        follow_redirects: bool | None = None,
    ) -> httpx.Response:
        attempt = 0
        while True:
            async with self._throttle:
                response = await self._client.request(
                    method,
                    path,
                    params=params,
                    headers=headers,
                    follow_redirects=(
                        httpx.USE_CLIENT_DEFAULT if follow_redirects is None else follow_redirects
                    ),
                )

            if (
                response.status_code in RETRYABLE_STATUS
                and attempt < self._settings.confluence_retry_max
            ):
                delay = retry_delay(
                    response.headers,
                    attempt,
                    backoff_seconds=self._settings.confluence_retry_backoff_seconds,
                    max_sleep_seconds=self._settings.confluence_retry_max_sleep_seconds,
                )
                log.warning(
                    "confluence.retry",
                    status=response.status_code,
                    attempt=attempt,
                    delay_seconds=delay,
                    path=path,
                )
                await asyncio.sleep(delay)
                attempt += 1
                continue
            return response

    async def list_pages(self) -> AsyncIterator[dict[str, Any]]:
        async for item in self._search(
            cql=self._build_cql("page", include_custom=True),
            resource="page",
            expand="version,space,history,ancestors",
        ):
            yield item

    async def list_attachment_index(self) -> AsyncIterator[dict[str, Any]]:
        """Every attachment in scope with its ``container`` (owning page) and ``version``.

        One CQL walk per sweep; the adapter folds it into each page's
        ``dependents_token``. The custom page CQL is not applied: it narrows
        pages, and attachment membership must follow the page it belongs to
        regardless of how that page was selected.
        """
        async for item in self._search(
            cql=self._build_cql("attachment", include_custom=False),
            resource="attachment",
            expand="container,version",
        ):
            container = item.get("container")
            container_id = container.get("id") if isinstance(container, dict) else None
            if not isinstance(container_id, str) or not container_id.strip():
                raise ConfluenceError(
                    f"Confluence attachment {item['id']} index record lacks a container id"
                )
            yield item

    async def _search(
        self, *, cql: str, resource: str, expand: str
    ) -> AsyncIterator[dict[str, Any]]:
        start = 0
        seen_ids: set[str] = set()
        limit = self._settings.confluence_page_size
        skipped = 0
        while True:
            items, next_start, window_skips = await self._fetch_window(
                cql,
                resource=resource,
                expand=expand,
                start=start,
                limit=limit,
                seen_ids=seen_ids,
            )
            skipped += window_skips
            if skipped > _POISON_SKIP_BUDGET * 4:
                raise ConfluenceError(
                    f"Confluence {resource} search skipped more than "
                    f"{_POISON_SKIP_BUDGET * 4} unmaterializable offsets; "
                    f"the instance cannot be walked"
                )
            if next_start is None:
                return
            for item in items:
                yield item
            start = next_start

    async def _fetch_window(
        self,
        cql: str,
        *,
        resource: str,
        expand: str,
        start: int,
        limit: int,
        seen_ids: set[str],
    ) -> tuple[list[dict[str, Any]], int | None, int]:
        """Fetch the `[start, start + limit)` window, skipping poison offsets.

        The instance refuses any window that contains an entity it cannot
        materialize (IRG-120) and holds such entities at more than one
        offset, so the window re-isolates and skips until the rest of it is
        fetchable. `None` as the next start marks the explicit empty page
        that ends the walk; a failure a single offset cannot explain is
        escalated instead of skipped, because that is an outage, not poison.
        """
        items: list[dict[str, Any]] = []
        cursor = start
        end = start + limit
        # Every iteration either returns or skips exactly one poison offset,
        # so the iteration index is the number of skips this window made.
        for skipped in range(_POISON_SKIP_BUDGET):
            if cursor >= end:
                return items, end, skipped
            response = await self._search_window(
                cql, start=cursor, limit=end - cursor, expand=expand
            )
            if not _is_unmaterializable_window(response):
                return _window_from_page(
                    response,
                    resource=resource,
                    cursor=cursor,
                    start=start,
                    end=end,
                    skipped=skipped,
                    items=items,
                    seen_ids=seen_ids,
                )
            poison = await self._isolate_unmaterializable_offset(
                cql, resource=resource, expand=expand, start=cursor, limit=end - cursor
            )
            if poison is None or poison < cursor:
                raise _search_failure(resource, response)
            await self._collect_prefix(
                cql,
                resource=resource,
                expand=expand,
                cursor=cursor,
                poison=poison,
                seen_ids=seen_ids,
                items=items,
            )
            cursor = poison + 1
        raise ConfluenceError(
            f"Confluence {resource} search could not get past poison offsets "
            f"within {_POISON_SKIP_BUDGET} attempts at start {start}"
        )

    async def _collect_prefix(
        self,
        cql: str,
        *,
        resource: str,
        expand: str,
        cursor: int,
        poison: int,
        seen_ids: set[str],
        items: list[dict[str, Any]],
    ) -> None:
        """Append the fetchable part of the window before a poison offset."""
        if poison <= cursor:
            return
        prefix = await self._search_window(cql, start=cursor, limit=poison - cursor, expand=expand)
        if prefix.status_code >= 400:
            return
        prefix_page = _collection_page(
            prefix,
            resource=resource,
            start=cursor,
            seen_ids=seen_ids,
        )
        items.extend(prefix_page.items)

    async def _search_window(
        self, cql: str, *, start: int, limit: int, expand: str
    ) -> httpx.Response:
        return await self._request(
            "GET",
            "/rest/api/content/search",
            params={
                "cql": cql,
                "start": start,
                "limit": limit,
                "expand": expand,
            },
        )

    async def _isolate_unmaterializable_offset(
        self, cql: str, *, resource: str, expand: str, start: int, limit: int
    ) -> int | None:
        """Bisect a failing window down to the one offset that reproduces it.

        Returns `None` when a single offset does not reproduce the failure:
        a real outage must escalate, never be skipped as a poison entity.
        The instance may hold the entity at a different offset between
        requests; every failing window simply bisects again.
        """
        low, high = start, start + limit
        while high - low > 1:
            probe_start = (low + high) // 2
            response = await self._search_window(
                cql, start=probe_start, limit=high - probe_start, expand=expand
            )
            if _is_unmaterializable_window(response):
                low = probe_start
            else:
                high = probe_start
        single = await self._search_window(cql, start=low, limit=1, expand=expand)
        if not _is_unmaterializable_window(single):
            return None
        log.warning(
            "confluence.unmaterializable_entity_skipped",
            offset=low,
            resource=resource,
        )
        return low

    async def get_page(self, page_id: str) -> dict[str, Any]:
        response = await self._request(
            "GET",
            f"/rest/api/content/{page_id}",
            params={
                "expand": ("body.export_view,body.storage,version,space,metadata.labels,ancestors")
            },
        )
        if response.status_code == 404:
            raise ConfluenceNotFoundError(f"page {page_id} not found")
        if response.status_code >= 400:
            raise ConfluenceError(
                f"Confluence page fetch failed: {response.status_code} {response.text[:400]}"
            )
        payload = _response_object(response, resource="page")
        returned_page_id = payload.get("id")
        if not isinstance(returned_page_id, str) or not returned_page_id.strip():
            raise ConfluenceError("Confluence page id must be a non-empty string")
        return payload

    async def list_attachments(self, page_id: str) -> list[dict[str, Any]]:
        attachments: list[dict[str, Any]] = []
        start = 0
        seen_ids: set[str] = set()
        while True:
            response = await self._request(
                "GET",
                f"/rest/api/content/{page_id}/child/attachment",
                params={
                    "start": start,
                    "limit": self._settings.confluence_page_size,
                    "expand": "version,metadata,space",
                },
            )
            if response.status_code == 404:
                raise ConfluenceError(
                    f"Confluence attachment collection changed while reading page {page_id}"
                )
            if response.status_code >= 400:
                raise ConfluenceError(
                    "Confluence attachment list failed: "
                    f"{response.status_code} {response.text[:400]}"
                )
            page = _collection_page(
                response,
                resource="attachment",
                start=start,
                seen_ids=seen_ids,
                require_download_link=True,
            )
            if page.next_start is None:
                return attachments
            attachments.extend(page.items)
            # Window advance, exactly as in `_search`: `start` indexes the
            # server's result set, not the rows this client kept.
            start += self._settings.confluence_page_size

    async def download(self, path_or_url: str) -> tuple[bytes, str]:
        # The download link is source data; it and every redirect it leads to
        # must stay on the Confluence origin, or the credentials this client
        # carries would go with the request (IRG-96).
        try:
            response = await fetch_within_origin(
                self._client,
                self._settings.confluence_base_url,
                path_or_url,
                send=lambda url: self._request("GET", url, follow_redirects=False),
            )
        except DownloadPolicyError as exc:
            raise ConfluenceError(f"Confluence download refused: {exc}") from exc
        if response.status_code == 404:
            raise ConfluenceNotFoundError(f"download target not found: {path_or_url}")
        if response.status_code >= 400:
            raise ConfluenceError(
                f"Confluence download failed: {response.status_code} {response.text[:400]}"
            )
        mime = response.headers.get("content-type", "application/octet-stream")
        return response.content, mime.split(";")[0].strip()

    def _build_cql(self, content_type: str, *, include_custom: bool) -> str:
        clauses: list[str] = [f"type={content_type}"]

        included = self._settings.included_space_keys()
        if included is not None:
            clauses.append(f"space in ({_quote_csv(included)})")

        excluded = self._settings.excluded_space_keys()
        if excluded:
            clauses.append(f"space not in ({_quote_csv(excluded)})")

        custom = (self._settings.confluence_cql or "").strip()
        if include_custom and custom:
            clauses.append(f"({custom})")

        # `order by id asc` on its own makes the source instance materialize
        # entities it cannot convert when the walk also expands `container`
        # (`docs.artix.su` answers 400 "globaldescription ... is not
        # ContentConvertible"), which aborted every attachment sweep. Leading
        # with `created` keeps the walk deterministic -- `id` stays as the
        # tie-break -- and the same instance answers it with 200.
        return f"{' and '.join(clauses)} order by created asc, id asc"


def _response_object(response: httpx.Response, *, resource: str) -> dict[str, Any]:
    try:
        payload = response.json()
    except ValueError as exc:
        raise ConfluenceError(f"Confluence {resource} response was not valid JSON") from exc
    if not isinstance(payload, dict):
        raise ConfluenceError(f"Confluence {resource} response must be an object")
    return payload


@dataclass(frozen=True)
class _CollectionPage:
    items: tuple[dict[str, Any], ...]
    next_start: int | None


def _validated_collection_item(
    item: Any,
    *,
    resource: str,
    index: int,
    require_download_link: bool,
) -> tuple[dict[str, Any], str]:
    if not isinstance(item, dict):
        raise ConfluenceError(f"Confluence {resource} collection element {index} must be an object")
    item_id = item.get("id")
    if not isinstance(item_id, str) or not item_id.strip():
        raise ConfluenceError(
            f"Confluence {resource} collection element {index} id must be a non-empty string"
        )
    if require_download_link:
        links = item.get("_links")
        download = links.get("download") if isinstance(links, dict) else None
        if not isinstance(download, str) or not download.strip():
            raise ConfluenceError(
                f"Confluence {resource} collection element {index} download link "
                "must be a non-empty string"
            )
    return item, item_id


# The instance answers this for any search window that contains an entity
# whose content type it cannot convert (IRG-120). The entity is not part of
# the content this connector syncs; the walk isolates and skips exactly it.
UNMATERIALIZABLE_ENTITY_MARKER = "is not ContentConvertible or API available"
# How many poison offsets one window may isolate and skip before the walk
# gives up: a shifting index is expected, an endless one is an outage.
_POISON_SKIP_BUDGET = 8


def _is_unmaterializable_window(response: httpx.Response) -> bool:
    return response.status_code == 400 and UNMATERIALIZABLE_ENTITY_MARKER in response.text


def _search_failure(resource: str, response: httpx.Response) -> ConfluenceError:
    return ConfluenceError(
        f"Confluence {resource} search failed: {response.status_code} {response.text[:400]}"
    )


def _window_from_page(
    response: httpx.Response,
    *,
    resource: str,
    cursor: int,
    start: int,
    end: int,
    skipped: int,
    items: list[dict[str, Any]],
    seen_ids: set[str],
) -> tuple[list[dict[str, Any]], int | None, int]:
    if response.status_code >= 400:
        raise _search_failure(resource, response)
    page = _collection_page(response, resource=resource, start=cursor, seen_ids=seen_ids)
    items.extend(page.items)
    if not page.items and cursor == start:
        return items, None, skipped
    return items, end, skipped


def _collection_page(
    response: httpx.Response,
    *,
    resource: str,
    start: int,
    seen_ids: set[str],
    require_download_link: bool = False,
) -> _CollectionPage:
    payload = _response_object(response, resource=f"{resource} collection")
    results = payload.get("results")
    if not isinstance(results, list):
        raise ConfluenceError(f"Confluence {resource} collection results must be a list")

    validated: list[dict[str, Any]] = []
    page_ids: set[str] = set()
    for index, item in enumerate(results):
        validated_item, item_id = _validated_collection_item(
            item,
            resource=resource,
            index=index,
            require_download_link=require_download_link,
        )
        if item_id in seen_ids or item_id in page_ids:
            raise ConfluenceError(
                f"Confluence {resource} collection returned duplicate id {item_id!r}"
            )
        page_ids.add(item_id)
        validated.append(validated_item)

    if not validated:
        return _CollectionPage((), None)
    next_start = start + len(validated)
    if next_start <= start:
        raise ConfluenceError(f"Confluence {resource} collection pagination made no progress")
    seen_ids.update(page_ids)
    return _CollectionPage(tuple(validated), next_start)


def _build_auth(
    settings: ConfluenceSettings,
) -> tuple[httpx.Auth | None, dict[str, str]]:
    mode = settings.confluence_auth_mode
    headers: dict[str, str] = {}

    if mode == "auto":
        if settings.confluence_username and (
            settings.confluence_password or settings.confluence_token
        ):
            mode = "basic"
        elif settings.confluence_token:
            mode = "bearer"
        else:
            mode = "anonymous"

    if mode == "anonymous":
        return None, headers
    if mode == "bearer":
        if not settings.confluence_token:
            raise ValueError("CONFLUENCE_TOKEN is required for bearer auth mode")
        headers["Authorization"] = f"Bearer {settings.confluence_token}"
        return None, headers
    if mode == "basic":
        if not settings.confluence_username:
            raise ValueError("CONFLUENCE_USERNAME is required for basic auth mode")
        secret = settings.confluence_password or settings.confluence_token
        if not secret:
            raise ValueError(
                "CONFLUENCE_PASSWORD or CONFLUENCE_TOKEN is required for basic auth mode"
            )
        return httpx.BasicAuth(settings.confluence_username, secret), headers

    raise ValueError(f"unsupported auth mode: {mode}")


def _quote_csv(values: tuple[str, ...]) -> str:
    escaped = [value.replace('"', '\\"') for value in values]
    return ", ".join(f'"{value}"' for value in escaped)
