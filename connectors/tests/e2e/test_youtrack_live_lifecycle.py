from __future__ import annotations

import os
from dataclasses import dataclass
from ipaddress import ip_address
from pathlib import Path
from types import TracebackType
from typing import Any, Self
from urllib.parse import quote, urlsplit
from uuid import UUID, uuid4

import httpx
import pytest
from pydantic import SecretStr
from test_youtrack_lifecycle import MemoryIronRag

from ironrag_connector.orchestrator import Orchestrator
from ironrag_connector.policy import PushPolicy
from ironrag_connector.routing import (
    PolicyOverrides,
    ResolvedLibraryTarget,
    Router,
    RoutingConfig,
)
from ironrag_connector.sources.youtrack.adapter import YouTrackAdapter
from ironrag_connector.sources.youtrack.config import YouTrackSettings
from ironrag_connector.state import StateStore
from ironrag_connector.sync import SyncManager

WORKSPACE_ID = UUID("00000000-0000-0000-0000-000000000199")
LIBRARY_ID = UUID("00000000-0000-0000-0000-000000000177")
LIBRARY_REF = "tests/youtrack-live"
ARTICLE_COUNT = 17
PAGE_SIZE = 7

pytestmark = pytest.mark.live


def _credentials_or_skip() -> tuple[str, SecretStr]:
    base_url = os.environ.get("YOUTRACK_E2E_URL", "").strip().rstrip("/")
    token = SecretStr(os.environ.get("YOUTRACK_E2E_TOKEN", "").strip())
    if not base_url or not token.get_secret_value():
        pytest.skip("set YOUTRACK_E2E_URL and YOUTRACK_E2E_TOKEN to run live tests")
    _assert_safe_target(base_url)
    return base_url, token


def _assert_safe_target(base_url: str) -> None:
    hostname = urlsplit(base_url).hostname
    is_loopback = hostname == "localhost"
    if hostname and not is_loopback:
        try:
            is_loopback = ip_address(hostname).is_loopback
        except ValueError:
            is_loopback = False
    if not is_loopback and os.environ.get("YOUTRACK_E2E_ALLOW_REMOTE_DESTRUCTIVE") != "1":
        raise AssertionError(
            "live lifecycle is destructive and defaults to loopback targets; "
            "set YOUTRACK_E2E_ALLOW_REMOTE_DESTRUCTIVE=1 for an isolated remote fixture"
        )


