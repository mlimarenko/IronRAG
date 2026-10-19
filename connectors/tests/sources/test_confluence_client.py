from __future__ import annotations

import httpx
import pytest

from ironrag_connector.sources.confluence.config import ConfluenceSettings
from ironrag_connector.sources.confluence.confluence import ConfluenceClient, ConfluenceError


def _settings(**overrides: object) -> ConfluenceSettings:
    data = {
        "confluence_base_url": "https://wiki.example.com",
        "confluence_auth_mode": "anonymous",
        "ironrag_base_url": "http://ironrag.example.com",
        "ironrag_api_token": "token",
        "admin_bearer_token": "admin",
    }
    data.update(overrides)
    return ConfluenceSettings(**data)


def _page(item_id: str) -> dict[str, object]:
    return {"id": item_id}


def _attachment(item_id: str) -> dict[str, object]:
    return {
        "id": item_id,
        "_links": {"download": f"/download/attachments/42/{item_id}"},
    }


def _collection_item(resource: str, item_id: str) -> dict[str, object]:
    return _page(item_id) if resource == "page" else _attachment(item_id)


async def _collect(api: ConfluenceClient, resource: str) -> list[dict[str, object]]:
    if resource == "page":
        return [item async for item in api.list_pages()]
    return await api.list_attachments("42")


@pytest.mark.asyncio
async def test_list_pages_uses_composed_cql() -> None:
    captured: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["cql"] = request.url.params.get("cql", "")
        results = [{"id": "1"}, {"id": "2"}]
        if request.url.params.get("start") != "0":
            results = []
        return httpx.Response(
            200,
            json={"results": results},
        )

    transport = httpx.MockTransport(handler)
    client = httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    )
    settings = _settings(
        confluence_space_keys="ENG,DOCS",
        confluence_exclude_space_keys="ARCHIVE",
        confluence_cql='label = "ops"',
    )
    api = ConfluenceClient(settings, client=client)
    items = [item async for item in api.list_pages()]
    assert [item["id"] for item in items] == ["1", "2"]
    assert 'space in ("ENG", "DOCS")' in captured["cql"]
    assert 'space not in ("ARCHIVE")' in captured["cql"]
    assert '(label = "ops")' in captured["cql"]
    await api.aclose()
    await client.aclose()


@pytest.mark.asyncio
async def test_auto_auth_uses_bearer_when_token_only() -> None:
    captured_auth: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured_auth["authorization"] = request.headers.get("authorization", "")
        return httpx.Response(200, json={"results": []})

    transport = httpx.MockTransport(handler)
    client = httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    )
    settings = _settings(confluence_auth_mode="auto", confluence_token="abc123")
    api = ConfluenceClient(settings, client=client)
    _ = [item async for item in api.list_pages()]
    assert captured_auth["authorization"] == "Bearer abc123"
    await api.aclose()
    await client.aclose()


@pytest.mark.asyncio
async def test_space_all_marker_disables_include_filter() -> None:
    captured: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["cql"] = request.url.params.get("cql", "")
        return httpx.Response(200, json={"results": []})

    transport = httpx.MockTransport(handler)
    client = httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    )
    settings = _settings(
        confluence_space_keys="all",
        confluence_exclude_space_keys="ARCHIVE",
    )
    api = ConfluenceClient(settings, client=client)
    _ = [item async for item in api.list_pages()]
    assert "space in (" not in captured["cql"]
    assert 'space not in ("ARCHIVE")' in captured["cql"]
    await api.aclose()
    await client.aclose()


@pytest.mark.asyncio
async def test_attachment_endpoint_404_is_an_incomplete_snapshot() -> None:
    transport = httpx.MockTransport(lambda _: httpx.Response(404))
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError, match="attachment collection changed"):
            await api.list_attachments("42")


@pytest.mark.asyncio
async def test_list_attachments_accepts_well_formed_empty_collection() -> None:
    transport = httpx.MockTransport(lambda _: httpx.Response(200, json={"results": []}))
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        assert await api.list_attachments("42") == []


