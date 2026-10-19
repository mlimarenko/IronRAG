"""BookStack client: vendor credentials go to the BookStack origin only.

The client installs ``Authorization`` as a default header, and httpx sends
default headers on every request whatever host the URL names. Two client
methods fetch URLs that come from source data: the ``content`` of an external
attachment (an arbitrary link an author pasted) and image URLs collected from
page markdown. Each test records every host the transport saw, so a leak shows
up as a request that should never have been made -- not merely as a header.
"""

from __future__ import annotations

from typing import Any

import httpx
import pytest

from ironrag_connector.sources.bookstack.bookstack import BookStackClient, BookStackError
from ironrag_connector.sources.bookstack.config import BookStackSettings

BASE = "http://wiki.example.com"
AUTH = "Token tid:tsecret"


def _settings(**overrides: Any) -> BookStackSettings:
    data: dict[str, Any] = {
        "bookstack_base_url": BASE,
        "bookstack_token_id": "tid",
        "bookstack_token_secret": "tsecret",
        "ironrag_base_url": "http://ironrag.example.com",
        "ironrag_api_token": "irtoken",
        "routing_config_path": "/nonexistent/routing.yaml",
        "bookstack_min_request_interval_seconds": 0.0,
    }
    data.update(overrides)
    return BookStackSettings(**data)


class _Transport:
    """MockTransport that remembers (url, authorization) of every request."""

    def __init__(self, routes: dict[str, httpx.Response]) -> None:
        self.seen: list[tuple[str, str | None]] = []
        self._routes = routes

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append((str(request.url), request.headers.get("authorization")))
        return self._routes.get(str(request.url), httpx.Response(200, content=b"leaked"))

    @property
    def hosts(self) -> list[str]:
        return [httpx.URL(url).host for url, _ in self.seen]


def _client(transport: _Transport) -> httpx.AsyncClient:
    # Production installs the header on the client it owns; a caller-supplied
    # client is used as-is, so the test supplies the same shape.
    return httpx.AsyncClient(
        base_url=BASE,
        headers={"Authorization": AUTH},
        transport=httpx.MockTransport(transport),
    )


def _external_attachment(attachment_id: int, url: str) -> httpx.Response:
    return httpx.Response(
        200,
        json={
            "id": attachment_id,
            "name": "link",
            "extension": "",
            "external": True,
            "content": url,
        },
    )


@pytest.mark.asyncio
async def test_external_attachment_on_another_origin_is_never_requested() -> None:
    transport = _Transport(
        {f"{BASE}/api/attachments/7": _external_attachment(7, "http://evil.example/steal")}
    )
    async with _client(transport) as client:
        payload, _mime, _name = await BookStackClient(
            _settings(), client=client
        ).get_attachment_payload(7)

    assert payload == b""
    assert transport.hosts == ["wiki.example.com"]


@pytest.mark.asyncio
async def test_external_attachment_on_the_bookstack_origin_is_fetched_with_credentials() -> None:
    transport = _Transport(
        {
            f"{BASE}/api/attachments/8": _external_attachment(8, f"{BASE}/uploads/files/x.bin"),
            f"{BASE}/uploads/files/x.bin": httpx.Response(200, content=b"bytes"),
        }
    )
    async with _client(transport) as client:
        payload, _mime, _name = await BookStackClient(
            _settings(), client=client
        ).get_attachment_payload(8)

    assert payload == b"bytes"
    assert transport.seen[-1] == (f"{BASE}/uploads/files/x.bin", AUTH)


@pytest.mark.asyncio
async def test_image_redirected_off_origin_is_not_followed() -> None:
    transport = _Transport(
        {
            f"{BASE}/uploads/images/a.png": httpx.Response(
                302, headers={"location": "http://evil.example/a.png"}
            ),
        }
    )
    async with _client(transport) as client:
        bookstack = BookStackClient(_settings(), client=client)
        with pytest.raises(BookStackError, match="origin"):
            await bookstack.download_image(f"{BASE}/uploads/images/a.png")

    assert transport.hosts == ["wiki.example.com"]


@pytest.mark.asyncio
async def test_image_redirected_within_origin_is_followed() -> None:
    transport = _Transport(
        {
            f"{BASE}/uploads/images/a.png": httpx.Response(
                302, headers={"location": "/uploads/images/moved/a.png"}
            ),
            f"{BASE}/uploads/images/moved/a.png": httpx.Response(
                301, headers={"location": f"{BASE}/uploads/images/final/a.png"}
            ),
            f"{BASE}/uploads/images/final/a.png": httpx.Response(
                200, content=b"png", headers={"content-type": "image/png"}
            ),
        }
    )
    async with _client(transport) as client:
        payload, mime = await BookStackClient(_settings(), client=client).download_image(
            f"{BASE}/uploads/images/a.png"
        )

    assert (payload, mime) == (b"png", "image/png")
    assert transport.hosts == ["wiki.example.com"] * 3


@pytest.mark.asyncio
async def test_image_on_another_origin_is_rejected_before_any_request() -> None:
    transport = _Transport({})
    async with _client(transport) as client:
        bookstack = BookStackClient(_settings(), client=client)
        with pytest.raises(BookStackError, match="origin"):
            await bookstack.download_image("http://evil.example/uploads/images/a.png")

    assert transport.seen == []


@pytest.mark.asyncio
async def test_image_on_a_client_bound_elsewhere_is_refused_before_any_request() -> None:
    transport = _Transport({})
    async with httpx.AsyncClient(
        base_url="http://other.example",
        headers={"Authorization": AUTH},
        transport=httpx.MockTransport(transport),
    ) as client:
        bookstack = BookStackClient(_settings(), client=client)
        with pytest.raises(BookStackError, match="origin"):
            await bookstack.download_image("/uploads/a.png")

    assert transport.seen == []
