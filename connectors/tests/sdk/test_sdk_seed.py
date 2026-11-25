from __future__ import annotations

from pathlib import Path
from uuid import UUID

import pytest
from pytest_httpx import HTTPXMock

from ironrag_connector.config import BaseConnectorSettings
from ironrag_connector.ironrag import IronRagError
from ironrag_connector.seed import _seed_async
from ironrag_connector.state import StateStore

WS = UUID("00000000-0000-0000-0000-000000000099")
LIB = UUID("00000000-0000-0000-0000-000000000000")


class SeedAdapter:
    name = "seed-test"
    kinds = ("page",)
    primary_kinds = ("page",)

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return None

    def external_key(self, kind: str, item_id: str) -> str:
        return f"seed-test:{kind}:{item_id}"

    async def close(self) -> None:
        return None


class ParsedSeedAdapter(SeedAdapter):
    def __init__(self, parsed: tuple[str, str]) -> None:
        self._parsed = parsed

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return self._parsed


def _settings(tmp_path: Path) -> BaseConnectorSettings:
    routing = tmp_path / "routing.yaml"
    routing.write_text(
        """
default:
  library: main/knowledge-base
rules:
  - match: {tag: reference}
    target: {library: main/knowledge-base}
""".strip(),
        encoding="utf-8",
    )
    return BaseConnectorSettings(
        ironrag_base_url="http://ironrag.example.com",
        ironrag_api_token="test-token",
        request_timeout_seconds=60.0,
        routing_config_path=routing,
        state_db_path=tmp_path / "state.sqlite",
    )


def _mock_library(
    httpx_mock: HTTPXMock,
    documents: list[dict[str, str]],
    *,
    next_cursor: str | None = None,
) -> None:
    httpx_mock.add_response(
        url="http://ironrag.example.com/v1/catalog/workspaces",
        json=[{"id": str(WS), "slug": "main", "displayName": "Main"}],
    )
    httpx_mock.add_response(
        url=(
            f"http://ironrag.example.com/v1/catalog/workspaces/{WS}/libraries"
            "?search=knowledge-base&offset=0&limit=100"
        ),
        json={
            "items": [
                {
                    "id": str(LIB),
                    "workspaceId": str(WS),
                    "slug": "knowledge-base",
                    "displayName": "Knowledge base",
                }
            ],
            "totalCount": 1,
            "offset": 0,
            "limit": 100,
        },
    )
    httpx_mock.add_response(
        url=f"http://ironrag.example.com/v1/content/libraries/{LIB}/documents?limit=200",
        json={"items": documents, "nextCursor": next_cursor},
    )


@pytest.mark.asyncio
async def test_seed_resolves_friendly_ref_before_scanning_library(
    tmp_path: Path, httpx_mock: HTTPXMock
) -> None:
    settings = _settings(tmp_path)
    _mock_library(httpx_mock, [])

    counts = await _seed_async(settings, SeedAdapter())  # type: ignore[arg-type]

    assert counts == {"libraries": 1, "docs_scanned": 0, "rows_inserted": 0}
    requests = httpx_mock.get_requests()
    assert [request.url.path for request in requests] == [
        "/v1/catalog/workspaces",
        f"/v1/catalog/workspaces/{WS}/libraries",
        f"/v1/content/libraries/{LIB}/documents",
    ]
    assert all(request.headers["Authorization"] == "Bearer test-token" for request in requests)


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("external_key", "parsed"),
    [
        ("seed-test:private:1", ("private", "1")),
        ("seed-test:page:wrong", ("page", "1")),
    ],
    ids=("undeclared-kind", "noncanonical-round-trip"),
)
async def test_seed_rejects_unowned_or_noncanonical_external_keys(
    tmp_path: Path,
    httpx_mock: HTTPXMock,
    external_key: str,
    parsed: tuple[str, str],
) -> None:
    settings = _settings(tmp_path)
    _mock_library(
        httpx_mock,
        [
            {
                "id": "00000000-0000-0000-0000-000000000001",
                "libraryId": str(LIB),
                "externalKey": external_key,
                "status": "ready",
            }
        ],
    )

    counts = await _seed_async(settings, ParsedSeedAdapter(parsed))  # type: ignore[arg-type]

    assert counts == {"libraries": 1, "docs_scanned": 1, "rows_inserted": 0}