@pytest.mark.asyncio
async def test_list_attachments_rejects_malformed_later_page_without_returning_partial() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.params.get("start") == "0":
            return httpx.Response(
                200,
                json={
                    "results": [
                        {
                            "id": "77",
                            "_links": {"download": "/download/attachments/42/guide.pdf"},
                        }
                    ]
                },
            )
        return httpx.Response(200, json={"results": {"id": "not-a-list"}})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=1), client=client)

        with pytest.raises(ConfluenceError, match="results must be a list"):
            await api.list_attachments("42")


@pytest.mark.asyncio
async def test_list_attachments_rejects_malformed_element() -> None:
    transport = httpx.MockTransport(
        lambda _: httpx.Response(
            200,
            json={
                "results": [
                    {
                        "id": "77",
                        "_links": {"download": "/download/attachments/42/guide.pdf"},
                    },
                    "not-an-object",
                ]
            },
        )
    )
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError, match=r"element 1 must be an object"):
            await api.list_attachments("42")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("element", "error"),
    [
        ({}, "id must be a non-empty string"),
        ({"id": "77"}, "download link must be a non-empty string"),
        ({"id": "77", "_links": {"download": " "}}, "download link"),
    ],
    ids=("missing-id", "missing-links", "blank-download-link"),
)
async def test_list_attachments_rejects_malformed_object_element(
    element: dict[str, object],
    error: str,
) -> None:
    transport = httpx.MockTransport(lambda _: httpx.Response(200, json={"results": [element]}))
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError, match=error):
            await api.list_attachments("42")


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "response",
    [
        httpx.Response(200, json=[]),
        httpx.Response(200, json={}),
        httpx.Response(200, json={"results": "not-a-list"}),
        httpx.Response(200, content=b"not-json"),
    ],
    ids=("non-object", "missing-results", "non-list-results", "invalid-json"),
)
async def test_list_pages_rejects_malformed_first_page(response: httpx.Response) -> None:
    transport = httpx.MockTransport(lambda _: response)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError):
            _ = [item async for item in api.list_pages()]


@pytest.mark.asyncio
async def test_list_pages_rejects_malformed_later_page() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        if request.url.params.get("start") == "0":
            return httpx.Response(200, json={"results": [{"id": "1"}]})
        return httpx.Response(200, json={"results": None})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=1), client=client)

        with pytest.raises(ConfluenceError, match="results must be a list"):
            _ = [item async for item in api.list_pages()]


@pytest.mark.asyncio
async def test_list_pages_rejects_non_object_row() -> None:
    transport = httpx.MockTransport(
        lambda _: httpx.Response(
            200,
            json={"results": [{"id": "1"}, "not-an-object"]},
        )
    )
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError, match=r"element 1 must be an object"):
            _ = [item async for item in api.list_pages()]


@pytest.mark.asyncio
async def test_list_pages_rejects_object_row_without_valid_id() -> None:
    transport = httpx.MockTransport(lambda _: httpx.Response(200, json={"results": [{"id": " "}]}))
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError, match="id must be a non-empty string"):
            _ = [item async for item in api.list_pages()]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "payload",
    [
        [],
        {},
        {"id": 42},
    ],
    ids=("non-object", "missing-id", "non-string-id"),
)
async def test_get_page_rejects_malformed_object(payload: object) -> None:
    transport = httpx.MockTransport(lambda _: httpx.Response(200, json=payload))
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(), client=client)

        with pytest.raises(ConfluenceError, match="Confluence page"):
            await api.get_page("42")


