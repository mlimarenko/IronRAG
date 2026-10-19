from __future__ import annotations

from unittest.mock import AsyncMock, patch

import httpx
import pytest

from ironrag_connector.sources.youtrack.config import YouTrackSettings
from ironrag_connector.sources.youtrack.youtrack import (
    YouTrackClient,
    YouTrackError,
    YouTrackNotFoundError,
)

RELATED_COLLECTION_CASES = (
    (
        "list_issue_comments",
        ("issue/1",),
        b"/youtrack/api/issues/issue%2F1/comments",
        frozenset(
            {
                "id",
                "text",
                "created",
                "updated",
                "deleted",
                "pinned",
                "author(id,login)",
                "visibility($type)",
            }
        ),
    ),
    (
        "list_article_comments",
        ("article/1",),
        b"/youtrack/api/articles/article%2F1/comments",
        frozenset(
            {
                "id",
                "text",
                "created",
                "updated",
                "pinned",
                "author(id,login)",
                "visibility($type)",
            }
        ),
    ),
    (
        "list_issue_attachments",
        ("issue/1",),
        b"/youtrack/api/issues/issue%2F1/attachments",
        frozenset(
            {
                "id",
                "name",
                "created",
                "updated",
                "size",
                "mimeType",
                "draft",
                "removed",
                "comment(id)",
                "visibility($type)",
            }
        ),
    ),
    (
        "list_article_attachments",
        ("article/1",),
        b"/youtrack/api/articles/article%2F1/attachments",
        frozenset(
            {
                "id",
                "name",
                "created",
                "updated",
                "size",
                "mimeType",
                "draft",
                "removed",
                "comment(id)",
                "visibility($type)",
            }
        ),
    ),
    (
        "list_issue_links",
        ("issue/1",),
        b"/youtrack/api/issues/issue%2F1/links",
        frozenset(
            {
                "id",
                "direction",
                "linkType(id,name,sourceToTarget,targetToSource)",
                "issues(id)",
            }
        ),
    ),
    (
        "list_issue_link_targets",
        ("issue/1", "link/1"),
        b"/youtrack/api/issues/issue%2F1/links/link%2F1/issues",
        frozenset(
            {
                "id",
                "idReadable",
                "project(id,name,shortName,archived)",
                "tags(id,name)",
                "visibility($type)",
            }
        ),
    ),
)


def _settings(**overrides: object) -> YouTrackSettings:
    data: dict[str, object] = {
        "youtrack_base_url": "https://tracker.example.com/youtrack",
        "youtrack_token": "perm:synthetic-token",
        "youtrack_page_size": 2,
        "youtrack_min_request_interval_seconds": 0,
        "ironrag_base_url": "https://ironrag.example.com",
        "ironrag_api_token": "synthetic-ironrag-token",
        "admin_bearer_token": "synthetic-admin-token",
    }
    data.update(overrides)
    return YouTrackSettings(**data)


def _top_level_fields(projection: str) -> frozenset[str]:
    fields: list[str] = []
    depth = 0
    start = 0
    for index, character in enumerate(projection):
        if character == "(":
            depth += 1
        elif character == ")":
            depth -= 1
        elif character == "," and depth == 0:
            fields.append(projection[start:index])
            start = index + 1
    fields.append(projection[start:])
    return frozenset(fields)


