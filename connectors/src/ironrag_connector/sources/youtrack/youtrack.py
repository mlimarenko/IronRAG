from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import quote

import httpx

from ironrag_connector.http_retry import RequestThrottle, backoff_delay, retry_delay
from ironrag_connector.observability import get_logger

from .config import YouTrackSettings

log = get_logger(__name__)

RETRYABLE_STATUS = {429, 500, 502, 503, 504}

VISIBILITY_FIELDS = "$type"
USER_IDENTITY_FIELDS = "id,login"
PROJECT_FIELDS = "id,name,shortName,archived"
TAG_FIELDS = "id,name"
CUSTOM_FIELD_VALUE_FIELDS = "id,name,login"
CREDENTIAL_FIELDS = "id,login,guest"

ARTICLE_SUMMARY_FIELDS = ",".join(
    (
        "id",
        "idReadable",
        "summary",
        "created",
        "updated",
        f"project({PROJECT_FIELDS})",
        f"reporter({USER_IDENTITY_FIELDS})",
        f"updatedBy({USER_IDENTITY_FIELDS})",
        "parentArticle(id,idReadable)",
        f"tags({TAG_FIELDS})",
        f"visibility({VISIBILITY_FIELDS})",
    )
)
ARTICLE_DETAIL_FIELDS = f"{ARTICLE_SUMMARY_FIELDS},content"
ARTICLE_ANCESTOR_DESCRIPTOR_FIELDS = "id,parentArticle(id),visibility($type)"

# Project custom fields are not built-in issue properties: they only come
# back inside `customFields(...)`. The repeated top-level `customFields=`
# query parameters below are what restrict *which* custom fields are
# projected; the configured allowlist in YouTrackSettings is the sole source
# of truth. `fields=customFields(...)` alone would return every project field.

ISSUE_SUMMARY_BASE_FIELDS = ",".join(
    (
        "id",
        "idReadable",
        "summary",
        "created",
        "updated",
        "resolved",
        "commentsCount",
        f"project({PROJECT_FIELDS})",
        f"reporter({USER_IDENTITY_FIELDS})",
        f"updater({USER_IDENTITY_FIELDS})",
        f"tags({TAG_FIELDS})",
        f"visibility({VISIBILITY_FIELDS})",
    )
)
ISSUE_CUSTOM_FIELD_PROJECTION = f"customFields(name,$type,value({CUSTOM_FIELD_VALUE_FIELDS}))"
ISSUE_SUMMARY_FIELDS = ",".join((ISSUE_SUMMARY_BASE_FIELDS, ISSUE_CUSTOM_FIELD_PROJECTION))
ISSUE_DETAIL_FIELDS = f"{ISSUE_SUMMARY_FIELDS},description"

ISSUE_COMMENT_FIELDS = ",".join(
    (
        "id",
        "text",
        "created",
        "updated",
        "deleted",
        "pinned",
        f"author({USER_IDENTITY_FIELDS})",
        f"visibility({VISIBILITY_FIELDS})",
    )
)
ARTICLE_COMMENT_FIELDS = ",".join(
    (
        "id",
        "text",
        "created",
        "updated",
        "pinned",
        f"author({USER_IDENTITY_FIELDS})",
        f"visibility({VISIBILITY_FIELDS})",
    )
)
ATTACHMENT_FIELDS = ",".join(
    (
        "id",
        "name",
        "created",
        "updated",
        "size",
        "mimeType",
        "draft",
        "removed",
        "comment(id)",
        f"visibility({VISIBILITY_FIELDS})",
    )
)
ISSUE_LINK_FIELDS = ",".join(
    (
        "id",
        "direction",
        "linkType(id,name,sourceToTarget,targetToSource)",
        # The nested id is only an emptiness sentinel. Every target that is
        # consumed comes from the separately paginated link target endpoint.
        "issues(id)",
    )
)
ISSUE_LINK_TARGET_FIELDS = ",".join(
    (
        "id",
        "idReadable",
        f"project({PROJECT_FIELDS})",
        f"tags({TAG_FIELDS})",
        f"visibility({VISIBILITY_FIELDS})",
    )
)