@pytest.mark.parametrize(
    ("resource", "ids"),
    [
        pytest.param("page", ("1", "2", "3"), id="pages"),
        pytest.param("attachment", ("77", "78", "79"), id="attachments"),
    ],
)
@pytest.mark.asyncio
async def test_collection_continues_after_short_page_until_explicit_empty(
    resource: str,
    ids: tuple[str, str, str],
) -> None:
    starts: list[int] = []
    pages = {
        0: [_collection_item(resource, ids[0]), _collection_item(resource, ids[1])],
        3: [_collection_item(resource, ids[2])],
        6: [],
    }

    def handler(request: httpx.Request) -> httpx.Response:
        start = int(request.url.params["start"])
        starts.append(start)
        return httpx.Response(200, json={"results": pages[start]})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=3), client=client)

        items = await _collect(api, resource)

    assert [item["id"] for item in items] == list(ids)
    # Windows advance by the requested page size: a page that returned fewer
    # items than its window covers must not shift the next window's start, or
    # the walk re-fetches the tail of the previous window.
    assert starts == [0, 3, 6]


@pytest.mark.parametrize(
    ("resource", "ids", "duplicate_indexes"),
    [
        pytest.param("page", ("1", "2"), (1,), id="pages-short"),
        pytest.param("page", ("1", "2"), (0, 1), id="pages-full"),
        pytest.param("attachment", ("77", "78"), (1,), id="attachments-short"),
        pytest.param("attachment", ("77", "78"), (0, 1), id="attachments-full"),
    ],
)
@pytest.mark.asyncio
async def test_collection_rejects_duplicate_or_repeated_page(
    resource: str,
    ids: tuple[str, str],
    duplicate_indexes: tuple[int, ...],
) -> None:
    first_page = [_collection_item(resource, item_id) for item_id in ids]
    duplicate_page = [first_page[index] for index in duplicate_indexes]

    def handler(request: httpx.Request) -> httpx.Response:
        start = int(request.url.params["start"])
        if start == 0:
            return httpx.Response(200, json={"results": first_page})
        if start == 2:
            return httpx.Response(200, json={"results": duplicate_page})
        return httpx.Response(200, json={"results": []})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=transport,
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=2), client=client)

        with pytest.raises(ConfluenceError, match="duplicate id"):
            await _collect(api, resource)


@pytest.mark.asyncio
async def test_list_attachment_index_walks_attachment_cql_with_container_expand() -> None:
    captured: dict[str, str] = {}

    def handler(request: httpx.Request) -> httpx.Response:
        captured["cql"] = request.url.params.get("cql", "")
        captured["expand"] = request.url.params.get("expand", "")
        results = [
            {"id": "77", "container": {"id": "42"}, "version": {"when": "w1"}},
            {"id": "78", "container": {"id": "43"}, "version": {"number": 3}},
        ]
        if request.url.params.get("start") != "0":
            results = []
        return httpx.Response(200, json={"results": results})

    transport = httpx.MockTransport(handler)
    client = httpx.AsyncClient(base_url="https://wiki.example.com", transport=transport)
    settings = _settings(
        confluence_space_keys="ENG",
        confluence_exclude_space_keys="ARCHIVE",
        confluence_cql='label = "ops"',
    )
    api = ConfluenceClient(settings, client=client)

    items = [item async for item in api.list_attachment_index()]

    assert [item["id"] for item in items] == ["77", "78"]
    assert captured["cql"].startswith("type=attachment")
    assert 'space in ("ENG")' in captured["cql"]
    assert 'space not in ("ARCHIVE")' in captured["cql"]
    assert "label" not in captured["cql"], "page-oriented custom CQL does not narrow attachments"
    assert "container" in captured["expand"].split(",")
    assert "version" in captured["expand"].split(",")
    await api.aclose()
    await client.aclose()