@pytest.mark.asyncio
async def test_list_articles_paginates_and_preserves_context_path() -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "226-0"}, {"id": "226-1"}])
        if skip == 2:
            return httpx.Response(200, json=[{"id": "226-2"}])
        if skip == 3:
            return httpx.Response(200, json=[])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(_settings(), client=http)

    articles = [article async for article in api.list_articles()]

    assert [article["id"] for article in articles] == ["226-0", "226-1", "226-2"]
    assert [request.url.path for request in requests] == [
        "/youtrack/api/articles",
        "/youtrack/api/articles",
        "/youtrack/api/articles",
    ]
    assert requests[0].url.params["$top"] == "2"
    assert requests[0].url.params["$skip"] == "0"
    assert "content" not in requests[0].url.params["fields"]
    assert "attachments(" not in requests[0].url.params["fields"]
    assert "visibility($type)" in requests[0].url.params["fields"]
    assert "parentArticle(id,idReadable)" in requests[0].url.params["fields"]
    assert requests[0].headers["Authorization"] == "Bearer perm:synthetic-token"

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_get_article_requests_full_content() -> None:
    captured_fields = ""

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal captured_fields
        captured_fields = request.url.params.get("fields", "")
        return httpx.Response(
            200,
            json={
                "id": "226-0",
                "idReadable": "DOCS-A-1",
                "summary": "Overview",
                "content": "Body",
            },
        )

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(_settings(), client=http)

    article = await api.get_article("226-0")

    assert article["content"] == "Body"
    assert "content" in captured_fields.split(",")
    assert "visibility($type)" in captured_fields
    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_get_article_ancestor_descriptor_uses_exact_projection_and_url() -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        return httpx.Response(
            200,
            json={
                "id": "226/0",
                "parentArticle": {"id": "226-1"},
                "visibility": {"$type": "UnlimitedVisibility"},
            },
        )

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(_settings(), client=http)

    descriptor = await api.get_article_ancestor_descriptor("226/0")

    assert descriptor["parentArticle"] == {"id": "226-1"}
    assert len(requests) == 1
    assert requests[0].method == "GET"
    assert requests[0].url.raw_path.split(b"?", 1)[0] == b"/youtrack/api/articles/226%2F0"
    assert requests[0].url.params == httpx.QueryParams(
        {"fields": "id,parentArticle(id),visibility($type)"}
    )
    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_get_article_ancestor_descriptor_maps_404_to_soft_not_found() -> None:
    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(lambda _: httpx.Response(404)),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackNotFoundError, match=r"article 226/404 not found"):
        await api.get_article_ancestor_descriptor("226/404")

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_retry_after_is_honoured_for_rate_limit() -> None:
    attempts = 0

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal attempts
        attempts += 1
        if attempts == 1:
            return httpx.Response(429, headers={"Retry-After": "3"})
        return httpx.Response(200, json=[])

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(
        _settings(youtrack_retry_max=1, youtrack_retry_max_sleep_seconds=10),
        client=http,
    )

    sleep_target = "ironrag_connector.sources.youtrack.youtrack.asyncio.sleep"
    with patch(sleep_target, new=AsyncMock()) as sleep:
        assert [article async for article in api.list_articles()] == []

    sleep.assert_awaited_once_with(3.0)
    assert attempts == 2
    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_get_article_maps_404_to_soft_not_found() -> None:
    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(lambda _: httpx.Response(404)),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackNotFoundError):
        await api.get_article("226-404")

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_api_errors_do_not_expose_token() -> None:
    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(lambda _: httpx.Response(401, text="denied")),
    )
    api = YouTrackClient(_settings(youtrack_token="perm:must-not-leak"), client=http)

    with pytest.raises(YouTrackError) as raised:
        _ = [article async for article in api.list_articles()]

    assert "must-not-leak" not in str(raised.value)
    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_invalid_collection_payload_fails_closed() -> None:
    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(lambda _: httpx.Response(200, json={"id": "bad"})),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="collection"):
        _ = [article async for article in api.list_articles()]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_duplicate_id_during_offset_pagination_fails_closed() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "226-0"}, {"id": "226-1"}])
        if skip == 2:
            return httpx.Response(200, json=[{"id": "226-1"}, {"id": "226-2"}])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="duplicate article id"):
        _ = [article async for article in api.list_articles()]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_list_issues_paginates_with_stable_sort_and_field_projection() -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "3-0"}, {"id": "3-1"}])
        if skip == 2:
            return httpx.Response(200, json=[{"id": "3-2"}])
        if skip == 3:
            return httpx.Response(200, json=[])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(_settings(), client=http)

    issues = [issue async for issue in api.list_issues()]

    assert [issue["id"] for issue in issues] == ["3-0", "3-1", "3-2"]
    assert [request.url.path for request in requests] == [
        "/youtrack/api/issues",
        "/youtrack/api/issues",
        "/youtrack/api/issues",
    ]
    # Default `updated desc` ordering reshuffles offset pages on every live
    # edit; the connector needs a deterministic tie-breaker when two issues
    # share the same creation timestamp.
    assert [request.url.params["query"] for request in requests] == [
        "sort by: created asc, {issue id} asc",
        "sort by: created asc, {issue id} asc",
        "sort by: created asc, {issue id} asc",
    ]
    assert [request.url.params["$skip"] for request in requests] == ["0", "2", "3"]
    assert all(request.url.params["$top"] == "2" for request in requests)
    assert "description" not in requests[0].url.params["fields"]
    assert "commentsCount" in _top_level_fields(requests[0].url.params["fields"])
    assert "visibility($type)" in requests[0].url.params["fields"]
    assert requests[0].url.params.get_list("customFields") == [
        "Type",
        "State",
        "Priority",
        "Assignee",
    ]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_get_issue_requests_full_description_and_customfields() -> None:
    captured_fields = ""
    captured_custom_fields: list[str] = []

    def handler(request: httpx.Request) -> httpx.Response:
        nonlocal captured_fields, captured_custom_fields
        captured_fields = request.url.params.get("fields", "")
        captured_custom_fields = request.url.params.get_list("customFields")
        return httpx.Response(
            200, json={"id": "3-0", "idReadable": "IRG-83", "description": "Body"}
        )

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(_settings(), client=http)

    issue = await api.get_issue("3-0")

    assert issue["description"] == "Body"
    assert "description" in captured_fields.split(",")
    assert "commentsCount" in _top_level_fields(captured_fields)
    assert "visibility($type)" in captured_fields
    assert captured_custom_fields == ["Type", "State", "Priority", "Assignee"]
    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_issue_requests_use_configured_custom_field_allowlist() -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.url.path.endswith("/issues"):
            return httpx.Response(200, json=[])
        return httpx.Response(200, json={"id": "3-0"})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = YouTrackClient(
        _settings(youtrack_custom_fields="Product / Area,Type"),
        client=http,
    )

    assert [issue async for issue in api.list_issues()] == []
    assert await api.get_issue("3-0") == {"id": "3-0"}

    assert len(requests) == 2
    for request in requests:
        assert request.url.params.get_list("customFields") == ["Product / Area", "Type"]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_empty_custom_field_allowlist_does_not_request_all_project_fields() -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        if request.url.path.endswith("/issues"):
            return httpx.Response(200, json=[])
        return httpx.Response(200, json={"id": "3-0"})

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = YouTrackClient(_settings(youtrack_custom_fields=""), client=http)

    assert [issue async for issue in api.list_issues()] == []
    assert await api.get_issue("3-0") == {"id": "3-0"}

    assert len(requests) == 2
    for request in requests:
        assert request.url.params.get_list("customFields") == []
        assert "customFields(" not in request.url.params["fields"]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_get_issue_maps_404_to_soft_not_found() -> None:
    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(lambda _: httpx.Response(404)),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackNotFoundError):
        await api.get_issue("3-404")

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_duplicate_id_during_issue_pagination_fails_closed() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "3-0"}, {"id": "3-1"}])
        if skip == 2:
            return httpx.Response(200, json=[{"id": "3-1"}, {"id": "3-2"}])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="duplicate issue id"):
        _ = [issue async for issue in api.list_issues()]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_guest_identity_is_rejected() -> None:
    http = httpx.AsyncClient(
        transport=httpx.MockTransport(
            lambda _: httpx.Response(200, json={"id": "1-0", "login": "guest", "guest": True})
        ),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="guest"):
        await api.validate_credentials()

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "payload",
    [
        pytest.param({"id": "1-2", "login": "writer"}, id="missing-guest"),
        pytest.param(
            {"id": "1-2", "login": "writer", "guest": "false"},
            id="non-boolean-guest",
        ),
        pytest.param({"id": "1-2", "guest": False}, id="missing-login"),
        pytest.param({"id": "1-2", "login": "", "guest": False}, id="empty-login"),
        pytest.param({"id": "1-2", "login": 7, "guest": False}, id="non-string-login"),
    ],
)
async def test_incomplete_credential_identity_is_rejected(
    payload: dict[str, object],
) -> None:
    http = httpx.AsyncClient(
        transport=httpx.MockTransport(lambda _: httpx.Response(200, json=payload)),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match=r"guest flag|login"):
        await api.validate_credentials()

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("method_name", "arguments", "expected_raw_path", "expected_fields"),
    RELATED_COLLECTION_CASES,
)
async def test_related_collections_are_fully_paginated_with_least_data_projection(
    method_name: str,
    arguments: tuple[str, ...],
    expected_raw_path: bytes,
    expected_fields: frozenset[str],
) -> None:
    requests: list[httpx.Request] = []

    def handler(request: httpx.Request) -> httpx.Response:
        requests.append(request)
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "item-0"}, {"id": "item-1"}])
        if skip == 2:
            # A short page is not assumed to be terminal: the API must return
            # an explicit empty page before the client stops the collection.
            return httpx.Response(200, json=[{"id": "item-2"}])
        if skip == 3:
            return httpx.Response(200, json=[])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    api = YouTrackClient(_settings(), client=http)

    collection = getattr(api, method_name)(*arguments)
    items = [item async for item in collection]

    assert [item["id"] for item in items] == ["item-0", "item-1", "item-2"]
    assert len(requests) == 3
    assert all(request.url.raw_path.split(b"?", 1)[0] == expected_raw_path for request in requests)
    assert [request.url.params["$skip"] for request in requests] == ["0", "2", "3"]
    assert all(request.url.params["$top"] == "2" for request in requests)
    assert all(
        _top_level_fields(request.url.params["fields"]) == expected_fields for request in requests
    )
    projected_fields = requests[0].url.params["fields"].lower()
    for forbidden_field in (
        "email",
        "url",
        "base64content",
        "reactions",
        "watchers",
        "voters",
        "textpreview",
    ):
        assert forbidden_field not in projected_fields

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_related_collection_partial_duplicate_id_fails_closed() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "comment-0"}, {"id": "comment-1"}])
        if skip == 2:
            return httpx.Response(200, json=[{"id": "comment-1"}, {"id": "comment-2"}])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="duplicate issue comment id"):
        _ = [comment async for comment in api.list_issue_comments("3-0")]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_related_collection_all_duplicate_page_fails_as_no_progress() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        skip = int(request.url.params.get("$skip", "0"))
        if skip == 0:
            return httpx.Response(200, json=[{"id": "attachment-0"}, {"id": "attachment-1"}])
        if skip == 2:
            return httpx.Response(200, json=[{"id": "attachment-0"}, {"id": "attachment-1"}])
        raise AssertionError(f"unexpected pagination offset {skip}")

    http = httpx.AsyncClient(transport=httpx.MockTransport(handler))
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="article attachment pagination made no progress"):
        _ = [attachment async for attachment in api.list_article_attachments("226-0")]

    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_related_collection_404_is_not_mapped_to_item_not_found() -> None:
    http = httpx.AsyncClient(
        transport=httpx.MockTransport(lambda _: httpx.Response(404)),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="list issue comments") as raised:
        _ = [comment async for comment in api.list_issue_comments("3-404")]

    assert not isinstance(raised.value, YouTrackNotFoundError)
    await api.aclose()
    await http.aclose()


@pytest.mark.asyncio
async def test_malformed_related_collection_error_does_not_expose_comment_body() -> None:
    http = httpx.AsyncClient(
        transport=httpx.MockTransport(
            lambda _: httpx.Response(200, json=[{"text": "must-not-leak-comment-body"}])
        ),
    )
    api = YouTrackClient(_settings(), client=http)

    with pytest.raises(YouTrackError, match="without id") as raised:
        _ = [comment async for comment in api.list_issue_comments("3-0")]

    assert "must-not-leak-comment-body" not in str(raised.value)
    await api.aclose()
    await http.aclose()