# `/api/issues` defaults to `updated desc` when no sort is requested, so an
# ordinary comment or field edit on any issue reshuffles later offset pages
# mid-sweep. `created` prevents that movement, while `{issue id}` gives issues
# sharing a creation timestamp a deterministic total order.
ISSUE_LIST_QUERY = "sort by: created asc, {issue id} asc"


class YouTrackError(RuntimeError):
    """YouTrack request or response violated the connector contract."""


class YouTrackNotFoundError(YouTrackError):
    """An item disappeared between enumeration and materialization."""


class YouTrackClient:
    def __init__(
        self,
        settings: YouTrackSettings,
        client: httpx.AsyncClient | None = None,
    ) -> None:
        self._settings = settings
        self._owns_client = client is None
        self._service_url = settings.youtrack_base_url.rstrip("/")
        headers = {
            "Accept": "application/json",
            "Content-Type": "application/json",
            "Authorization": f"Bearer {settings.youtrack_token.get_secret_value()}",
        }
        if client is None:
            self._client = httpx.AsyncClient(
                timeout=settings.request_timeout_seconds,
                headers=headers,
                follow_redirects=True,
            )
        else:
            client.headers.update(headers)
            self._client = client
        self._indexed_custom_fields = settings.indexed_custom_fields()
        self._throttle = RequestThrottle(settings.youtrack_min_request_interval_seconds)

    async def aclose(self) -> None:
        if self._owns_client:
            await self._client.aclose()

    async def validate_credentials(self) -> dict[str, Any]:
        response = await self._request(
            "GET",
            self._api_url("users/me"),
            params={"fields": CREDENTIAL_FIELDS},
        )
        self._raise_for_status(response, operation="validate credentials")
        user = _json_object(response, operation="validate credentials")
        guest = user.get("guest")
        login = user.get("login")
        if not isinstance(guest, bool):
            raise YouTrackError("YouTrack credential response did not contain a boolean guest flag")
        if not isinstance(login, str) or not login:
            raise YouTrackError("YouTrack credential response did not contain a login")
        if guest or login == "guest":
            raise YouTrackError("YouTrack token resolved to the guest account")
        if not isinstance(user.get("id"), str) or not user["id"]:
            raise YouTrackError("YouTrack credential response did not contain a user id")
        return user

    async def list_articles(self) -> AsyncIterator[dict[str, Any]]:
        async for article in self._list_collection(
            resource="articles",
            fields=ARTICLE_SUMMARY_FIELDS,
            operation="list articles",
            item_name="article",
        ):
            yield article

    async def get_article(self, article_id: str) -> dict[str, Any]:
        response = await self._request(
            "GET",
            self._api_url(f"articles/{quote(article_id, safe='')}"),
            params={"fields": ARTICLE_DETAIL_FIELDS},
        )
        if response.status_code == 404:
            raise YouTrackNotFoundError(f"article {article_id} not found")
        self._raise_for_status(response, operation="get article")
        return _json_object(response, operation="get article")

    async def get_article_ancestor_descriptor(self, article_id: str) -> dict[str, Any]:
        response = await self._request(
            "GET",
            self._api_url(f"articles/{quote(article_id, safe='')}"),
            params={"fields": ARTICLE_ANCESTOR_DESCRIPTOR_FIELDS},
        )
        if response.status_code == 404:
            raise YouTrackNotFoundError(f"article {article_id} not found")
        self._raise_for_status(response, operation="get article ancestor descriptor")
        return _json_object(response, operation="get article ancestor descriptor")

    async def list_issues(self) -> AsyncIterator[dict[str, Any]]:
        extra_params: dict[str, Any] = {"query": ISSUE_LIST_QUERY}
        if self._indexed_custom_fields:
            extra_params["customFields"] = list(self._indexed_custom_fields)
        async for issue in self._list_collection(
            resource="issues",
            fields=self._issue_fields(detail=False),
            operation="list issues",
            item_name="issue",
            extra_params=extra_params,
        ):
            yield issue

    async def get_issue(self, issue_id: str) -> dict[str, Any]:
        params: dict[str, Any] = {"fields": self._issue_fields(detail=True)}
        if self._indexed_custom_fields:
            params["customFields"] = list(self._indexed_custom_fields)
        response = await self._request(
            "GET",
            self._api_url(f"issues/{quote(issue_id, safe='')}"),
            params=params,
        )
        if response.status_code == 404:
            raise YouTrackNotFoundError(f"issue {issue_id} not found")
        self._raise_for_status(response, operation="get issue")
        return _json_object(response, operation="get issue")

    def _issue_fields(self, *, detail: bool) -> str:
        if self._indexed_custom_fields:
            return ISSUE_DETAIL_FIELDS if detail else ISSUE_SUMMARY_FIELDS
        if detail:
            return f"{ISSUE_SUMMARY_BASE_FIELDS},description"
        return ISSUE_SUMMARY_BASE_FIELDS

    async def list_issue_comments(self, issue_id: str) -> AsyncIterator[dict[str, Any]]:
        resource = f"issues/{quote(issue_id, safe='')}/comments"
        async for comment in self._list_collection(
            resource=resource,
            fields=ISSUE_COMMENT_FIELDS,
            operation="list issue comments",
            item_name="issue comment",
        ):
            yield comment

    async def list_article_comments(self, article_id: str) -> AsyncIterator[dict[str, Any]]:
        resource = f"articles/{quote(article_id, safe='')}/comments"
        async for comment in self._list_collection(
            resource=resource,
            fields=ARTICLE_COMMENT_FIELDS,
            operation="list article comments",
            item_name="article comment",
        ):
            yield comment

    async def list_issue_attachments(self, issue_id: str) -> AsyncIterator[dict[str, Any]]:
        resource = f"issues/{quote(issue_id, safe='')}/attachments"
        async for attachment in self._list_collection(
            resource=resource,
            fields=ATTACHMENT_FIELDS,
            operation="list issue attachments",
            item_name="issue attachment",
        ):
            yield attachment

    async def list_article_attachments(self, article_id: str) -> AsyncIterator[dict[str, Any]]:
        resource = f"articles/{quote(article_id, safe='')}/attachments"
        async for attachment in self._list_collection(
            resource=resource,
            fields=ATTACHMENT_FIELDS,
            operation="list article attachments",
            item_name="article attachment",
        ):
            yield attachment

    async def list_issue_links(self, issue_id: str) -> AsyncIterator[dict[str, Any]]:
        resource = f"issues/{quote(issue_id, safe='')}/links"
        async for link in self._list_collection(
            resource=resource,
            fields=ISSUE_LINK_FIELDS,
            operation="list issue links",
            item_name="issue link",
        ):
            yield link

    async def list_issue_link_targets(
        self,
        issue_id: str,
        link_id: str,
    ) -> AsyncIterator[dict[str, Any]]:
        resource = f"issues/{quote(issue_id, safe='')}/links/{quote(link_id, safe='')}/issues"
        async for target in self._list_collection(
            resource=resource,
            fields=ISSUE_LINK_TARGET_FIELDS,
            operation="list issue link targets",
            item_name="issue link target",
        ):
            yield target

    async def _list_collection(
        self,
        *,
        resource: str,
        fields: str,
        operation: str,
        item_name: str,
        extra_params: dict[str, Any] | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        offset = 0
        seen_ids: set[str] = set()
        while True:
            params = dict(extra_params or {})
            params.update(
                {
                    "fields": fields,
                    "$top": self._settings.youtrack_page_size,
                    "$skip": offset,
                }
            )
            response = await self._request(
                "GET",
                self._api_url(resource),
                params=params,
            )
            self._raise_for_status(response, operation=operation)
            page = _json_array(response, operation=operation)
            if not page:
                return

            page_id_set = _validated_page_ids(page, seen_ids=seen_ids, item_name=item_name)

            seen_ids.update(page_id_set)
            for item in page:
                yield item
            next_offset = offset + len(page_id_set)
            if next_offset <= offset:  # pragma: no cover - guarded by a non-empty page
                raise YouTrackError(f"YouTrack {item_name} pagination made no progress")
            offset = next_offset

    async def _request(
        self,
        method: str,
        url: str,
        *,
        params: dict[str, Any] | None = None,
    ) -> httpx.Response:
        for attempt in range(self._settings.youtrack_retry_max + 1):
            async with self._throttle:
                try:
                    response = await self._client.request(method, url, params=params)
                except httpx.RequestError as transport_error:
                    request_error: httpx.RequestError | None = transport_error
                else:
                    request_error = None
            try:
                if request_error is not None:
                    raise request_error
            except httpx.RequestError as exc:
                if attempt >= self._settings.youtrack_retry_max:
                    raise YouTrackError(
                        f"YouTrack request failed after {attempt + 1} attempt(s)"
                    ) from exc
                delay = backoff_delay(
                    attempt,
                    backoff_seconds=self._settings.youtrack_retry_backoff_seconds,
                    max_sleep_seconds=self._settings.youtrack_retry_max_sleep_seconds,
                )
                log.warning(
                    "youtrack.retry",
                    reason="transport_error",
                    error_type=type(exc).__name__,
                    attempt=attempt,
                    delay_seconds=delay,
                )
                await asyncio.sleep(delay)
                continue

            if response.status_code not in RETRYABLE_STATUS:
                return response
            if attempt >= self._settings.youtrack_retry_max:
                return response
            delay = retry_delay(
                response.headers,
                attempt,
                backoff_seconds=self._settings.youtrack_retry_backoff_seconds,
                max_sleep_seconds=self._settings.youtrack_retry_max_sleep_seconds,
            )
            log.warning(
                "youtrack.retry",
                status=response.status_code,
                attempt=attempt,
                delay_seconds=delay,
            )
            await asyncio.sleep(delay)

        raise AssertionError("retry loop must return or raise")  # pragma: no cover

    def _api_url(self, resource: str) -> str:
        return f"{self._service_url}/api/{resource.lstrip('/')}"

    @staticmethod
    def _raise_for_status(response: httpx.Response, *, operation: str) -> None:
        if response.status_code >= 400:
            raise YouTrackError(f"YouTrack {operation} failed with HTTP {response.status_code}")


def _json_object(response: httpx.Response, *, operation: str) -> dict[str, Any]:
    try:
        payload = response.json()
    except ValueError as exc:
        raise YouTrackError(f"YouTrack {operation} returned invalid JSON") from exc
    if not isinstance(payload, dict):
        raise YouTrackError(f"YouTrack {operation} did not return an object")
    return payload


def _json_array(response: httpx.Response, *, operation: str) -> list[dict[str, Any]]:
    try:
        payload = response.json()
    except ValueError as exc:
        raise YouTrackError(f"YouTrack {operation} returned invalid JSON") from exc
    if not isinstance(payload, list):
        raise YouTrackError(f"YouTrack {operation} did not return a collection")
    if any(not isinstance(item, dict) for item in payload):
        raise YouTrackError(f"YouTrack {operation} returned a malformed collection")
    return payload


def _validated_page_ids(
    page: list[dict[str, Any]],
    *,
    seen_ids: set[str],
    item_name: str,
) -> set[str]:
    page_ids: set[str] = set()
    duplicate_ids: set[str] = set()
    for item in page:
        item_id = item.get("id")
        if not isinstance(item_id, str) or not item_id:
            raise YouTrackError(f"YouTrack {item_name} collection contained an item without id")
        if item_id in seen_ids or item_id in page_ids:
            duplicate_ids.add(item_id)
        page_ids.add(item_id)
    if not page_ids.difference(seen_ids):
        raise YouTrackError(f"YouTrack {item_name} pagination made no progress")
    if duplicate_ids:
        raise YouTrackError(
            f"YouTrack {item_name} pagination returned a duplicate {item_name} id; retry next sweep"
        )
    return page_ids