@pytest.mark.asyncio
async def test_list_attachment_index_avoids_the_order_by_id_container_server_bug() -> None:
    """The source instance rejects `order by id asc` combined with
    `expand=container` (IRG-120): it answers 400 "The entity globaldescription
    ... is not ContentConvertible". The attachment walk must not send that
    combination -- an ordering that leads with `created` is deterministic and
    accepted."""

    def handler(request: httpx.Request) -> httpx.Response:
        cql = request.url.params.get("cql", "")
        expand = request.url.params.get("expand", "")
        if cql.endswith("order by id asc") and "container" in expand.split(","):
            return httpx.Response(
                400,
                json={
                    "statusCode": 400,
                    "message": (
                        "The entity globaldescription:  v.1 (524290) is not "
                        "ContentConvertible or API available"
                    ),
                    "reason": "Bad Request",
                },
            )
        results = [{"id": "77", "container": {"id": "42"}, "version": {"when": "w1"}}]
        if request.url.params.get("start") != "0":
            results = []
        return httpx.Response(200, json={"results": results})

    transport = httpx.MockTransport(handler)
    client = httpx.AsyncClient(base_url="https://wiki.example.com", transport=transport)
    api = ConfluenceClient(_settings(confluence_space_keys="ENG"), client=client)

    items = [item async for item in api.list_attachment_index()]

    assert [item["id"] for item in items] == ["77"]
    await api.aclose()
    await client.aclose()


@pytest.mark.asyncio
async def test_list_attachment_index_skips_the_one_unmaterializable_offset() -> None:
    """The instance refuses any window containing an entity it cannot
    convert; the walk isolates that single offset, skips exactly it, and
    still enumerates every real attachment around it (IRG-120)."""
    poison = 3
    all_items = [
        {"id": str(100 + index), "container": {"id": "42"}, "version": {"when": "w"}}
        for index in range(6)
    ]

    def handler(request: httpx.Request) -> httpx.Response:
        start = int(request.url.params["start"])
        limit = int(request.url.params["limit"])
        if start <= poison < start + limit:
            return httpx.Response(
                400,
                json={
                    "statusCode": 400,
                    "message": (
                        "The entity globaldescription:  v.1 (524290) is not "
                        "ContentConvertible or API available"
                    ),
                },
            )
        return httpx.Response(200, json={"results": all_items[start : start + limit]})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com", transport=transport
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=4), client=client)
        items = [item async for item in api.list_attachment_index()]

    assert [item["id"] for item in items] == [
        "100",
        "101",
        "102",
        "104",
        "105",
    ]
    await api.aclose()


@pytest.mark.asyncio
async def test_search_escalates_a_failure_that_is_not_a_single_offset() -> None:
    """A 400 without the unmaterializable marker is an outage, not a poison
    entity: it must raise instead of being skipped."""

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(400, json={"statusCode": 400, "message": "Bad Request"})

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com", transport=transport
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=4), client=client)
        with pytest.raises(ConfluenceError, match="search failed"):
            [item async for item in api.list_attachment_index()]

    await api.aclose()


@pytest.mark.asyncio
async def test_search_gives_up_when_offsets_stay_unmaterializable() -> None:
    """An instance where every offset reproduces the poison marker is not
    walkable: the walk reports that instead of skipping forever."""

    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(
            400,
            json={
                "statusCode": 400,
                "message": (
                    "The entity globaldescription:  v.1 (524290) is not "
                    "ContentConvertible or API available"
                ),
            },
        )

    transport = httpx.MockTransport(handler)
    async with httpx.AsyncClient(
        base_url="https://wiki.example.com", transport=transport
    ) as client:
        api = ConfluenceClient(_settings(confluence_page_size=4), client=client)
        with pytest.raises(ConfluenceError, match="cannot be walked"):
            [item async for item in api.list_attachment_index()]

    await api.aclose()


@pytest.mark.asyncio
async def test_list_attachment_index_rejects_records_without_a_container() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        results = [{"id": "77", "version": {"when": "w1"}}]
        if request.url.params.get("start") != "0":
            results = []
        return httpx.Response(200, json={"results": results})

    async with httpx.AsyncClient(
        base_url="https://wiki.example.com", transport=httpx.MockTransport(handler)
    ) as client:
        api = ConfluenceClient(_settings(), client=client)
        with pytest.raises(ConfluenceError, match="container"):
            async for _ in api.list_attachment_index():
                pass


# --- downloads: vendor credentials stay on the Confluence origin (IRG-96) ---


class _RecordingTransport:
    def __init__(self, routes: dict[str, httpx.Response]) -> None:
        self.seen: list[tuple[str, str | None]] = []
        self._routes = routes

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append((str(request.url), request.headers.get("authorization")))
        return self._routes.get(str(request.url), httpx.Response(200, content=b"leaked"))

    @property
    def hosts(self) -> list[str]:
        return [httpx.URL(url).host for url, _ in self.seen]


