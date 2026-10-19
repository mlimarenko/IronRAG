from __future__ import annotations

import httpx
import pytest

from ironrag_connector.download_policy import (
    CrossOriginDownloadError,
    DownloadPolicyError,
    Origin,
    fetch_within_origin,
)

BASE = "https://wiki.example.com/wiki"


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("https://wiki.example.com", Origin("https", "wiki.example.com", 443)),
        ("http://Wiki.Example.com/api/", Origin("http", "wiki.example.com", 80)),
        ("https://wiki.example.com:8443/x?y=1", Origin("https", "wiki.example.com", 8443)),
        ("https://wiki.example.com:443/", Origin("https", "wiki.example.com", 443)),
        ("https://wiki.example.com:0/", Origin("https", "wiki.example.com", 0)),
    ],
    ids=("https-default", "http-default-case-folded", "explicit-port", "explicit-default", "zero"),
)
def test_origin_normalises_scheme_host_and_port(url: str, expected: Origin) -> None:
    assert Origin.of(url) == expected
    assert str(expected) == f"{expected.scheme}://{expected.host}:{expected.port}"


@pytest.mark.parametrize(
    "url",
    [
        "ftp://wiki.example.com/x",
        "file:///etc/passwd",
        "/download/x",
        "https:///no-host",
        "https://wiki.example.com:notaport/",
    ],
    ids=("ftp", "file", "relative", "no-host", "malformed"),
)
def test_origin_rejects_non_http_hostless_or_malformed_urls(url: str) -> None:
    with pytest.raises(DownloadPolicyError):
        Origin.of(url)


class _Sender:
    """Stands in for a client's single-request method; records every URL sent."""

    def __init__(self, routes: dict[str, httpx.Response]) -> None:
        self.sent: list[str] = []
        self._routes = routes

    async def __call__(self, url: str) -> httpx.Response:
        self.sent.append(url)
        request = httpx.Request("GET", url)
        response = self._routes[url]
        response.request = request
        if response.has_redirect_location:
            response.next_request = httpx.Request(
                "GET", request.url.join(response.headers["location"])
            )
        return response


def _client(base_url: str = "https://wiki.example.com") -> httpx.AsyncClient:
    return httpx.AsyncClient(base_url=base_url)


@pytest.mark.asyncio
async def test_absolute_url_on_another_origin_is_refused_before_sending() -> None:
    sender = _Sender({})
    client = _client()
    with pytest.raises(CrossOriginDownloadError, match="outside the source origin"):
        await fetch_within_origin(client, BASE, "https://evil.example/x", send=sender)
    assert sender.sent == []


@pytest.mark.asyncio
async def test_relative_path_is_resolved_with_the_client_and_returned() -> None:
    sender = _Sender({"https://wiki.example.com/download/x": httpx.Response(200, content=b"x")})
    response = await fetch_within_origin(_client(), BASE, "/download/x", send=sender)
    assert response.content == b"x"
    assert sender.sent == ["https://wiki.example.com/download/x"]


@pytest.mark.asyncio
async def test_relative_path_on_a_client_bound_elsewhere_is_refused_before_sending() -> None:
    # An injected client whose base_url is not the source origin would resolve
    # a relative path to another host; that is caught before any request.
    sender = _Sender({})
    client = _client("https://other.example")
    with pytest.raises(CrossOriginDownloadError):
        await fetch_within_origin(client, BASE, "/x", send=sender)
    assert sender.sent == []


@pytest.mark.parametrize(
    "reference",
    [
        "https://user:secret@wiki.example.com/x",
        "//evil.example/x",
        "\\\\evil.example\\x",
        "https://wiki.example.com:0/x",
        "https://wiki.example.com:notaport/x",
    ],
    ids=("userinfo", "authority-relative", "backslash-authority", "port-zero", "malformed"),
)
@pytest.mark.asyncio
async def test_ambiguous_or_off_origin_references_are_refused_before_sending(
    reference: str,
) -> None:
    sender = _Sender({})
    client = _client()
    with pytest.raises(DownloadPolicyError):
        await fetch_within_origin(client, BASE, reference, send=sender)
    assert sender.sent == []


@pytest.mark.asyncio
async def test_redirect_hops_inside_the_origin_are_followed_up_to_the_limit() -> None:
    routes = {
        f"https://wiki.example.com/hop{i}": httpx.Response(
            302, headers={"location": f"/hop{i + 1}"}
        )
        for i in range(5)
    }
    routes["https://wiki.example.com/hop5"] = httpx.Response(200, content=b"done")
    sender = _Sender(routes)

    response = await fetch_within_origin(_client(), BASE, "/hop0", send=sender)

    assert response.content == b"done"
    assert len(sender.sent) == 6


@pytest.mark.asyncio
async def test_one_redirect_beyond_the_limit_fails() -> None:
    routes = {
        f"https://wiki.example.com/hop{i}": httpx.Response(
            302, headers={"location": f"/hop{i + 1}"}
        )
        for i in range(6)
    }
    routes["https://wiki.example.com/hop6"] = httpx.Response(200, content=b"never")
    sender = _Sender(routes)

    client = _client()
    with pytest.raises(DownloadPolicyError, match="more than 5 redirects"):
        await fetch_within_origin(client, BASE, "/hop0", send=sender)
    assert len(sender.sent) == 6


@pytest.mark.parametrize(
    "location",
    [
        "http://wiki.example.com/c",
        "https://user:secret@wiki.example.com/c",
        "//wiki.example.com/c",
        "\\\\wiki.example.com\\c",
    ],
    ids=("scheme-downgrade", "userinfo", "authority-relative", "backslash-authority"),
)
@pytest.mark.asyncio
async def test_redirect_hop_leaving_the_origin_or_carrying_userinfo_is_not_sent(
    location: str,
) -> None:
    sender = _Sender(
        {
            "https://wiki.example.com/a": httpx.Response(302, headers={"location": "/b"}),
            "https://wiki.example.com/b": httpx.Response(302, headers={"location": location}),
        }
    )
    client = _client()
    with pytest.raises(DownloadPolicyError):
        await fetch_within_origin(client, BASE, "/a", send=sender)
    assert sender.sent == ["https://wiki.example.com/a", "https://wiki.example.com/b"]


@pytest.mark.asyncio
async def test_response_actually_fetched_from_another_origin_is_refused() -> None:
    # Belt and braces: even if a sender ignored the URL it was given, the URL
    # the response reports is checked too.
    async def send(url: str) -> httpx.Response:
        response = httpx.Response(200, content=b"x")
        response.request = httpx.Request("GET", "https://other.example/x")
        return response

    client = _client()
    with pytest.raises(CrossOriginDownloadError):
        await fetch_within_origin(client, BASE, "/download/x", send=send)
