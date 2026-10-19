"""Tests for BookStackClient rate-limit gate."""

from __future__ import annotations

import asyncio
import time
from typing import Any
from unittest.mock import AsyncMock, MagicMock

import httpx
import pytest

from ironrag_connector.sources.bookstack.bookstack import BookStackClient, BookStackError
from ironrag_connector.sources.bookstack.config import BookStackSettings


def _settings(**overrides: Any) -> BookStackSettings:
    base = {
        "bookstack_base_url": "http://wiki.example.com",
        "bookstack_token_id": "tid",
        "bookstack_token_secret": "tsecret",
        "bookstack_webhook_bearer": "bearer-token",
        "ironrag_base_url": "http://ironrag.example.com",
        "ironrag_api_token": "irtoken",
        "routing_config_path": "/nonexistent/routing.yaml",
        "bookstack_min_request_interval_seconds": 0.0,
    }
    base.update(overrides)
    return BookStackSettings.model_validate(base)


def _mock_client(status_code: int = 200, json_body: dict | None = None) -> httpx.AsyncClient:
    """Return an httpx.AsyncClient stub that always returns the given status."""
    mock = MagicMock(spec=httpx.AsyncClient)
    response = MagicMock(spec=httpx.Response)
    response.status_code = status_code
    response.headers = httpx.Headers({})
    response.json.return_value = json_body if json_body is not None else {"data": [], "total": 0}
    response.text = ""
    response.content = b""
    mock.request = AsyncMock(return_value=response)
    return mock


class TestRateLimitGate:
    async def test_zero_interval_no_sleep(self):
        """With interval=0, two sequential requests complete without extra sleep."""
        client = _mock_client()
        settings = _settings(bookstack_min_request_interval_seconds=0.0)
        bs = BookStackClient(settings, client=client)

        t0 = time.monotonic()
        await bs._request("GET", "/api/pages/1")
        await bs._request("GET", "/api/pages/2")
        elapsed = time.monotonic() - t0

        # Should be fast — definitely under 0.3 s even on a slow machine.
        assert elapsed < 0.3
        assert client.request.await_count == 2

    async def test_interval_enforced_between_requests(self):
        """Two requests with interval=0.15 s must be at least 0.14 s apart."""
        client = _mock_client()
        settings = _settings(bookstack_min_request_interval_seconds=0.15)
        bs = BookStackClient(settings, client=client)

        t0 = time.monotonic()
        await bs._request("GET", "/api/pages/1")
        t1 = time.monotonic()
        await bs._request("GET", "/api/pages/2")
        t2 = time.monotonic()

        gap = t2 - t1
        total = t2 - t0
        # Second request must wait for the remaining interval from when the
        # first one started; total elapsed ≥ interval.
        assert total >= 0.14, f"total elapsed {total:.3f}s < 0.14s"
        assert gap >= 0.0  # sanity

    async def test_concurrent_requests_start_spaced_by_the_interval(self):
        """Concurrent callers are still spaced by the interval (rate limit kept)."""
        call_times: list[float] = []

        mock_client = MagicMock(spec=httpx.AsyncClient)
        response = MagicMock(spec=httpx.Response)
        response.status_code = 200
        response.headers = httpx.Headers({})
        response.json.return_value = {"data": [], "total": 0}
        response.text = ""

        async def _request(*args: Any, **kwargs: Any) -> MagicMock:
            call_times.append(time.monotonic())
            return response

        mock_client.request = _request

        settings = _settings(bookstack_min_request_interval_seconds=0.1)
        bs = BookStackClient(settings, client=mock_client)

        # Fire 3 concurrent requests.
        await asyncio.gather(
            bs._request("GET", "/api/pages/1"),
            bs._request("GET", "/api/pages/2"),
            bs._request("GET", "/api/pages/3"),
        )

        assert len(call_times) == 3
        # Each request must be at least ~0.09 s after the previous.
        for i in range(1, len(call_times)):
            gap = call_times[i] - call_times[i - 1]
            assert gap >= 0.09, f"gap {i}: {gap:.3f}s < 0.09s (requests not spaced by the interval)"

    async def test_concurrent_requests_overlap_while_staying_spaced(self):
        """The second request is in flight while the first one is still open."""
        started: list[float] = []
        first_in_flight = asyncio.Event()
        release_first = asyncio.Event()
        second_in_flight = asyncio.Event()

        mock_client = MagicMock(spec=httpx.AsyncClient)
        response = MagicMock(spec=httpx.Response)
        response.status_code = 200
        response.headers = httpx.Headers({})
        response.json.return_value = {"data": [], "total": 0}
        response.text = ""

        async def _request(*args: Any, **kwargs: Any) -> MagicMock:
            started.append(time.monotonic())
            if len(started) == 1:
                first_in_flight.set()
                await release_first.wait()
            else:
                second_in_flight.set()
            return response

        mock_client.request = _request
        settings = _settings(bookstack_min_request_interval_seconds=0.05)
        bs = BookStackClient(settings, client=mock_client)

        first = asyncio.create_task(bs._request("GET", "/api/pages/1"))
        await first_in_flight.wait()
        second = asyncio.create_task(bs._request("GET", "/api/pages/2"))
        try:
            await asyncio.wait_for(second_in_flight.wait(), timeout=5.0)
        finally:
            release_first.set()
            await asyncio.gather(first, second)

        assert started[1] - started[0] >= 0.04