def _download_client(transport: _RecordingTransport) -> httpx.AsyncClient:
    return httpx.AsyncClient(
        base_url="https://wiki.example.com",
        transport=httpx.MockTransport(transport),
    )


@pytest.mark.asyncio
async def test_download_rejects_another_origin_before_any_request() -> None:
    transport = _RecordingTransport({})
    async with _download_client(transport) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        with pytest.raises(ConfluenceError, match="origin"):
            await api.download("https://evil.example/download/attachments/42/x.pdf")

    assert transport.seen == []


@pytest.mark.asyncio
async def test_download_rejects_scheme_downgrade_of_the_same_host() -> None:
    transport = _RecordingTransport({})
    async with _download_client(transport) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        with pytest.raises(ConfluenceError, match="origin"):
            await api.download("http://wiki.example.com/download/attachments/42/x.pdf")

    assert transport.seen == []


@pytest.mark.asyncio
async def test_download_absolute_same_origin_link_carries_credentials() -> None:
    url = "https://wiki.example.com/download/attachments/42/x.pdf"
    transport = _RecordingTransport(
        {url: httpx.Response(200, content=b"pdf", headers={"content-type": "application/pdf"})}
    )
    async with _download_client(transport) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        assert await api.download(url) == (b"pdf", "application/pdf")

    assert transport.seen == [(url, "Bearer abc")]


@pytest.mark.asyncio
async def test_download_redirected_off_origin_is_not_followed() -> None:
    transport = _RecordingTransport(
        {
            "https://wiki.example.com/download/attachments/42/x.pdf": httpx.Response(
                302, headers={"location": "https://evil.example/x.pdf"}
            )
        }
    )
    async with _download_client(transport) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        with pytest.raises(ConfluenceError, match="origin"):
            await api.download("/download/attachments/42/x.pdf")

    assert transport.hosts == ["wiki.example.com"]


@pytest.mark.asyncio
async def test_download_follows_redirects_within_origin() -> None:
    transport = _RecordingTransport(
        {
            "https://wiki.example.com/download/attachments/42/x.pdf": httpx.Response(
                302, headers={"location": "/download/moved/x.pdf"}
            ),
            "https://wiki.example.com/download/moved/x.pdf": httpx.Response(
                307, headers={"location": "https://wiki.example.com/download/final/x.pdf"}
            ),
            "https://wiki.example.com/download/final/x.pdf": httpx.Response(
                200, content=b"pdf", headers={"content-type": "application/pdf; charset=binary"}
            ),
        }
    )
    async with _download_client(transport) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        assert await api.download("/download/attachments/42/x.pdf") == (b"pdf", "application/pdf")

    assert transport.hosts == ["wiki.example.com"] * 3
    assert all(auth == "Bearer abc" for _, auth in transport.seen)


@pytest.mark.asyncio
async def test_download_relative_path_on_a_client_bound_elsewhere_is_refused() -> None:
    # A caller-supplied client gets the credentials installed on it; if its
    # base_url is not the Confluence origin, a relative download link must not
    # be resolved there.
    transport = _RecordingTransport({})
    async with httpx.AsyncClient(
        base_url="https://other.example", transport=httpx.MockTransport(transport)
    ) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        with pytest.raises(ConfluenceError, match="origin"):
            await api.download("/download/attachments/42/x.pdf")

    assert transport.seen == []


@pytest.mark.asyncio
async def test_download_link_with_userinfo_is_refused_before_any_request() -> None:
    transport = _RecordingTransport({})
    async with _download_client(transport) as client:
        api = ConfluenceClient(
            _settings(confluence_auth_mode="bearer", confluence_token="abc"), client=client
        )

        with pytest.raises(ConfluenceError, match="userinfo"):
            await api.download("https://user:secret@wiki.example.com/download/x.pdf")

    assert transport.seen == []
