from __future__ import annotations

import asyncio
import os
import subprocess
import time
from ipaddress import ip_address
from typing import Any
from urllib.parse import urlsplit

import httpx
import pytest
from test_youtrack_live_lifecycle import ARTICLE_COUNT, LiveYouTrack, _credentials_or_skip

pytestmark = [pytest.mark.live, pytest.mark.fullstack]


def _loopback_url(name: str) -> str:
    value = os.environ.get(name, "").strip().rstrip("/")
    if not value:
        pytest.skip(f"set {name} to run full-stack tests")
    hostname = urlsplit(value).hostname
    is_loopback = hostname == "localhost"
    if hostname and not is_loopback:
        try:
            is_loopback = ip_address(hostname).is_loopback
        except ValueError:
            is_loopback = False
    if not is_loopback:
        raise AssertionError(f"{name} must resolve to an explicit loopback URL")
    return value


def _required_env(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        pytest.skip(f"set {name} to run full-stack tests")
    return value


class FullStackClient:
    def __init__(self) -> None:
        self.library_id = _required_env("IRONRAG_E2E_LIBRARY_ID")
        self.connector_url = _loopback_url("CONNECTOR_E2E_URL")
        self.connector_admin_token = _required_env("CONNECTOR_E2E_ADMIN_TOKEN")
        ironrag_url = _loopback_url("IRONRAG_E2E_URL")
        ironrag_token = _required_env("IRONRAG_E2E_TOKEN")
        self._connector = httpx.AsyncClient(
            base_url=self.connector_url,
            headers={"Authorization": f"Bearer {self.connector_admin_token}"},
            timeout=180.0,
        )
        self._ironrag = httpx.AsyncClient(
            base_url=ironrag_url,
            headers={"Authorization": f"Bearer {ironrag_token}"},
            timeout=60.0,
        )

    async def close(self) -> None:
        await self._connector.aclose()
        await self._ironrag.aclose()

    async def wait_connector(self, timeout: float = 90.0) -> None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                response = await self._connector.get("/health")
                if response.status_code == 200 and response.json().get("status") == "ok":
                    return
            except (httpx.HTTPError, ValueError):
                pass
            await asyncio.sleep(0.5)
        raise AssertionError("connector did not become healthy before the deadline")

    async def sync(self) -> dict[str, Any]:
        response = await self._connector.post("/sync/run")
        _assert_status(response, 200, "connector sync")
        payload = _json_object(response, "connector sync")
        assert payload.get("errors") == 0, payload
        return payload

    async def documents(self, *, include_deleted: bool = False) -> list[dict[str, Any]]:
        response = await self._ironrag.get(
            "/v1/content/documents",
            params={
                "libraryId": self.library_id,
                "includeDeleted": str(include_deleted).lower(),
                "includeTotal": "true",
                "limit": 200,
            },
        )
        _assert_status(response, 200, "list IronRAG documents")
        payload = _json_object(response, "list IronRAG documents")
        items = payload.get("items")
        if not isinstance(items, list) or not all(isinstance(item, dict) for item in items):
            raise AssertionError("IronRAG document list did not contain object items")
        return items

    async def wait_ready(
        self,
        expected_keys: set[str],
        *,
        timeout: float = 180.0,
    ) -> dict[str, dict[str, Any]]:
        deadline = time.monotonic() + timeout
        last: list[dict[str, Any]] = []
        while time.monotonic() < deadline:
            last = await self.documents()
            by_key = {str(item.get("externalKey")): item for item in last}
            if set(by_key) == expected_keys:
                details = await asyncio.gather(
                    *(self.document_detail(str(item["id"])) for item in by_key.values())
                )
                if all(_detail_is_fully_applied(detail) for detail in details):
                    return by_key
            failed = [
                {
                    "externalKey": item.get("externalKey"),
                    "status": item.get("status"),
                    "failureCode": item.get("failureCode"),
                }
                for item in last
                if item.get("status") == "failed"
            ]
            if failed:
                raise AssertionError(f"IronRAG ingestion failed: {failed}")
            await asyncio.sleep(1.0)
        summary = [
            (item.get("externalKey"), item.get("status"), item.get("readiness")) for item in last
        ]
        raise AssertionError(f"IronRAG documents did not become ready: {summary}")

    async def wait_empty(self, *, timeout: float = 120.0) -> None:
        deadline = time.monotonic() + timeout
        last: list[dict[str, Any]] = []
        while time.monotonic() < deadline:
            last = await self.documents()
            if not last:
                return
            await asyncio.sleep(0.5)
        summary = [str(item.get("externalKey")) for item in last]
        raise AssertionError(f"IronRAG documents were not reaped: {summary}")

    async def document_detail(self, document_id: str) -> dict[str, Any]:
        response = await self._ironrag.get(f"/v1/content/documents/{document_id}")
        _assert_status(response, 200, "get IronRAG document")
        return _json_object(response, "get IronRAG document")

    async def revisions(self, document_id: str) -> list[dict[str, Any]]:
        response = await self._ironrag.get(f"/v1/content/documents/{document_id}/revisions")
        _assert_status(response, 200, "list IronRAG revisions")
        payload = response.json()
        if not isinstance(payload, list) or not all(isinstance(item, dict) for item in payload):
            raise AssertionError("IronRAG revisions response was not an object list")
        return payload

    async def source(self, document_id: str, revision_id: str) -> bytes:
        response = await self._ironrag.get(
            f"/v1/content/documents/{document_id}/source",
            params={"revisionId": revision_id},
        )
        _assert_status(response, 200, "download IronRAG source")
        return response.content

    async def knowledge_document(self, document_id: str) -> dict[str, Any]:
        response = await self._ironrag.get(
            f"/v1/knowledge/libraries/{self.library_id}/documents/{document_id}"
        )
        _assert_status(response, 200, "get IronRAG knowledge document")
        return _json_object(response, "get IronRAG knowledge document")

    async def search(self, query: str) -> dict[str, Any]:
        response = await self._ironrag.get(
            "/v1/search/documents",
            params={"libraryId": self.library_id, "query": query, "limit": 20},
        )
        _assert_status(response, 200, "search IronRAG documents")
        return _json_object(response, "search IronRAG documents")


def _assert_status(response: httpx.Response, expected: int, operation: str) -> None:
    if response.status_code != expected:
        raise AssertionError(f"{operation} returned HTTP {response.status_code}")


def _json_object(response: httpx.Response, operation: str) -> dict[str, Any]:
    try:
        payload = response.json()
    except ValueError as error:
        raise AssertionError(f"{operation} returned invalid JSON") from error
    if not isinstance(payload, dict):
        raise AssertionError(f"{operation} did not return an object")
    return payload


def _assert_report(report: dict[str, Any], **expected: int) -> None:
    for field, value in expected.items():
        assert report.get(field) == value, report


def _detail_is_fully_applied(detail: dict[str, Any]) -> bool:
    head = detail.get("head")
    readiness = detail.get("readiness")
    pipeline = detail.get("pipeline")
    if not all(isinstance(value, dict) for value in (head, readiness, pipeline)):
        return False
    assert isinstance(head, dict)
    assert isinstance(readiness, dict)
    assert isinstance(pipeline, dict)
    revision_id = head.get("active_revision_id")
    latest_job = pipeline.get("latest_job")
    latest_mutation = pipeline.get("latest_mutation")
    return bool(
        revision_id
        and head.get("active_revision_id") == revision_id
        and head.get("readable_revision_id") == revision_id
        and readiness.get("revision_id") == revision_id
        and readiness.get("text_state") in {"readable", "ready", "text_readable"}
        and readiness.get("vector_state") == "ready"
        # A completed extraction with no grounded graph contributions is a
        # stable IronRAG v0.5.9 state named ``processing``. Vector retrieval is
        # fully usable, and the initial fixture separately proves graph-ready
        # ingestion with a grounded deterministic entity.
        and readiness.get("graph_state") in {"ready", "graph_degraded", "processing"}
        and isinstance(latest_job, dict)
        and latest_job.get("queue_state") == "completed"
        and isinstance(latest_mutation, dict)
        and latest_mutation.get("mutation_state") == "applied"
    )


def _restart_connector() -> None:
    container = _required_env("CONNECTOR_E2E_CONTAINER")
    if not container.startswith("ironrag-youtrack-connector-e2e-"):
        raise AssertionError("CONNECTOR_E2E_CONTAINER is outside the isolated E2E namespace")
    subprocess.run(
        ["docker", "restart", container],
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        timeout=120,
    )


@pytest.mark.asyncio
async def test_youtrack_connector_real_ironrag_lifecycle() -> None:
    base_url, youtrack_token = _credentials_or_skip()
    stack = FullStackClient()
    try:
        await stack.wait_connector()

        # Make interrupted local runs self-healing before creating a new isolated
        # YouTrack project. The generated runtime points at a dedicated E2E library.
        preclean = await stack.sync()
        assert preclean.get("errors") == 0, preclean
        await stack.wait_empty()
    except BaseException:
        await stack.close()
        raise

    async with LiveYouTrack(base_url, youtrack_token) as youtrack:
        try:
            _, project_short_name = await youtrack.create_project()
            run_marker = f"Fixture run {project_short_name}"
            article_ids = [
                await youtrack.create_article(
                    summary=f"Full stack record {index:03d}",
                    content=f"Initial marker FS{index:03d} ALPHA. {run_marker}.",
                )
                for index in range(ARTICLE_COUNT)
            ]
            issue_id = await youtrack.create_issue(
                summary="Full stack issue comment lifecycle",
                description=f"Stable issue body. {run_marker}.",
            )
            issue_key = f"youtrack:issue:{issue_id}"
            expected_keys = {
                *(f"youtrack:article:{article_id}" for article_id in article_ids),
                issue_key,
            }

            initial = await stack.sync()
            _assert_report(
                initial,
                items_seen=ARTICLE_COUNT + 1,
                created=ARTICLE_COUNT + 1,
                replaced=0,
                reaped=0,
                errors=0,
            )
            initial_documents = await stack.wait_ready(expected_keys)
            initial_ids = {key: str(document["id"]) for key, document in initial_documents.items()}
            initial_details = await asyncio.gather(
                *(stack.document_detail(document_id) for document_id in initial_ids.values())
            )
            assert {
                detail.get("readiness", {}).get("graph_state") for detail in initial_details
            } == {"ready"}

            unchanged = await stack.sync()
            _assert_report(
                unchanged,
                items_seen=ARTICLE_COUNT + 1,
                created=0,
                replaced=0,
                noop_unchanged=ARTICLE_COUNT + 1,
                reaped=0,
                errors=0,
            )

            renamed_id = article_ids[2]
            cleared_id = article_ids[3]
            renamed_key = f"youtrack:article:{renamed_id}"
            cleared_key = f"youtrack:article:{cleared_id}"
            await youtrack.update_article(
                renamed_id,
                summary="Full stack record 002 revised",
                content="Unique revised marker OMEGA-002.",
            )
            await youtrack.update_article(cleared_id, content="")

            changed = await stack.sync()
            _assert_report(
                changed,
                items_seen=ARTICLE_COUNT + 1,
                created=0,
                replaced=2,
                noop_unchanged=ARTICLE_COUNT - 1,
                reaped=0,
                errors=0,
            )
            changed_documents = await stack.wait_ready(expected_keys)
            assert str(changed_documents[renamed_key]["id"]) == initial_ids[renamed_key]
            assert str(changed_documents[cleared_key]["id"]) == initial_ids[cleared_key]

            renamed_revisions = await stack.revisions(initial_ids[renamed_key])
            cleared_revisions = await stack.revisions(initial_ids[cleared_key])
            assert sorted(revision["revision_number"] for revision in renamed_revisions) == [1, 2]
            assert sorted(revision["revision_number"] for revision in cleared_revisions) == [1, 2]
            renamed_latest = max(
                renamed_revisions, key=lambda revision: revision["revision_number"]
            )
            cleared_latest = max(
                cleared_revisions, key=lambda revision: revision["revision_number"]
            )
            renamed_source = await stack.source(initial_ids[renamed_key], str(renamed_latest["id"]))
            assert renamed_source.startswith(b"# Full stack record 002 revised\n")
            assert b"Unique revised marker OMEGA-002." in renamed_source
            cleared_source = await stack.source(initial_ids[cleared_key], str(cleared_latest["id"]))
            assert cleared_source.startswith(b"# Full stack record 003\n")
            assert b"Initial marker FS003 ALPHA" not in cleared_source

            knowledge = await stack.knowledge_document(initial_ids[renamed_key])
            chunks = knowledge.get("latestRevisionChunks")
            assert isinstance(chunks, list) and chunks
            assert any("OMEGA-002" in str(chunk.get("content_text", "")) for chunk in chunks)
            search = await stack.search("OMEGA-002")
            hits = search.get("documentHits")
            assert isinstance(hits, list) and hits
            assert any(
                str(hit.get("document", {}).get("document_id")) == initial_ids[renamed_key]
                for hit in hits
                if isinstance(hit, dict)
            )

            comment_alpha = f"FULLSTACK-ISSUE-COMMENT-ALPHA-{project_short_name}"
            comment_beta = f"FULLSTACK-ISSUE-COMMENT-BETA-{project_short_name}"
            issue_comment_id = await youtrack.create_issue_comment(
                issue_id,
                text=comment_alpha,
            )
            comment_created = await stack.sync()
            _assert_report(
                comment_created,
                items_seen=ARTICLE_COUNT + 1,
                created=0,
                replaced=1,
                noop_unchanged=ARTICLE_COUNT,
                reaped=0,
                errors=0,
            )
            await stack.wait_ready(expected_keys)
            issue_revisions = await stack.revisions(initial_ids[issue_key])
            assert sorted(revision["revision_number"] for revision in issue_revisions) == [1, 2]
            issue_latest = max(issue_revisions, key=lambda revision: revision["revision_number"])
            assert comment_alpha.encode() in await stack.source(
                initial_ids[issue_key], str(issue_latest["id"])
            )

            await youtrack.update_issue_comment(
                issue_id,
                issue_comment_id,
                text=comment_beta,
            )
            comment_edited = await stack.sync()
            _assert_report(
                comment_edited,
                items_seen=ARTICLE_COUNT + 1,
                created=0,
                replaced=1,
                noop_unchanged=ARTICLE_COUNT,
                reaped=0,
                errors=0,
            )
            await stack.wait_ready(expected_keys)
            issue_revisions = await stack.revisions(initial_ids[issue_key])
            assert sorted(revision["revision_number"] for revision in issue_revisions) == [
                1,
                2,
                3,
            ]
            issue_latest = max(issue_revisions, key=lambda revision: revision["revision_number"])
            issue_source = await stack.source(initial_ids[issue_key], str(issue_latest["id"]))
            assert comment_beta.encode() in issue_source
            assert comment_alpha.encode() not in issue_source
            comment_search = await stack.search(comment_beta)
            comment_hits = comment_search.get("documentHits")
            assert isinstance(comment_hits, list)
            assert comment_hits
            assert any(
                str(hit.get("document", {}).get("document_id")) == initial_ids[issue_key]
                for hit in comment_hits
                if isinstance(hit, dict)
            )

            await youtrack.delete_issue_comment(issue_id, issue_comment_id)
            comment_deleted = await stack.sync()
            _assert_report(
                comment_deleted,
                items_seen=ARTICLE_COUNT + 1,
                created=0,
                replaced=1,
                noop_unchanged=ARTICLE_COUNT,
                reaped=0,
                errors=0,
            )
            await stack.wait_ready(expected_keys)
            issue_revisions = await stack.revisions(initial_ids[issue_key])
            assert sorted(revision["revision_number"] for revision in issue_revisions) == [
                1,
                2,
                3,
                4,
            ]
            issue_latest = max(issue_revisions, key=lambda revision: revision["revision_number"])
            assert comment_beta.encode() not in await stack.source(
                initial_ids[issue_key], str(issue_latest["id"])
            )
            deleted_comment_search = await stack.search(comment_beta)
            deleted_comment_hits = deleted_comment_search.get("documentHits")
            assert isinstance(deleted_comment_hits, list)
            assert all(
                str(hit.get("document", {}).get("document_id")) != initial_ids[issue_key]
                for hit in deleted_comment_hits
                if isinstance(hit, dict)
            )

            deleted_ids = {
                article_ids[0],
                article_ids[len(article_ids) // 2],
                article_ids[-1],
            }
            deleted_keys = {f"youtrack:article:{article_id}" for article_id in deleted_ids}
            deleted_document_ids = {initial_ids[key] for key in deleted_keys}
            for article_id in deleted_ids:
                await youtrack.delete_article(article_id)
            new_id = await youtrack.create_article(
                summary="Full stack replacement record",
                content="Unique post-reconciliation marker SIGMA-NEW.",
            )
            new_key = f"youtrack:article:{new_id}"
            final_keys = (expected_keys - deleted_keys) | {new_key}

            reconciled = await stack.sync()
            _assert_report(
                reconciled,
                items_seen=ARTICLE_COUNT - 1,
                created=1,
                replaced=0,
                noop_unchanged=ARTICLE_COUNT - 2,
                reaped=3,
                errors=0,
            )
            await stack.wait_ready(final_keys)
            including_deleted = await stack.documents(include_deleted=True)
            deleted_rows = {
                str(item.get("id")): item
                for item in including_deleted
                if str(item.get("id")) in deleted_document_ids
            }
            assert set(deleted_rows) == deleted_document_ids
            assert all(item.get("documentState") == "deleted" for item in deleted_rows.values())
            deleted_search = await stack.search("FS000")
            deleted_hits = deleted_search.get("documentHits")
            assert isinstance(deleted_hits, list)
            assert all(
                str(hit.get("document", {}).get("document_id")) not in deleted_document_ids
                for hit in deleted_hits
                if isinstance(hit, dict)
            )

            _restart_connector()
            await stack.wait_connector()
            after_restart = await stack.sync()
            _assert_report(
                after_restart,
                items_seen=ARTICLE_COUNT - 1,
                created=0,
                replaced=0,
                noop_unchanged=ARTICLE_COUNT - 1,
                reaped=0,
                errors=0,
            )
            await stack.wait_ready(final_keys)
            assert sorted(
                revision["revision_number"]
                for revision in await stack.revisions(initial_ids[issue_key])
            ) == [1, 2, 3, 4]

            final_article_ids = (set(article_ids) - deleted_ids) | {new_id}
            for article_id in final_article_ids:
                await youtrack.delete_article(article_id)
            await youtrack.delete_issue(issue_id)
            cleaned = await stack.sync()
            _assert_report(
                cleaned,
                items_seen=0,
                created=0,
                replaced=0,
                noop_unchanged=0,
                reaped=ARTICLE_COUNT - 1,
                errors=0,
            )
            await stack.wait_empty()
            final_search = await stack.search("SIGMA-NEW")
            assert final_search.get("documentHits") == []
        finally:
            await stack.close()