@pytest.mark.asyncio
async def test_seed_rejects_document_from_different_library(
    tmp_path: Path,
    httpx_mock: HTTPXMock,
) -> None:
    settings = _settings(tmp_path)
    _mock_library(
        httpx_mock,
        [
            {
                "id": "00000000-0000-0000-0000-000000000001",
                "libraryId": str(WS),
                "externalKey": "seed-test:page:1",
                "status": "ready",
            }
        ],
    )

    adapter = ParsedSeedAdapter(("page", "1"))
    with pytest.raises(IronRagError, match="library identity"):
        await _seed_async(settings, adapter)  # type: ignore[arg-type]


@pytest.mark.asyncio
async def test_seed_fails_instead_of_looping_on_repeated_document_cursor(
    tmp_path: Path,
    httpx_mock: HTTPXMock,
) -> None:
    settings = _settings(tmp_path)
    _mock_library(httpx_mock, [], next_cursor="stuck")
    httpx_mock.add_response(
        url=(
            f"http://ironrag.example.com/v1/content/libraries/{LIB}/documents"
            "?limit=200&cursor=stuck"
        ),
        json={"items": [], "nextCursor": "stuck"},
    )

    adapter = SeedAdapter()
    with pytest.raises(IronRagError, match="repeated pagination cursor"):
        await _seed_async(settings, adapter)  # type: ignore[arg-type]


def _document(external_key: str) -> dict[str, str]:
    return {
        "id": "00000000-0000-0000-0000-000000000001",
        "libraryId": str(LIB),
        "externalKey": external_key,
        "status": "ready",
    }


@pytest.mark.asyncio
async def test_seed_skips_a_document_the_adapter_cannot_parse(
    tmp_path: Path,
    httpx_mock: HTTPXMock,
) -> None:
    """A library may hold another connector's documents; seeding owns only its own."""

    settings = _settings(tmp_path)
    _mock_library(httpx_mock, [_document("other-connector:page:1")])

    counts = await _seed_async(settings, SeedAdapter())  # type: ignore[arg-type]

    assert counts == {"libraries": 1, "docs_scanned": 1, "rows_inserted": 0}


class UnbuildableKeyAdapter(SeedAdapter):
    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return ("page", "1")

    def external_key(self, kind: str, item_id: str) -> str:
        raise ValueError(f"unbuildable identity for {kind}:{item_id}")


@pytest.mark.asyncio
async def test_seed_skips_a_document_whose_identity_cannot_be_rebuilt(
    tmp_path: Path,
    httpx_mock: HTTPXMock,
) -> None:
    settings = _settings(tmp_path)
    _mock_library(httpx_mock, [_document("seed-test:page:1")])

    counts = await _seed_async(settings, UnbuildableKeyAdapter())  # type: ignore[arg-type]

    assert counts == {"libraries": 1, "docs_scanned": 1, "rows_inserted": 0}


@pytest.mark.asyncio
async def test_seed_registers_a_fully_owned_document_with_no_change_token(
    tmp_path: Path,
    httpx_mock: HTTPXMock,
) -> None:
    """The cursor starts undated: the next sweep picks the live source token."""

    settings = _settings(tmp_path)
    _mock_library(httpx_mock, [_document("seed-test:page:1")])

    counts = await _seed_async(settings, ParsedSeedAdapter(("page", "1")))  # type: ignore[arg-type]

    assert counts == {"libraries": 1, "docs_scanned": 1, "rows_inserted": 1}
    state = StateStore(settings.state_db_path)
    try:
        row = state.get("page", "1")
        assert row is not None
        assert row.external_key == "seed-test:page:1"
        assert row.change_token is None
        assert row.current_library_id == str(LIB)
        locations = state.locations_for("page", "1")
        assert [location.ironrag_document_id for location in locations] == [
            "00000000-0000-0000-0000-000000000001"
        ]
    finally:
        state.close()