class LiveYouTrack:
    """Small lifecycle fixture that never includes credentials in failures."""

    def __init__(self, base_url: str, token: SecretStr) -> None:
        self._client = httpx.AsyncClient(
            base_url=f"{base_url}/api/",
            headers={
                "Accept": "application/json",
                "Content-Type": "application/json",
                "Authorization": f"Bearer {token.get_secret_value()}",
            },
            follow_redirects=True,
            timeout=30.0,
        )
        self.project_id: str | None = None
        self.project_short_name: str | None = None
        self.article_ids: list[str] = []
        self.issue_ids: list[str] = []
        self.comment_ids: list[tuple[str, str, str]] = []

    async def __aenter__(self) -> Self:
        return self

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        traceback: TracebackType | None,
    ) -> bool:
        del exc_type, traceback
        cleanup_failures: list[str] = []
        try:
            for kind, parent_id, comment_id in reversed(self.comment_ids):
                path = f"{kind}/{quote(parent_id, safe='')}/comments/{quote(comment_id, safe='')}"
                try:
                    response = await self._client.delete(path)
                except httpx.HTTPError:
                    cleanup_failures.append(f"{kind} comment cleanup request failed")
                    continue
                if response.status_code not in {200, 204, 404}:
                    cleanup_failures.append(
                        f"{kind} comment cleanup returned HTTP {response.status_code}"
                    )
            for issue_id in reversed(self.issue_ids):
                try:
                    response = await self._client.delete(f"issues/{quote(issue_id, safe='')}")
                except httpx.HTTPError:
                    cleanup_failures.append("issue cleanup request failed")
                    continue
                if response.status_code not in {200, 204, 404}:
                    cleanup_failures.append(f"issue cleanup returned HTTP {response.status_code}")
            for article_id in reversed(self.article_ids):
                try:
                    response = await self._client.delete(f"articles/{quote(article_id, safe='')}")
                except httpx.HTTPError:
                    cleanup_failures.append("article cleanup request failed")
                    continue
                if response.status_code not in {200, 204, 404}:
                    cleanup_failures.append(f"article cleanup returned HTTP {response.status_code}")
            if self.project_id is not None:
                try:
                    response = await self._client.delete(
                        f"admin/projects/{quote(self.project_id, safe='')}"
                    )
                except httpx.HTTPError:
                    cleanup_failures.append("project cleanup request failed")
                else:
                    if response.status_code not in {200, 204, 404}:
                        cleanup_failures.append(
                            f"project cleanup returned HTTP {response.status_code}"
                        )
        finally:
            await self._client.aclose()

        if cleanup_failures:
            message = "; ".join(cleanup_failures)
            if exc is not None:
                exc.add_note(message)
            else:
                raise AssertionError(message)
        return False

    async def create_project(self) -> tuple[str, str]:
        user = await self._json(
            "GET",
            "users/me",
            operation="read current user",
            params={"fields": "id,login,guest"},
        )
        user_id = _required_id(user, operation="read current user")
        if user.get("guest") is True or user.get("login") == "guest":
            raise AssertionError("live token resolved to the guest account")

        suffix = uuid4().hex[:5].upper()
        short_name = f"E2E{suffix}"
        project = await self._json(
            "POST",
            "admin/projects",
            operation="create synthetic project",
            params={"fields": "id,name,shortName"},
            json={
                "name": f"Connector lifecycle {suffix}",
                "shortName": short_name,
                "leader": {"id": user_id},
            },
        )
        self.project_id = _required_id(project, operation="create synthetic project")
        self.project_short_name = short_name
        returned_short_name = project.get("shortName")
        if returned_short_name != short_name:
            raise AssertionError("YouTrack returned an unexpected project short name")
        return self.project_id, short_name

    async def create_article(self, *, summary: str, content: str) -> str:
        if self.project_id is None:
            raise AssertionError("synthetic project must be created first")
        article = await self._json(
            "POST",
            "articles",
            operation="create synthetic article",
            params={"fields": "id,idReadable,summary,content,updated"},
            json={
                "project": {"id": self.project_id},
                "summary": summary,
                "content": content,
            },
        )
        article_id = _required_id(article, operation="create synthetic article")
        self.article_ids.append(article_id)
        return article_id

    async def create_issue(self, *, summary: str, description: str) -> str:
        if self.project_id is None:
            raise AssertionError("synthetic project must be created first")
        issue = await self._json(
            "POST",
            "issues",
            operation="create synthetic issue",
            params={"fields": "id,idReadable,summary,description,updated"},
            json={
                "project": {"id": self.project_id},
                "summary": summary,
                "description": description,
            },
        )
        issue_id = _required_id(issue, operation="create synthetic issue")
        self.issue_ids.append(issue_id)
        return issue_id

    async def create_article_comment(self, article_id: str, *, text: str) -> str:
        return await self._create_comment("articles", article_id, text=text)

    async def create_issue_comment(self, issue_id: str, *, text: str) -> str:
        return await self._create_comment("issues", issue_id, text=text)

    async def _create_comment(self, kind: str, parent_id: str, *, text: str) -> str:
        comment = await self._json(
            "POST",
            f"{kind}/{quote(parent_id, safe='')}/comments",
            operation=f"create synthetic {kind} comment",
            params={"fields": "id,text,created,updated"},
            json={"text": text},
        )
        comment_id = _required_id(comment, operation=f"create synthetic {kind} comment")
        self.comment_ids.append((kind, parent_id, comment_id))
        return comment_id

    async def update_article_comment(self, article_id: str, comment_id: str, *, text: str) -> None:
        await self._update_comment("articles", article_id, comment_id, text=text)

    async def update_issue_comment(self, issue_id: str, comment_id: str, *, text: str) -> None:
        await self._update_comment("issues", issue_id, comment_id, text=text)

    async def _update_comment(
        self, kind: str, parent_id: str, comment_id: str, *, text: str
    ) -> None:
        comment = await self._json(
            "POST",
            f"{kind}/{quote(parent_id, safe='')}/comments/{quote(comment_id, safe='')}",
            operation=f"update synthetic {kind} comment",
            params={"fields": "id,text,created,updated"},
            json={"text": text},
        )
        if _required_id(comment, operation=f"update synthetic {kind} comment") != comment_id:
            raise AssertionError(f"YouTrack updated an unexpected {kind} comment")

    async def delete_article_comment(self, article_id: str, comment_id: str) -> None:
        await self._delete_comment("articles", article_id, comment_id)

    async def delete_issue_comment(self, issue_id: str, comment_id: str) -> None:
        await self._delete_comment("issues", issue_id, comment_id)

    async def _delete_comment(self, kind: str, parent_id: str, comment_id: str) -> None:
        response = await self._client.delete(
            f"{kind}/{quote(parent_id, safe='')}/comments/{quote(comment_id, safe='')}"
        )
        _require_status(
            response,
            operation=f"delete synthetic {kind} comment",
            expected={200, 204},
        )

    async def update_article(self, article_id: str, **changes: str) -> None:
        article = await self._json(
            "POST",
            f"articles/{quote(article_id, safe='')}",
            operation="update synthetic article",
            params={"fields": "id,summary,content,updated"},
            json=changes,
        )
        if _required_id(article, operation="update synthetic article") != article_id:
            raise AssertionError("YouTrack updated an unexpected article")

    async def delete_article(self, article_id: str) -> None:
        response = await self._client.delete(f"articles/{quote(article_id, safe='')}")
        _require_status(response, operation="delete synthetic article", expected={200, 204})

    async def delete_issue(self, issue_id: str) -> None:
        response = await self._client.delete(f"issues/{quote(issue_id, safe='')}")
        _require_status(response, operation="delete synthetic issue", expected={200, 204})

    async def _json(
        self,
        method: str,
        path: str,
        *,
        operation: str,
        params: dict[str, str] | None = None,
        json: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        response = await self._client.request(method, path, params=params, json=json)
        _require_status(response, operation=operation, expected={200})
        try:
            payload = response.json()
        except ValueError as error:
            raise AssertionError(f"YouTrack {operation} returned invalid JSON") from error
        if not isinstance(payload, dict):
            raise AssertionError(f"YouTrack {operation} did not return an object")
        return payload


@dataclass
class ConnectorRuntime:
    manager: SyncManager
    adapter: YouTrackAdapter
    state: StateStore
    closed: bool = False

    async def close(self) -> None:
        if self.closed:
            return
        try:
            await self.adapter.close()
        finally:
            self.state.close()
            self.closed = True


def _runtime(
    *,
    state_path: Path,
    base_url: str,
    token: SecretStr,
    project_short_name: str,
    ironrag: MemoryIronRag,
) -> ConnectorRuntime:
    settings = YouTrackSettings(
        youtrack_base_url=base_url,
        youtrack_token=token,
        youtrack_page_size=PAGE_SIZE,
        youtrack_projects=project_short_name,
        youtrack_min_request_interval_seconds=0,
        youtrack_snapshot_validation_passes=2,
        ironrag_base_url="https://ironrag.example.com",
        ironrag_api_token="synthetic-ironrag-token",
        admin_bearer_token="synthetic-admin-token",
        sync_concurrency=1,
    )
    policies = PolicyOverrides(default=PushPolicy(), by_kind={})
    adapter = YouTrackAdapter(settings, policies)
    router = Router(
        RoutingConfig.model_validate(
            {
                "default": {
                    "library": LIBRARY_REF,
                }
            }
        ),
        resolved_targets={
            LIBRARY_REF: ResolvedLibraryTarget(
                library_ref=LIBRARY_REF,
                workspace_id=WORKSPACE_ID,
                library_id=LIBRARY_ID,
            )
        },
    )
    state = StateStore(state_path)
    orchestrator = Orchestrator(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        router=router,
        state=state,
        policies=policies,
    )
    manager = SyncManager(
        adapter=adapter,
        ironrag=ironrag,  # type: ignore[arg-type]
        orchestrator=orchestrator,
        router=router,
        state=state,
        policies=policies,
        concurrency=1,
        interval_seconds=60,
    )
    return ConnectorRuntime(manager=manager, adapter=adapter, state=state)


def _required_id(payload: dict[str, Any], *, operation: str) -> str:
    value = payload.get("id")
    if not isinstance(value, str) or not value:
        raise AssertionError(f"YouTrack {operation} response did not contain an id")
    return value


def _require_status(
    response: httpx.Response,
    *,
    operation: str,
    expected: set[int],
) -> None:
    if response.status_code not in expected:
        raise AssertionError(f"YouTrack {operation} failed with HTTP {response.status_code}")


@pytest.mark.asyncio
async def test_real_youtrack_article_lifecycle_and_cursor_restart(tmp_path: Path) -> None:
    base_url, token = _credentials_or_skip()
    state_path = tmp_path / "state.sqlite"
    ironrag = MemoryIronRag()
    runtime: ConnectorRuntime | None = None
    restarted: ConnectorRuntime | None = None

    async with LiveYouTrack(base_url, token) as youtrack:
        try:
            _, project_short_name = await youtrack.create_project()
            article_ids = [
                await youtrack.create_article(
                    summary=f"Record {index:03d}",
                    content=f"Synthetic content for record {index:03d}.",
                )
                for index in range(ARTICLE_COUNT)
            ]
            assert ARTICLE_COUNT > PAGE_SIZE * 2

            runtime = _runtime(
                state_path=state_path,
                base_url=base_url,
                token=token,
                project_short_name=project_short_name,
                ironrag=ironrag,
            )

            initial = await runtime.manager.run_once(reason="live-initial")
            assert (initial.items_seen, initial.created, initial.errors) == (
                ARTICLE_COUNT,
                ARTICLE_COUNT,
                0,
            )
            expected_keys = {f"youtrack:article:{article_id}" for article_id in article_ids}
            assert set(ironrag.documents) == expected_keys

            unchanged = await runtime.manager.run_once(reason="live-unchanged")
            assert (
                unchanged.noop_unchanged,
                unchanged.created,
                unchanged.replaced,
                unchanged.errors,
            ) == (ARTICLE_COUNT, 0, 0, 0)

            renamed_id = article_ids[2]
            cleared_id = article_ids[3]
            await youtrack.update_article(
                renamed_id,
                summary="Record 002 revised",
                content="Revised synthetic content.",
            )
            await youtrack.update_article(cleared_id, content="")

            changed = await runtime.manager.run_once(reason="live-update-and-clear")
            assert (changed.replaced, changed.noop_unchanged, changed.errors) == (
                2,
                ARTICLE_COUNT - 2,
                0,
            )
            renamed_payload = ironrag.documents[f"youtrack:article:{renamed_id}"]["payload"]
            assert renamed_payload.startswith(b"# Record 002 revised\n")
            assert b"Revised synthetic content." in renamed_payload
            cleared_payload = ironrag.documents[f"youtrack:article:{cleared_id}"]["payload"]
            assert cleared_payload.startswith(b"# Record 003\n")
            assert b"Synthetic content for record 003." not in cleared_payload

            deleted_ids = {
                article_ids[0],
                article_ids[len(article_ids) // 2],
                article_ids[-1],
            }
            deleted_document_ids = {
                ironrag.documents[f"youtrack:article:{article_id}"]["id"]
                for article_id in deleted_ids
            }
            for article_id in deleted_ids:
                await youtrack.delete_article(article_id)
            new_id = await youtrack.create_article(
                summary="Replacement record",
                content="Synthetic content created after reconciliation.",
            )

            reconciled = await runtime.manager.run_once(reason="live-create-and-delete")
            assert (
                reconciled.created,
                reconciled.reaped,
                reconciled.noop_unchanged,
                reconciled.errors,
            ) == (1, 3, ARTICLE_COUNT - 3, 0)
            final_keys = (expected_keys - {f"youtrack:article:{item}" for item in deleted_ids}) | {
                f"youtrack:article:{new_id}"
            }
            assert set(ironrag.documents) == final_keys
            assert deleted_document_ids <= set(ironrag.deleted_ids)

            await runtime.close()
            restarted = _runtime(
                state_path=state_path,
                base_url=base_url,
                token=token,
                project_short_name=project_short_name,
                ironrag=ironrag,
            )
            after_restart = await restarted.manager.run_once(reason="live-restart")
            assert (
                after_restart.items_seen,
                after_restart.noop_unchanged,
                after_restart.created,
                after_restart.replaced,
                after_restart.reaped,
                after_restart.errors,
            ) == (ARTICLE_COUNT - 2, ARTICLE_COUNT - 2, 0, 0, 0, 0)
            assert set(ironrag.documents) == final_keys
        finally:
            if runtime is not None:
                await runtime.close()
            if restarted is not None:
                await restarted.close()


@pytest.mark.asyncio
async def test_real_youtrack_article_and_issue_comment_lifecycle(tmp_path: Path) -> None:
    base_url, token = _credentials_or_skip()
    state_path = tmp_path / "state.sqlite"
    ironrag = MemoryIronRag()
    runtime: ConnectorRuntime | None = None
    restarted: ConnectorRuntime | None = None

    async with LiveYouTrack(base_url, token) as youtrack:
        try:
            _, project_short_name = await youtrack.create_project()
            article_id = await youtrack.create_article(
                summary="Article comment lifecycle",
                content="Stable article body.",
            )
            issue_id = await youtrack.create_issue(
                summary="Issue comment lifecycle",
                description="Stable issue body.",
            )
            article_key = f"youtrack:article:{article_id}"
            issue_key = f"youtrack:issue:{issue_id}"
            expected_keys = {article_key, issue_key}

            runtime = _runtime(
                state_path=state_path,
                base_url=base_url,
                token=token,
                project_short_name=project_short_name,
                ironrag=ironrag,
            )
            initial = await runtime.manager.run_once(reason="live-comment-parents")
            assert (initial.items_seen, initial.created, initial.errors) == (2, 2, 0)
            assert set(ironrag.documents) == expected_keys
            initial_ids = {key: ironrag.documents[key]["id"] for key in expected_keys}

            article_comment_id = await youtrack.create_article_comment(
                article_id,
                text="LIVE-ARTICLE-COMMENT-ALPHA",
            )
            issue_comment_id = await youtrack.create_issue_comment(
                issue_id,
                text="LIVE-ISSUE-COMMENT-ALPHA",
            )
            created = await runtime.manager.run_once(reason="live-comment-create")
            assert (created.replaced, created.noop_unchanged, created.errors) == (2, 0, 0)
            assert b"LIVE-ARTICLE-COMMENT-ALPHA" in ironrag.documents[article_key]["payload"]
            assert b"LIVE-ISSUE-COMMENT-ALPHA" in ironrag.documents[issue_key]["payload"]
            assert {key: ironrag.documents[key]["id"] for key in expected_keys} == initial_ids
            assert {ironrag.revision_counts[key] for key in expected_keys} == {2}

            stable = await runtime.manager.run_once(reason="live-comment-create-stable")
            assert (stable.noop_unchanged, stable.replaced, stable.errors) == (2, 0, 0)
            assert {ironrag.revision_counts[key] for key in expected_keys} == {2}

            await youtrack.update_article_comment(
                article_id,
                article_comment_id,
                text="LIVE-ARTICLE-COMMENT-BETA",
            )
            await youtrack.update_issue_comment(
                issue_id,
                issue_comment_id,
                text="LIVE-ISSUE-COMMENT-BETA",
            )
            edited = await runtime.manager.run_once(reason="live-comment-edit")
            assert (edited.replaced, edited.noop_unchanged, edited.errors) == (2, 0, 0)
            article_payload = ironrag.documents[article_key]["payload"]
            issue_payload = ironrag.documents[issue_key]["payload"]
            assert b"LIVE-ARTICLE-COMMENT-BETA" in article_payload
            assert b"LIVE-ARTICLE-COMMENT-ALPHA" not in article_payload
            assert b"LIVE-ISSUE-COMMENT-BETA" in issue_payload
            assert b"LIVE-ISSUE-COMMENT-ALPHA" not in issue_payload
            assert {ironrag.revision_counts[key] for key in expected_keys} == {3}

            await youtrack.delete_article_comment(article_id, article_comment_id)
            await youtrack.delete_issue_comment(issue_id, issue_comment_id)
            deleted = await runtime.manager.run_once(reason="live-comment-delete")
            assert (deleted.replaced, deleted.noop_unchanged, deleted.errors) == (2, 0, 0)
            assert b"LIVE-ARTICLE-COMMENT-BETA" not in ironrag.documents[article_key]["payload"]
            assert b"LIVE-ISSUE-COMMENT-BETA" not in ironrag.documents[issue_key]["payload"]
            assert {ironrag.revision_counts[key] for key in expected_keys} == {4}

            await runtime.close()
            restarted = _runtime(
                state_path=state_path,
                base_url=base_url,
                token=token,
                project_short_name=project_short_name,
                ironrag=ironrag,
            )
            after_restart = await restarted.manager.run_once(reason="live-comment-restart")
            assert (
                after_restart.items_seen,
                after_restart.noop_unchanged,
                after_restart.replaced,
                after_restart.errors,
            ) == (2, 2, 0, 0)
            assert {ironrag.revision_counts[key] for key in expected_keys} == {4}
        finally:
            if runtime is not None:
                await runtime.close()
            if restarted is not None:
                await restarted.close()