class TestCollectionContract:
    @pytest.mark.parametrize(
        "payload",
        [
            {},
            {"total": 0},
            {"data": {}, "total": 0},
            {"data": [], "total": "zero"},
            {"data": [{}], "total": 1},
            {"data": [{"id": True}], "total": 1},
            {"data": [{"id": 0}], "total": 1},
            {"data": [{"id": "1"}], "total": 1},
        ],
    )
    async def test_malformed_first_page_raises(self, payload: dict[str, Any]) -> None:
        bs = BookStackClient(_settings(), client=_mock_client(json_body=payload))

        with pytest.raises(BookStackError, match="malformed collection"):
            _ = [item async for item in bs.list_pages()]

    async def test_malformed_later_page_raises(self) -> None:
        client = _mock_client()
        first = MagicMock(spec=httpx.Response)
        first.status_code = 200
        first.headers = httpx.Headers({})
        first.json.return_value = {"data": [{"id": 1}], "total": 2}
        first.text = ""
        second = MagicMock(spec=httpx.Response)
        second.status_code = 200
        second.headers = httpx.Headers({})
        second.json.return_value = {}
        second.text = ""
        client.request = AsyncMock(side_effect=[first, second])
        bs = BookStackClient(_settings(), client=client)

        with pytest.raises(BookStackError, match="malformed collection"):
            _ = [item async for item in bs.list_pages(page_size=1)]

    @pytest.mark.parametrize(
        "payload",
        [
            {"data": [], "total": 1},
            {"data": [{"id": 1}], "total": 2},
            {"data": [{"id": 1}, {"id": 2}], "total": 1},
        ],
    )
    async def test_contradictory_total_raises(self, payload: dict[str, Any]) -> None:
        bs = BookStackClient(_settings(), client=_mock_client(json_body=payload))

        with pytest.raises(BookStackError, match="pagination contract"):
            _ = [item async for item in bs.list_pages(page_size=2)]


class TestDependentReads:
    async def test_download_image_goes_through_the_throttled_request_path(self) -> None:
        image_url = "http://wiki.example.com/uploads/images/a.png"
        client = _mock_client()
        # The download policy resolves the URL the way the real client would
        # and reads back which URL was actually requested, so those two pieces
        # stay real while the wire call itself stays a stub.
        client.build_request = httpx.AsyncClient(base_url="http://wiki.example.com").build_request
        response = client.request.return_value
        response.headers = httpx.Headers({"content-type": "image/png; charset=binary"})
        response.content = b"\x89PNG"
        response.request = httpx.Request("GET", image_url)
        response.has_redirect_location = False
        client.get = AsyncMock(return_value=response)
        bs = BookStackClient(_settings(), client=client)

        payload, mime = await bs.download_image(image_url)

        assert (payload, mime) == (b"\x89PNG", "image/png")
        assert client.request.await_count == 1
        # Redirects are followed by the same-origin policy hop by hop, never by
        # httpx on its own, so the throttled call must not follow them itself.
        assert client.request.await_args.kwargs.get("follow_redirects") is False
        client.get.assert_not_called()

    async def test_attachment_payload_rejects_undecodable_base64(self) -> None:
        client = _mock_client(
            json_body={"id": 5, "name": "notes.txt", "extension": "txt", "content": "not base64!!"}
        )
        bs = BookStackClient(_settings(), client=client)

        with pytest.raises(BookStackError, match="base64"):
            await bs.get_attachment_payload(5)
