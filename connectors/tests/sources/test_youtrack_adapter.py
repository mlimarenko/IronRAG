from __future__ import annotations

import os
from collections.abc import AsyncIterator
from copy import deepcopy
from dataclasses import replace
from pathlib import Path
from uuid import UUID

import httpx
import pytest

import ironrag_connector.sources.youtrack.adapter as youtrack_adapter_module
from ironrag_connector import (
    PolicyOverrides,
    PushPolicy,
    ResolvedLibraryTarget,
    Router,
    RoutingReloader,
    SourceDeleteEnforcement,
    SourceExclusion,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceMaterializationEnforcement,
    load_routing_config,
)
from ironrag_connector.sources.youtrack.adapter import YouTrackAdapter, YouTrackApi
from ironrag_connector.sources.youtrack.config import YouTrackSettings
from ironrag_connector.sources.youtrack.mapping import KIND_ARTICLE, KIND_ISSUE
from ironrag_connector.sources.youtrack.youtrack import (
    YouTrackClient,
    YouTrackError,
    YouTrackNotFoundError,
)


def _settings(**overrides: object) -> YouTrackSettings:
    data: dict[str, object] = {
        "youtrack_base_url": "https://tracker.example.com/youtrack",
        "youtrack_token": "perm:synthetic-token",
        "ironrag_base_url": "https://ironrag.example.com",
        "ironrag_api_token": "synthetic-ironrag-token",
        "admin_bearer_token": "synthetic-admin-token",
    }
    data.update(overrides)
    return YouTrackSettings(**data)


def _policies(**include_comments: bool) -> PolicyOverrides:
    """The live per-kind table the framework hands the adapter at construction."""
    defaults = PushPolicy()
    return PolicyOverrides(
        default=defaults,
        by_kind={
            kind: replace(defaults, include_comments=enabled)
            for kind, enabled in include_comments.items()
        },
    )


def _adapter(
    settings: YouTrackSettings,
    *,
    client: YouTrackApi,
    policies: PolicyOverrides | None = None,
) -> YouTrackAdapter:
    return YouTrackAdapter(settings, policies or _policies(), client=client)


def _article(**overrides: object) -> dict[str, object]:
    article: dict[str, object] = {
        "id": "226-0",
        "idReadable": "DOCS-A-1",
        "summary": "Neutral Overview",
        "content": "A reusable body with `code` and a table.\n\n| A | B |\n|---|---|\n| 1 | 2 |",
        "created": 1_700_000_000_000,
        "updated": 1_700_000_001_000,
        "project": {
            "id": "0-7",
            "shortName": "DOCS",
            "name": "Documentation",
            "archived": False,
        },
        "parentArticle": None,
        "reporter": {"id": "1-2", "login": "writer", "name": "Writer"},
        "tags": [
            {"id": "6-2", "name": "beta"},
            {"id": "6-1", "name": "alpha"},
        ],
        "attachments": [
            {
                "id": "237-3",
                "name": "diagram.png",
                "updated": 1_700_000_000_500,
                "size": 1234,
                "mimeType": "image/png",
                "draft": False,
                "removed": False,
                "visibility": {"$type": "UnlimitedVisibility"},
                "url": "/youtrack/api/files/237-3?sign=must-not-be-indexed",
            }
        ],
        "visibility": {"$type": "UnlimitedVisibility"},
    }
    article.update(overrides)
    return article


def _issue(**overrides: object) -> dict[str, object]:
    issue: dict[str, object] = {
        "id": "3-0",
        "idReadable": "IRG-83",
        "summary": "Add issue sync",
        "description": "Body with **markdown** and a [link](https://example.com).",
        "created": 1_700_000_000_000,
        "updated": 1_700_000_001_000,
        "commentsCount": 0,
        "project": {
            "id": "0-7",
            "shortName": "IRG",
            "name": "IronRAG",
            "archived": False,
        },
        "reporter": {"id": "1-2", "login": "writer", "name": "Writer"},
        "tags": [
            {"id": "6-2", "name": "beta"},
            {"id": "6-1", "name": "alpha"},
        ],
        "customFields": [
            {
                "name": "Type",
                "$type": "SingleEnumIssueCustomField",
                "value": {"id": "t-1", "name": "Change"},
            },
            {
                "name": "State",
                "$type": "StateIssueCustomField",
                "value": {"id": "s-1", "name": "In Progress"},
            },
            {
                "name": "Priority",
                "$type": "SingleEnumIssueCustomField",
                "value": {"id": "p-1", "name": "Major"},
            },
            {
                "name": "Assignee",
                "$type": "SingleUserIssueCustomField",
                "value": {"id": "1-3", "login": "reviewer", "name": "Reviewer"},
            },
        ],
        "attachments": [],
        "links": [],
        "visibility": {"$type": "UnlimitedVisibility"},
    }
    issue.update(overrides)
    return issue


class FakeYouTrackClient:
    def __init__(
        self,
        summaries: list[dict[str, object]],
        details: dict[str, dict[str, object]],
        issue_summaries: list[dict[str, object]] | None = None,
        issue_details: dict[str, dict[str, object]] | None = None,
        article_comments: dict[str, list[dict[str, object]]] | None = None,
        issue_comments: dict[str, list[dict[str, object]]] | None = None,
        article_attachments: dict[str, list[dict[str, object]]] | None = None,
        issue_attachments: dict[str, list[dict[str, object]]] | None = None,
        issue_links: dict[str, list[dict[str, object]]] | None = None,
        issue_link_targets: dict[tuple[str, str], list[dict[str, object]]] | None = None,
    ) -> None:
        self.summaries = summaries
        self.details = details
        self.issue_summaries = issue_summaries or []
        self.issue_details = issue_details or {}
        self.article_comments = article_comments or {}
        self.issue_comments = issue_comments or {}
        self.article_attachments = article_attachments or {
            str(article["id"]): deepcopy(article.get("attachments", [])) for article in summaries
        }
        self.issue_attachments = issue_attachments or {
            str(issue["id"]): deepcopy(issue.get("attachments", []))
            for issue in self.issue_summaries
        }
        self.issue_links = issue_links or {}
        self.issue_link_targets = issue_link_targets or {}
        self.fetches: list[str] = []
        self.ancestor_fetches: list[str] = []
        self.issue_fetches: list[str] = []
        self.comment_reads: list[tuple[str, str]] = []
        self.validation_count = 0
        self.closed = False

    async def validate_credentials(self) -> dict[str, object]:
        self.validation_count += 1
        return {"id": "1-2", "login": "writer"}

    async def list_articles(self) -> AsyncIterator[dict[str, object]]:
        for article in self.summaries:
            yield deepcopy(article)

    async def get_article(self, article_id: str) -> dict[str, object]:
        self.fetches.append(article_id)
        try:
            return deepcopy(self.details[article_id])
        except KeyError as exc:
            raise YouTrackNotFoundError(article_id) from exc

    async def get_article_ancestor_descriptor(self, article_id: str) -> dict[str, object]:
        self.ancestor_fetches.append(article_id)
        try:
            article = self.details[article_id]
        except KeyError as exc:
            raise YouTrackNotFoundError(article_id) from exc
        descriptor: dict[str, object] = {"id": article.get("id")}
        if "parentArticle" in article:
            parent = article["parentArticle"]
            if isinstance(parent, dict):
                descriptor["parentArticle"] = {"id": parent.get("id")}
            else:
                descriptor["parentArticle"] = parent
        if "visibility" in article:
            descriptor["visibility"] = deepcopy(article["visibility"])
        return descriptor

    async def list_issues(self) -> AsyncIterator[dict[str, object]]:
        for issue in self.issue_summaries:
            yield deepcopy(issue)

    async def get_issue(self, issue_id: str) -> dict[str, object]:
        self.issue_fetches.append(issue_id)
        try:
            return deepcopy(self.issue_details[issue_id])
        except KeyError as exc:
            raise YouTrackNotFoundError(issue_id) from exc

    async def list_article_comments(self, article_id: str) -> AsyncIterator[dict[str, object]]:
        self.comment_reads.append((KIND_ARTICLE, article_id))
        for comment in self.article_comments.get(article_id, []):
            yield deepcopy(comment)

    async def list_issue_comments(self, issue_id: str) -> AsyncIterator[dict[str, object]]:
        self.comment_reads.append((KIND_ISSUE, issue_id))
        for comment in self.issue_comments.get(issue_id, []):
            yield deepcopy(comment)

    async def list_article_attachments(self, article_id: str) -> AsyncIterator[dict[str, object]]:
        for attachment in self.article_attachments.get(article_id, []):
            yield deepcopy(attachment)

    async def list_issue_attachments(self, issue_id: str) -> AsyncIterator[dict[str, object]]:
        for attachment in self.issue_attachments.get(issue_id, []):
            yield deepcopy(attachment)

    async def list_article_comment_attachments(
        self, article_id: str, comment_id: str
    ) -> AsyncIterator[dict[str, object]]:
        return
        yield  # pragma: no cover

    async def list_issue_comment_attachments(
        self, issue_id: str, comment_id: str
    ) -> AsyncIterator[dict[str, object]]:
        return
        yield  # pragma: no cover

    async def list_issue_links(self, issue_id: str) -> AsyncIterator[dict[str, object]]:
        for link in self.issue_links.get(issue_id, []):
            yield deepcopy(link)

    async def list_issue_link_targets(
        self, issue_id: str, link_id: str
    ) -> AsyncIterator[dict[str, object]]:
        for target in self.issue_link_targets.get((issue_id, link_id), []):
            yield deepcopy(target)

    async def aclose(self) -> None:
        self.closed = True


@pytest.mark.asyncio
async def test_iter_items_emits_stable_identity_routing_and_composite_token() -> None:
    source = _article()
    client = FakeYouTrackClient([source], {"226-0": source})
    adapter = _adapter(_settings(), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert len(refs) == 1
    ref = refs[0]
    assert ref.kind == KIND_ARTICLE
    assert ref.item_id == "226-0"
    assert ref.external_key == "youtrack:article:226-0"
    assert ref.change_token is not None
    assert ref.change_token.startswith("youtrack-composite-v3:")
    assert len(ref.change_token.rsplit(":", 1)[1]) == 64
    assert ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED
    assert ref.routing_facts == {
        "kind": KIND_ARTICLE,
        "article_id": "226-0",
        "article_id_readable": "DOCS-A-1",
        "project_id": "0-7",
        "project": "DOCS",
        "project_short_name": "DOCS",
        "project_name": "Documentation",
        "parent_article_id": None,
        "parent_article_id_readable": None,
        "tag": ["alpha", "beta"],
        "reporter_login": "writer",
    }
    assert client.validation_count == 2


@pytest.mark.asyncio
async def test_nested_article_keeps_readable_parent_identity_in_routing_and_content() -> None:
    parent = _article(id="226-parent", idReadable="DOCS-A-0", summary="Root")
    child = _article(
        parentArticle={"id": "226-parent", "idReadable": "DOCS-A-0"},
        attachments=[],
    )
    client = FakeYouTrackClient(
        [parent, child],
        {"226-parent": parent, "226-0": child},
    )
    adapter = _adapter(_settings(), client=client)

    refs = {ref.item_id: ref async for ref in adapter.iter_items()}
    fetched = await adapter.fetch(refs["226-0"])

    assert refs["226-0"].routing_facts["parent_article_id"] == "226-parent"
    assert refs["226-0"].routing_facts["parent_article_id_readable"] == "DOCS-A-0"
    assert isinstance(fetched, SourceFetchMaterialized)
    assert "**Parent article:** `DOCS-A-0`" in fetched.item.payload.decode("utf-8")
    assert client.fetches == ["226-0"]
    assert client.ancestor_fetches == ["226-parent"]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "ancestor_detail",
    [
        pytest.param(
            {
                "id": "226-parent",
                "visibility": {"$type": "UnlimitedVisibility"},
            },
            id="missing-parent-field",
        ),
        pytest.param(
            _article(id="226-other", idReadable="DOCS-A-9", summary="Other"),
            id="mismatched-id",
        ),
    ],
)
async def test_invalid_minimal_ancestor_descriptor_requires_security_delete(
    ancestor_detail: dict[str, object],
) -> None:
    parent_summary = _article(id="226-parent", idReadable="DOCS-A-0", summary="Root")
    child = _article(parentArticle={"id": "226-parent", "idReadable": "DOCS-A-0"})
    client = FakeYouTrackClient(
        [parent_summary, child],
        {"226-parent": ancestor_detail, "226-0": child},
    )
    adapter = _adapter(_settings(), client=client)
    refs = {ref.item_id: ref async for ref in adapter.iter_items()}

    fetched = await adapter.fetch(refs["226-0"])

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is False
    assert client.fetches == ["226-0"]
    assert client.ancestor_fetches == ["226-parent"]


@pytest.mark.asyncio
@pytest.mark.parametrize("visibility", ["missing", None, {"$type": "FutureVisibility"}])
async def test_unknown_minimal_ancestor_visibility_requires_security_delete(
    visibility: object,
) -> None:
    parent_summary = _article(id="226-parent", idReadable="DOCS-A-0", summary="Root")
    parent_detail = deepcopy(parent_summary)
    if visibility == "missing":
        del parent_detail["visibility"]
    else:
        parent_detail["visibility"] = visibility
    child = _article(parentArticle={"id": "226-parent", "idReadable": "DOCS-A-0"})
    client = FakeYouTrackClient(
        [parent_summary, child],
        {"226-parent": parent_detail, "226-0": child},
    )
    adapter = _adapter(_settings(), client=client)
    refs = {ref.item_id: ref async for ref in adapter.iter_items()}

    fetched = await adapter.fetch(refs["226-0"])

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.exclusion is SourceExclusion.LIMITED_VISIBILITY
    assert client.fetches == ["226-0"]
    assert client.ancestor_fetches == ["226-parent"]


@pytest.mark.asyncio
async def test_ancestor_missing_after_census_requires_security_delete() -> None:
    parent = _article(id="226-parent", idReadable="DOCS-A-0", summary="Root")
    child = _article(parentArticle={"id": "226-parent", "idReadable": "DOCS-A-0"})
    client = FakeYouTrackClient([parent, child], {"226-0": child})
    adapter = _adapter(_settings(), client=client)
    refs = {ref.item_id: ref async for ref in adapter.iter_items()}

    fetched = await adapter.fetch(refs["226-0"])

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.exclusion is SourceExclusion.NONE
    assert fetched.cacheable is False
    assert client.fetches == ["226-0"]
    assert client.ancestor_fetches == ["226-parent"]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    "parent_article", [pytest.param({}, id="missing-id"), pytest.param("bad", id="non-object")]
)
async def test_malformed_parent_before_census_is_enumerated_for_required_delete(
    parent_article: object,
) -> None:
    article = _article(parentArticle=parent_article)
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is True


@pytest.mark.asyncio
async def test_cyclic_article_ancestry_requires_security_delete() -> None:
    first = _article(
        id="226-0",
        parentArticle={"id": "226-parent", "idReadable": "DOCS-A-0"},
    )
    parent = _article(
        id="226-parent",
        idReadable="DOCS-A-0",
        parentArticle={"id": "226-0", "idReadable": "DOCS-A-1"},
    )
    client = FakeYouTrackClient(
        [first, parent],
        {"226-0": first, "226-parent": parent},
    )
    adapter = _adapter(_settings(), client=client)
    refs = {ref.item_id: ref async for ref in adapter.iter_items()}

    fetched = await adapter.fetch(refs["226-0"])

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is True
    assert client.ancestor_fetches == ["226-parent", "226-0"]


@pytest.mark.asyncio
async def test_change_token_detects_attachment_only_and_summary_changes() -> None:
    original = _article()
    attachment_changed = deepcopy(original)
    attachments = attachment_changed["attachments"]
    assert isinstance(attachments, list)
    assert isinstance(attachments[0], dict)
    attachments[0]["updated"] = 1_700_000_009_999
    summary_changed = deepcopy(original)
    summary_changed["summary"] = "Renamed Overview"

    original_ref = await _single_ref(original)
    attachment_ref = await _single_ref(attachment_changed)
    summary_ref = await _single_ref(summary_changed)

    assert original_ref.change_token != attachment_ref.change_token
    assert original_ref.change_token != summary_ref.change_token
    assert original_ref.external_key == summary_ref.external_key


@pytest.mark.asyncio
async def test_projection_contract_version_perturbs_both_token_hashes(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    article = _article()
    issue = _issue()
    article_live = await _single_ref(article)
    issue_live = await _first(
        _adapter(
            _settings(),
            client=FakeYouTrackClient(
                [],
                {},
                issue_summaries=[issue],
                issue_details={"3-0": issue},
            ),
        ).iter_items()
    )

    live_version = youtrack_adapter_module._YOUTRACK_PROJECTION_VERSION
    monkeypatch.setattr(youtrack_adapter_module, "_YOUTRACK_PROJECTION_VERSION", live_version + 1)
    article_bumped = await _single_ref(article)
    issue_bumped = await _first(
        _adapter(
            _settings(),
            client=FakeYouTrackClient(
                [],
                {},
                issue_summaries=[issue],
                issue_details={"3-0": issue},
            ),
        ).iter_items()
    )

    assert article_live.change_token is not None
    assert article_bumped.change_token is not None
    assert issue_live.change_token is not None
    assert issue_bumped.change_token is not None
    assert article_live.change_token.startswith(f"youtrack-composite-v{live_version}:")
    assert article_bumped.change_token.startswith(f"youtrack-composite-v{live_version + 1}:")
    assert (
        article_live.change_token.rsplit(":", 1)[1] != article_bumped.change_token.rsplit(":", 1)[1]
    )
    assert issue_live.change_token.rsplit(":", 1)[1] != issue_bumped.change_token.rsplit(":", 1)[1]


@pytest.mark.asyncio
async def test_attachment_order_does_not_change_token() -> None:
    first = _article(
        attachments=[
            {"id": "2", "name": "b.txt", "updated": 2, "size": 2},
            {"id": "1", "name": "a.txt", "updated": 1, "size": 1},
        ]
    )
    second = deepcopy(first)
    attachments = second["attachments"]
    assert isinstance(attachments, list)
    attachments.reverse()

    assert (await _single_ref(first)).change_token == (await _single_ref(second)).change_token


@pytest.mark.asyncio
async def test_fetch_renders_title_body_and_safe_attachment_index() -> None:
    summary = _article(summary="Current title", content=None)
    detail = _article(summary="Current title")
    client = FakeYouTrackClient([summary], {"226-0": detail})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    item = fetched.item
    assert item.ref.external_key == ref.external_key
    assert item.title == "Current title"
    assert item.mime_type == "text/markdown"
    assert item.file_name == "DOCS-A-1.md"
    assert item.document_hint == "https://tracker.example.com/youtrack/articles/DOCS-A-1"
    markdown = item.payload.decode("utf-8")
    assert markdown.startswith("# Current title\n")
    assert "A reusable body" in markdown
    assert "diagram.png" in markdown
    assert "image/png" in markdown
    assert "must-not-be-indexed" not in markdown
    assert item.dependents == ()


@pytest.mark.asyncio
async def test_parent_attachment_renders_created_and_updated_timestamps() -> None:
    attachment = _parent_attachment(
        id="10-1",
        name="spec.pdf",
        mimeType="application/pdf",
        size=2048,
        created=1_700_000_000_000,
        updated=1_700_000_005_000,
    )

    markdown = await _issue_attachment_markdown(attachment)

    assert (
        "- **spec.pdf** — id `10-1` — `application/pdf` — 2048 B"
        " — created 2023-11-14T22:13:20Z — updated 2023-11-14T22:13:25Z"
    ) in markdown


@pytest.mark.asyncio
async def test_comment_owned_attachment_renders_timestamps_for_both_kinds() -> None:
    issue = _issue(commentsCount=1)
    article = _article(attachments=[])
    issue_attachment = _parent_attachment(
        id="10-issue",
        name="issue-note.txt",
        size=11,
        created=1_700_000_000_000,
        updated=1_700_000_009_000,
        comment={"id": "7-1"},
    )
    article_attachment = _parent_attachment(
        id="10-article",
        name="article-note.txt",
        size=12,
        created=1_700_000_000_000,
        updated=1_700_000_009_000,
        comment={"id": "8-1"},
    )
    client = FakeYouTrackClient(
        [article],
        {"226-0": article},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        article_comments={"226-0": [_comment("8-1", "article owner", created=100)]},
        issue_comments={"3-0": [_comment("7-1", "issue owner", created=100)]},
        article_attachments={"226-0": [article_attachment]},
        issue_attachments={"3-0": [issue_attachment]},
    )
    adapter = _adapter(_settings(), client=client)

    payloads = await _payloads_by_kind(adapter)

    suffix = " — created 2023-11-14T22:13:20Z — updated 2023-11-14T22:13:29Z"
    assert f"`10-issue` — `text/plain` — 11 B{suffix}" in payloads[KIND_ISSUE]
    assert f"`10-article` — `text/plain` — 12 B{suffix}" in payloads[KIND_ARTICLE]


@pytest.mark.asyncio
async def test_attachment_without_timestamps_renders_the_legacy_line() -> None:
    attachment = _parent_attachment(
        id="10-legacy",
        name="legacy.bin",
        mimeType="application/octet-stream",
        size=7,
    )

    markdown = await _issue_attachment_markdown(attachment)

    legacy_line = "- **legacy.bin** — id `10-legacy` — `application/octet-stream` — 7 B"
    assert legacy_line in markdown.splitlines()


@pytest.mark.asyncio
async def test_attachment_with_equal_created_and_updated_omits_updated() -> None:
    attachment = _parent_attachment(
        id="10-equal",
        name="equal.txt",
        size=5,
        created=1_700_000_000_000,
        updated=1_700_000_000_000,
    )

    markdown = await _issue_attachment_markdown(attachment)

    line = "- **equal.txt** — id `10-equal` — `text/plain` — 5 B — created 2023-11-14T22:13:20Z"
    assert line in markdown.splitlines()
    assert " — updated " not in line


@pytest.mark.asyncio
async def test_attachment_without_created_renders_updated_alone() -> None:
    attachment = _parent_attachment(
        id="10-updated-only",
        name="updated-only.txt",
        size=6,
        updated=1_700_000_009_000,
    )

    markdown = await _issue_attachment_markdown(attachment)

    line = (
        "- **updated-only.txt** — id `10-updated-only` — `text/plain` — 6 B"
        " — updated 2023-11-14T22:13:29Z"
    )
    assert line in markdown.splitlines()
    assert "created " not in line


@pytest.mark.asyncio
async def test_attachment_bool_size_is_rejected() -> None:
    attachment = _parent_attachment(id="10-bool", name="bool-size.txt", size=True)
    adapter = _issue_attachment_adapter(attachment)

    with pytest.raises(YouTrackError, match="attachment size"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_attachment_updated_only_change_reaches_markdown_and_token() -> None:
    # The token already hashed these timestamps through ``related``; the
    # rendered Markdown is the coverage this increment adds.
    original = _article()
    changed = deepcopy(original)
    attachments = changed["attachments"]
    assert isinstance(attachments, list)
    assert isinstance(attachments[0], dict)
    attachments[0]["updated"] = 1_700_000_009_999

    original_ref = await _single_ref(original)
    changed_ref = await _single_ref(changed)

    assert original_ref.change_token is not None
    assert changed_ref.change_token is not None
    assert original_ref.change_token != changed_ref.change_token

    assert await _single_markdown(original) != await _single_markdown(changed)


@pytest.mark.asyncio
async def test_live_change_token_prefix_tracks_projection_version() -> None:
    expected_prefix = f"youtrack-composite-v{youtrack_adapter_module._YOUTRACK_PROJECTION_VERSION}:"
    assert expected_prefix == "youtrack-composite-v3:"
    ref = await _single_ref(_article())

    assert ref.change_token is not None
    assert ref.change_token.startswith(expected_prefix)


@pytest.mark.asyncio
async def test_fetch_defers_article_that_changed_after_stable_snapshot() -> None:
    summary = _article(summary="Old title", content=None)
    detail = _article(summary="Current title")
    client = FakeYouTrackClient([summary], {"226-0": detail})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    assert isinstance(await adapter.fetch(ref), SourceFetchDeferred)


@pytest.mark.asyncio
async def test_public_parent_becoming_restricted_during_fetch_requires_delete() -> None:
    summary = _article(content=None)
    restricted = _article(
        visibility={
            "$type": "LimitedVisibility",
            "permittedUsers": [],
            "permittedGroups": [{"id": "3-9"}],
        }
    )
    client = FakeYouTrackClient([summary], {"226-0": restricted})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is False


@pytest.mark.asyncio
async def test_fetch_defers_article_that_moved_outside_project_filter() -> None:
    summary = _article()
    detail = _article(
        project={
            "id": "0-8",
            "shortName": "PRIVATE",
            "name": "Private",
            "archived": False,
        }
    )
    client = FakeYouTrackClient([summary], {"226-0": detail})
    adapter = _adapter(_settings(youtrack_projects="DOCS"), client=client)
    ref = await _first(adapter.iter_items())

    assert isinstance(await adapter.fetch(ref), SourceFetchDeferred)


@pytest.mark.asyncio
async def test_fetch_preserves_indented_markdown_code_block() -> None:
    article = _article(content="\n    indented code\n", attachments=[])
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    markdown = fetched.item.payload.decode("utf-8")
    assert markdown.startswith("# Neutral Overview\n\n## Metadata")
    assert markdown.endswith("\n\n    indented code\n")


@pytest.mark.asyncio
@pytest.mark.parametrize("content", [None, "", "  \n"])
async def test_fetch_never_leaves_stale_content_for_empty_article(content: object) -> None:
    article = _article(content=content, attachments=[])
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    markdown = fetched.item.payload.decode("utf-8")
    assert markdown.startswith("# Neutral Overview\n\n## Metadata")
    assert "indented code" not in markdown


@pytest.mark.asyncio
async def test_project_filters_and_archived_projects_are_applied_before_sync() -> None:
    included = _article()
    excluded = _article(
        id="226-1",
        idReadable="ARCHIVE-A-1",
        project={"id": "0-8", "shortName": "ARCHIVE", "name": "Archive", "archived": False},
    )
    archived = _article(
        id="226-2",
        idReadable="DOCS-A-2",
        project={"id": "0-7", "shortName": "DOCS", "name": "Documentation", "archived": True},
    )
    client = FakeYouTrackClient(
        [included, excluded, archived],
        {"226-0": included, "226-1": excluded, "226-2": archived},
    )
    adapter = _adapter(
        _settings(youtrack_projects="DOCS", youtrack_exclude_projects="ARCHIVE"),
        client=client,
    )

    refs = [ref async for ref in adapter.iter_items()]

    assert [ref.item_id for ref in refs] == ["226-0"]


@pytest.mark.parametrize(
    ("archived_present", "archived_value"),
    [
        pytest.param(False, None, id="missing"),
        pytest.param(True, None, id="null"),
        pytest.param(True, "false", id="string"),
        pytest.param(True, 0, id="integer"),
    ],
)
@pytest.mark.asyncio
async def test_malformed_project_archived_flag_aborts_census(
    archived_present: bool,
    archived_value: object,
) -> None:
    article = _article()
    project = deepcopy(article["project"])
    assert isinstance(project, dict)
    if archived_present:
        project["archived"] = archived_value
    else:
        project.pop("archived")
    article["project"] = project
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(_settings(), client=client)

    with pytest.raises(YouTrackError, match="boolean archived flag"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_parent_unreadable_after_listing_requires_fail_closed_delete() -> None:
    article = _article()
    client = FakeYouTrackClient([article], {})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)
    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is False


@pytest.mark.asyncio
async def test_iter_items_emits_issue_kind_with_routing_facts_and_composite_token() -> None:
    issue = _issue()
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert len(refs) == 1
    ref = refs[0]
    assert ref.kind == KIND_ISSUE
    assert ref.item_id == "3-0"
    assert ref.external_key == "youtrack:issue:3-0"
    assert ref.change_token is not None
    assert ref.change_token.startswith("youtrack-composite-v3:")
    assert len(ref.change_token.rsplit(":", 1)[1]) == 64
    assert ref.materialization_enforcement is SourceMaterializationEnforcement.REQUIRED
    assert ref.routing_facts == {
        "kind": KIND_ISSUE,
        "issue_id": "3-0",
        "issue_id_readable": "IRG-83",
        "project_id": "0-7",
        "project": "IRG",
        "project_short_name": "IRG",
        "project_name": "IronRAG",
        "custom_field:Assignee": "reviewer",
        "custom_field:Priority": "Major",
        "custom_field:State": "In Progress",
        "custom_field:Type": "Change",
        "tag": ["alpha", "beta"],
        "reporter_login": "writer",
    }


@pytest.mark.asyncio
async def test_iter_items_emits_both_kinds_together() -> None:
    article = _article()
    issue = _issue()
    client = FakeYouTrackClient(
        [article], {"226-0": article}, issue_summaries=[issue], issue_details={"3-0": issue}
    )
    adapter = _adapter(_settings(), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert {ref.kind for ref in refs} == {KIND_ARTICLE, KIND_ISSUE}
    assert adapter.primary_kinds == (KIND_ARTICLE, KIND_ISSUE)


def test_complete_primary_censuses_require_missing_deletion() -> None:
    adapter = _adapter(_settings(), client=FakeYouTrackClient([], {}))

    assert {kind: adapter.missing_enforcement(kind) for kind in adapter.kinds} == {
        KIND_ARTICLE: SourceDeleteEnforcement.REQUIRED,
        KIND_ISSUE: SourceDeleteEnforcement.REQUIRED,
    }
    with pytest.raises(ValueError, match="unsupported YouTrack item kind"):
        adapter.missing_enforcement("future-kind")


@pytest.mark.asyncio
async def test_iter_items_census_counts_restricted_children_of_a_public_parent() -> None:
    issue = _issue(commentsCount=1)
    link = {
        "id": "80-1",
        "direction": "OUTWARD",
        "linkType": {"id": "70-1", "name": "Depend"},
        "issues": [{"id": "3-1"}],
    }
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={
            "3-0": [
                _comment(
                    "7-1",
                    "restricted comment marker",
                    created=100,
                    visibility=dict(_LIMITED_VISIBILITY),
                )
            ]
        },
        issue_links={"3-0": [link]},
        issue_link_targets={
            ("3-0", "80-1"): [
                _link_target("3-1", "IRG-84"),
                {**_link_target("3-2", "PRIVATE-1"), "visibility": dict(_LIMITED_VISIBILITY)},
            ]
        },
    )
    adapter = _adapter(_settings(youtrack_projects="IRG"), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert len(refs) == 1
    assert adapter._visibility_census[KIND_ISSUE] == {
        "kind": KIND_ISSUE,
        "in_scope": 1,
        "restricted_parents": 0,
        "comments": 1,
        "restricted_comments": 1,
        "attachments": 0,
        "restricted_attachments": 0,
        "link_targets": 2,
        "restricted_link_targets": 1,
        "unknown_visibility_items": 0,
    }
    assert adapter._visibility_census[KIND_ARTICLE]["in_scope"] == 0


@pytest.mark.asyncio
async def test_iter_items_census_restricted_parent_reads_no_children() -> None:
    article = _article(visibility=dict(_LIMITED_VISIBILITY))
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(_settings(), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert len(refs) == 1
    census = adapter._visibility_census[KIND_ARTICLE]
    assert census["in_scope"] == 1
    assert census["restricted_parents"] == 1
    assert census["comments"] == 0
    assert census["attachments"] == 0
    assert census["link_targets"] == 0
    # A restricted access chain means the related collections are never read,
    # so a restricted parent cannot leak child bytes through the census either.
    assert client.comment_reads == []


@pytest.mark.asyncio
async def test_iter_items_census_flags_unprovable_child_visibility() -> None:
    issue = _issue(commentsCount=1)
    comment = _comment("7-1", "unprovable marker", created=100)
    del comment["visibility"]
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [comment]},
    )
    adapter = _adapter(_settings(), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert len(refs) == 1
    census = adapter._visibility_census[KIND_ISSUE]
    assert census["comments"] == 1
    assert census["restricted_comments"] == 1
    assert census["unknown_visibility_items"] == 1
    assert "unprovable marker" not in repr(refs[0].raw)


@pytest.mark.asyncio
async def test_visibility_census_log_fields_carry_no_source_text() -> None:
    article = _article()
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(_settings(), client=client)

    _ = [ref async for ref in adapter.iter_items()]

    census = adapter._visibility_census[KIND_ARTICLE]
    assert census["kind"] == KIND_ARTICLE
    assert census["in_scope"] == 1
    assert census["attachments"] == 1
    assert census["restricted_attachments"] == 0
    rendered = repr(census)
    assert "diagram.png" not in rendered
    assert "sign=must-not-be-indexed" not in rendered


@pytest.mark.asyncio
async def test_issue_snapshot_tolerates_content_drift_between_validation_passes() -> None:
    stable = _issue()
    drifted = _issue()
    drifted["customFields"] = [
        {"name": "Type", "value": {"id": "t-1", "name": "Change"}},
        {"name": "State", "value": {"id": "s-2", "name": "Fixed"}},
        {"name": "Priority", "value": {"id": "p-1", "name": "Major"}},
        {"name": "Assignee", "value": None},
    ]

    class DriftingIssueClient(FakeYouTrackClient):
        def __init__(self) -> None:
            super().__init__([], {}, issue_summaries=[stable], issue_details={"3-0": drifted})
            self._passes = [[stable], [drifted]]

        async def list_issues(self) -> AsyncIterator[dict[str, object]]:
            current = self._passes.pop(0)
            for issue in current:
                yield deepcopy(issue)

    adapter = _adapter(_settings(), client=DriftingIssueClient())

    # Must NOT raise: the issue id set is stable across passes even though a
    # normal field edit landed mid-sweep. Article snapshotting requires exact
    # token equality; issue snapshotting only requires the id set to hold.
    refs = [ref async for ref in adapter.iter_items()]
    assert [ref.item_id for ref in refs] == ["3-0"]


@pytest.mark.asyncio
async def test_issue_snapshot_still_aborts_when_the_id_set_itself_changes() -> None:
    first_pass = [_issue()]
    second_pass = [_issue(), _issue(id="3-1", idReadable="IRG-84")]

    class AppearingIssueClient(FakeYouTrackClient):
        def __init__(self) -> None:
            super().__init__([], {})
            self._passes = [first_pass, second_pass]

        async def list_issues(self) -> AsyncIterator[dict[str, object]]:
            current = self._passes.pop(0)
            for issue in current:
                yield deepcopy(issue)

    adapter = _adapter(_settings(), client=AppearingIssueClient())

    with pytest.raises(YouTrackError, match="issue snapshot changed between validation passes"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_fetch_renders_issue_title_body_project_and_stable_identity() -> None:
    issue = _issue()
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    item = fetched.item
    assert item.title == "Add issue sync"
    assert item.mime_type == "text/markdown"
    assert item.file_name == "IRG-83.md"
    assert item.document_hint == "https://tracker.example.com/youtrack/issue/IRG-83"
    markdown = item.payload.decode("utf-8")
    assert markdown.startswith("# Add issue sync\n")
    assert "IRG-83" in markdown
    assert "IronRAG" in markdown
    assert "Change" in markdown
    assert "In Progress" in markdown
    assert "Major" in markdown
    assert "Body with **markdown**" in markdown


@pytest.mark.asyncio
async def test_issue_comments_count_must_be_a_non_negative_integer() -> None:
    for invalid in (None, True, -1, 1.5, "1"):
        issue = _issue(commentsCount=invalid)
        adapter = _adapter(
            _settings(),
            client=FakeYouTrackClient(
                [], {}, issue_summaries=[issue], issue_details={"3-0": issue}
            ),
        )

        with pytest.raises(YouTrackError, match="commentsCount"):
            _ = [ref async for ref in adapter.iter_items()]

    missing = _issue()
    del missing["commentsCount"]
    adapter = _adapter(
        _settings(),
        client=FakeYouTrackClient(
            [], {}, issue_summaries=[missing], issue_details={"3-0": missing}
        ),
    )
    with pytest.raises(YouTrackError, match="commentsCount"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_issue_comment_count_mismatch_requires_security_delete() -> None:
    issue = _issue(commentsCount=2)
    one_accessible_comment = _comment("7-1", "visible marker", created=100)
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [one_accessible_comment]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.exclusion is SourceExclusion.NONE
    assert fetched.cacheable is True


@pytest.mark.asyncio
async def test_fetch_defers_issue_that_moved_outside_project_filter() -> None:
    summary = _issue()
    detail = _issue(project={"id": "0-8", "shortName": "OTHER", "name": "Other", "archived": False})
    client = FakeYouTrackClient([], {}, issue_summaries=[summary], issue_details={"3-0": detail})
    adapter = _adapter(_settings(youtrack_projects="IRG"), client=client)
    ref = await _first(adapter.iter_items())

    assert isinstance(await adapter.fetch(ref), SourceFetchDeferred)


@pytest.mark.asyncio
async def test_require_tag_gates_both_kinds_and_is_anded_with_project_filter() -> None:
    tagged_article = _article(tags=[{"id": "6-1", "name": "ironrag-sync"}])
    untagged_article = _article(id="226-1", idReadable="DOCS-A-2", tags=[])
    tagged_issue = _issue(tags=[{"id": "6-1", "name": "ironrag-sync"}])
    untagged_issue = _issue(id="3-1", idReadable="IRG-84", tags=[])
    client = FakeYouTrackClient(
        [tagged_article, untagged_article],
        {"226-0": tagged_article, "226-1": untagged_article},
        issue_summaries=[tagged_issue, untagged_issue],
        issue_details={"3-0": tagged_issue, "3-1": untagged_issue},
    )
    adapter = _adapter(_settings(youtrack_require_tag="ironrag-sync"), client=client)

    refs = [ref async for ref in adapter.iter_items()]

    assert {(ref.kind, ref.item_id) for ref in refs} == {
        (KIND_ARTICLE, "226-0"),
        (KIND_ISSUE, "3-0"),
    }


@pytest.mark.parametrize(
    ("tags_present", "tags_value"),
    [
        pytest.param(False, None, id="missing"),
        pytest.param(True, {}, id="object"),
        pytest.param(True, [None], id="non-object-row"),
        pytest.param(True, [{}], id="missing-id-and-name"),
        pytest.param(True, [{"id": "6-1"}], id="missing-name"),
        pytest.param(True, [{"name": "ironrag-sync"}], id="missing-id"),
        pytest.param(
            True,
            [
                {"id": "6-1", "name": "ironrag-sync"},
                {"id": "6-1", "name": "other"},
            ],
            id="duplicate-id",
        ),
    ],
)
@pytest.mark.asyncio
async def test_malformed_required_tag_projection_aborts_census(
    tags_present: bool,
    tags_value: object,
) -> None:
    article = _article()
    if tags_present:
        article["tags"] = tags_value
    else:
        article.pop("tags")
    client = FakeYouTrackClient([article], {"226-0": article})
    adapter = _adapter(
        _settings(youtrack_require_tag="ironrag-sync"),
        client=client,
    )

    with pytest.raises(YouTrackError, match=r"tags|payload did not contain"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_polymorphic_custom_field_with_unexpected_shape_fails_closed() -> None:
    issue = _issue()
    issue["customFields"] = [
        {"name": "Type", "value": {"privateShape": "Change"}},
    ]
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client)

    with pytest.raises(YouTrackError, match="custom field"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_missing_requested_custom_field_collection_aborts_census() -> None:
    issue = _issue()
    issue.pop("customFields")
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client)

    with pytest.raises(YouTrackError, match="customFields"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_omitted_unrequested_custom_field_collection_is_valid() -> None:
    issue = _issue()
    issue.pop("customFields")
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(youtrack_custom_fields=""), client=client)

    ref = await _first(adapter.iter_items())

    assert not any(key.startswith("custom_field:") for key in ref.routing_facts)


@pytest.mark.asyncio
async def test_multi_value_custom_field_keeps_sorted_values_for_generic_routing() -> None:
    issue = _issue()
    issue["customFields"] = [
        {"name": "Assignee", "value": [{"login": "bravo"}, {"login": "alpha"}]},
    ]
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())

    assert ref.routing_facts["custom_field:Assignee"] == ["alpha", "bravo"]


@pytest.mark.asyncio
async def test_configured_custom_field_has_generic_namespaced_routing_fact() -> None:
    issue = _issue()
    issue["customFields"] = [
        {"name": "Product / Area", "value": {"id": "pa-1", "name": "IronRAG"}},
    ]
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(
        _settings(youtrack_custom_fields="Product / Area"),
        client=client,
    )

    ref = await _first(adapter.iter_items())

    assert ref.routing_facts["custom_field:Product / Area"] == "IronRAG"
    assert "type" not in ref.routing_facts
    assert "state" not in ref.routing_facts
    assert "priority" not in ref.routing_facts
    assert "assignee_login" not in ref.routing_facts


@pytest.mark.asyncio
async def test_unconfigured_custom_field_never_crosses_adapter_boundary() -> None:
    issue = _issue()
    issue["serverOverreturnedTopLevel"] = "TOP-LEVEL-MUST-NOT-CROSS"
    issue["customFields"] = [
        {
            "name": "Type",
            "value": {
                "id": "t-1",
                "name": "Change",
                "email": "allowed-field-email-must-not-cross@example.test",
                "fullName": "ALLOWED-NESTED-LEAF-MUST-NOT-CROSS",
            },
        },
        {
            "name": "Private Field",
            "value": {"id": "private-1", "name": "MUST-NOT-CROSS"},
        },
    ]
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(youtrack_custom_fields="Type"), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert ref.routing_facts["custom_field:Type"] == "Change"
    assert "Private Field" not in repr(ref.raw)
    assert "MUST-NOT-CROSS" not in repr(ref.raw)
    assert "allowed-field-email-must-not-cross" not in repr(ref.raw)
    assert "ALLOWED-NESTED-LEAF-MUST-NOT-CROSS" not in repr(ref.raw)
    assert "TOP-LEVEL-MUST-NOT-CROSS" not in repr(ref.raw)
    assert "Private Field" not in fetched.item.payload.decode("utf-8")
    assert "MUST-NOT-CROSS" not in fetched.item.payload.decode("utf-8")

    changed_private = deepcopy(issue)
    private_fields = changed_private["customFields"]
    assert isinstance(private_fields, list)
    assert isinstance(private_fields[1], dict)
    private_fields[1]["value"] = {
        "id": "private-2",
        "name": "A-DIFFERENT-PRIVATE-MARKER",
    }
    changed_client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[changed_private],
        issue_details={"3-0": changed_private},
    )
    changed_ref = await _first(
        _adapter(
            _settings(youtrack_custom_fields="Type"),
            client=changed_client,
        ).iter_items()
    )
    assert changed_ref.change_token == ref.change_token


@pytest.mark.asyncio
async def test_unconfigured_custom_field_value_is_not_copied() -> None:
    class ForbiddenCopy:
        def __deepcopy__(self, memo: dict[int, object]) -> object:
            raise AssertionError("unconfigured custom field value was copied")

    issue = _issue()
    issue["customFields"] = [
        {"name": "Type", "value": {"id": "t-1", "name": "Change"}},
        {"name": "Private Field", "value": ForbiddenCopy()},
    ]

    class BoundaryClient(FakeYouTrackClient):
        async def list_issues(self) -> AsyncIterator[dict[str, object]]:
            yield issue

        async def get_issue(self, issue_id: str) -> dict[str, object]:
            self.issue_fetches.append(issue_id)
            return issue

    client = BoundaryClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(
        _settings(youtrack_custom_fields="Type"),
        client=client,
    )

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert ref.routing_facts["custom_field:Type"] == "Change"
    assert "Private Field" not in repr(ref.raw)


@pytest.mark.asyncio
async def test_close_releases_vendor_client() -> None:
    client = FakeYouTrackClient([], {})
    adapter = _adapter(_settings(), client=client)

    await adapter.close()

    assert client.closed is True


class ChangingSnapshotClient(FakeYouTrackClient):
    def __init__(self) -> None:
        first = _article()
        second = _article(summary="Changed during pagination")
        super().__init__([first], {"226-0": second})
        self._passes = [[first], [second]]

    async def list_articles(self) -> AsyncIterator[dict[str, object]]:
        current = self._passes.pop(0)
        for article in current:
            yield deepcopy(article)


class ChangingIdentityClient(FakeYouTrackClient):
    def __init__(self) -> None:
        article = _article()
        super().__init__([article], {"226-0": article})
        self._identities = [
            {"id": "1-2", "login": "writer"},
            {"id": "1-3", "login": "other"},
        ]

    async def validate_credentials(self) -> dict[str, object]:
        self.validation_count += 1
        return self._identities.pop(0)


@pytest.mark.asyncio
async def test_inconsistent_validation_snapshots_abort_before_yielding() -> None:
    adapter = _adapter(_settings(), client=ChangingSnapshotClient())

    with pytest.raises(YouTrackError, match="changed between validation passes"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_authenticated_identity_change_after_snapshots_aborts_before_yielding() -> None:
    adapter = _adapter(_settings(), client=ChangingIdentityClient())

    with pytest.raises(YouTrackError, match="authenticated identity changed"):
        _ = [ref async for ref in adapter.iter_items()]


def _comment(
    comment_id: str,
    text: str,
    *,
    created: int,
    updated: int | None = None,
    visibility: dict[str, object] | None = None,
    deleted: bool = False,
) -> dict[str, object]:
    return {
        "id": comment_id,
        "text": text,
        "author": {"id": "1-8", "login": "commenter", "name": "Must not be indexed"},
        "created": created,
        "updated": updated if updated is not None else created,
        "deleted": deleted,
        "pinned": False,
        "attachments": [],
        "visibility": visibility or {"$type": "UnlimitedVisibility"},
    }


def _link_target(target_id: str, readable_id: str) -> dict[str, object]:
    return {
        "id": target_id,
        "idReadable": readable_id,
        "project": {
            "id": "0-7",
            "shortName": "IRG",
            "name": "IronRAG",
            "archived": False,
        },
        "tags": [],
        "visibility": {"$type": "UnlimitedVisibility"},
    }


@pytest.mark.asyncio
async def test_issue_comment_edit_changes_parent_token_without_parent_update() -> None:
    issue = _issue(updated=1_700_000_001_000, commentsCount=1)
    original = _comment("7-1", "before marker", created=100, updated=100)
    edited = _comment("7-1", "after marker", created=100, updated=200)

    first = _adapter(
        _settings(),
        client=FakeYouTrackClient(
            [],
            {},
            issue_summaries=[issue],
            issue_details={"3-0": issue},
            issue_comments={"3-0": [original]},
        ),
    )
    second = _adapter(
        _settings(),
        client=FakeYouTrackClient(
            [],
            {},
            issue_summaries=[issue],
            issue_details={"3-0": issue},
            issue_comments={"3-0": [edited]},
        ),
    )

    first_ref = await _first(first.iter_items())
    second_ref = await _first(second.iter_items())

    assert first_ref.change_token != second_ref.change_token
    fetched = await second.fetch(second_ref)
    assert isinstance(fetched, SourceFetchMaterialized)
    markdown = fetched.item.payload.decode("utf-8")
    assert "after marker" in markdown
    assert "before marker" not in markdown
    assert "7-1" in markdown
    assert "commenter" in markdown


@pytest.mark.asyncio
async def test_comment_order_is_canonical_created_then_id() -> None:
    issue = _issue(commentsCount=3)
    comments = [
        _comment("7-9", "third", created=200),
        _comment("7-2", "second", created=100),
        _comment("7-1", "first", created=100),
    ]
    reversed_client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": list(reversed(comments))},
    )
    unordered_client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": comments},
    )
    first_adapter = _adapter(_settings(), client=reversed_client)
    second_adapter = _adapter(_settings(), client=unordered_client)

    first_ref = await _first(first_adapter.iter_items())
    second_ref = await _first(second_adapter.iter_items())
    first_fetch = await first_adapter.fetch(first_ref)
    second_fetch = await second_adapter.fetch(second_ref)

    assert first_ref.change_token == second_ref.change_token
    assert isinstance(first_fetch, SourceFetchMaterialized)
    assert isinstance(second_fetch, SourceFetchMaterialized)
    assert first_fetch.item.payload == second_fetch.item.payload
    markdown = first_fetch.item.payload.decode("utf-8")
    assert markdown.index("first") < markdown.index("second") < markdown.index("third")


@pytest.mark.asyncio
async def test_comment_addition_appends_without_rewriting_prior_bytes() -> None:
    # IRG-197: an added comment must leave every earlier byte of the rendered
    # document untouched so the server reuses the checksum-identical chunks.
    created = 1_700_000_000_000
    first_comment = _comment("7-1", "first marker", created=created)
    added_comment = _comment("7-2", "second marker", created=created + 1_000)
    before_issue = _issue(commentsCount=1, updated=created + 1_000)
    # The comment add rewrites ``updated`` and ``updater`` on the issue; the
    # volatile parent fields must not leak into the rendered body.
    after_issue = _issue(
        commentsCount=2,
        updated=created + 2_000,
        updater={"id": "1-9", "login": "commenter", "name": "Must not be indexed"},
    )

    before = await _issue_markdown(before_issue, [first_comment])
    # Comments arrive out of order; the renderer imposes canonical order.
    after = await _issue_markdown(after_issue, [added_comment, first_comment])

    assert after.startswith(before)
    appended = after[len(before) :]
    assert appended.startswith("\n### Comment `7-2`")
    assert "second marker" in appended
    assert "first marker" not in appended


@pytest.mark.asyncio
async def test_comment_updated_line_only_records_a_real_edit() -> None:
    untouched = _comment("7-1", "untouched marker", created=100, updated=100)
    edited = _comment("7-2", "edited marker", created=100, updated=200)
    issue = _issue(commentsCount=2)

    markdown = await _issue_markdown(issue, [untouched, edited])

    blocks = markdown.split("### Comment `7-2`")
    assert len(blocks) == 2
    first_block, second_block = blocks
    assert "- **Updated:**" not in first_block
    assert "- **Created:**" in second_block
    assert "- **Updated:**" in second_block


@pytest.mark.asyncio
async def test_parent_metadata_omits_volatile_updated_fields() -> None:
    issue = _issue(
        updated=1_700_000_002_000,
        resolved=1_700_000_003_000,
        updater={"id": "1-9", "login": "closer", "name": "Closer"},
    )
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    markdown = fetched.item.payload.decode("utf-8")
    assert "- **Created:** 2023-11-14T22:13:20Z" in markdown
    assert "- **Resolved:** 2023-11-14T22:13:23Z" in markdown
    assert "- **Reporter:** `writer`" in markdown
    assert "**Updated:**" not in markdown
    assert "**Updated by:**" not in markdown


@pytest.mark.asyncio
async def test_restricted_and_deleted_comments_never_enter_payload() -> None:
    issue = _issue(commentsCount=3)
    comments = [
        _comment("7-1", "visible marker", created=100),
        _comment(
            "7-2",
            "restricted marker",
            created=200,
            visibility={
                "$type": "LimitedVisibility",
                "permittedUsers": [{"id": "1-9", "login": "private-reader"}],
                "permittedGroups": [],
            },
        ),
        _comment("7-3", "deleted marker", created=300, deleted=True),
    ]
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": comments},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    markdown = fetched.item.payload.decode("utf-8")
    assert "visible marker" in markdown
    assert "restricted marker" not in markdown
    assert "private-reader" not in markdown
    assert "deleted marker" not in markdown
    assert "restricted marker" not in repr(ref.raw)
    assert "private-reader" not in repr(ref.raw)
    assert "deleted marker" not in repr(ref.raw)


@pytest.mark.asyncio
async def test_limited_article_ancestor_requires_security_delete() -> None:
    parent = _article(
        id="226-parent",
        idReadable="DOCS-A-0",
        parentArticle=None,
        visibility={
            "$type": "LimitedVisibility",
            "permittedUsers": [],
            "permittedGroups": [{"id": "3-1", "name": "restricted"}],
        },
    )
    child = _article(
        parentArticle={"id": "226-parent", "idReadable": "DOCS-A-0", "summary": "Root"},
        visibility={"$type": "UnlimitedVisibility"},
    )
    client = FakeYouTrackClient(
        [parent, child],
        {"226-parent": parent, "226-0": child},
    )
    adapter = _adapter(_settings(), client=client)

    refs = {ref.item_id: ref async for ref in adapter.iter_items()}
    result = await adapter.fetch(refs["226-0"])

    assert isinstance(result, SourceFetchDelete)
    assert result.enforcement is SourceDeleteEnforcement.REQUIRED
    assert result.exclusion is SourceExclusion.LIMITED_VISIBILITY
    assert result.cacheable is True
    assert "restricted marker" not in result.reason


@pytest.mark.asyncio
async def test_article_comment_is_embedded_in_parent_document() -> None:
    article = _article(attachments=[])
    comment = _comment("8-1", "article comment marker", created=100)
    client = FakeYouTrackClient(
        [article],
        {"226-0": article},
        article_comments={"226-0": [comment]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    markdown = fetched.item.payload.decode("utf-8")
    assert "article comment marker" in markdown
    assert "Comment `8-1`" in markdown
    assert "commenter" in markdown


@pytest.mark.asyncio
async def test_deleted_comment_changes_token_and_removes_old_text() -> None:
    issue = _issue(commentsCount=1)
    visible = _comment("7-1", "remove me marker", created=100, updated=100)
    deleted = _comment("7-1", "must be ignored", created=100, updated=200, deleted=True)
    visible_client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [visible]},
    )
    deleted_client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [deleted]},
    )
    visible_adapter = _adapter(_settings(), client=visible_client)
    deleted_adapter = _adapter(_settings(), client=deleted_client)

    visible_ref = await _first(visible_adapter.iter_items())
    deleted_ref = await _first(deleted_adapter.iter_items())
    fetched = await deleted_adapter.fetch(deleted_ref)

    assert visible_ref.change_token != deleted_ref.change_token
    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    assert "remove me marker" not in fetched.item.payload.decode("utf-8")
    assert "must be ignored" not in fetched.item.payload.decode("utf-8")
    assert "must be ignored" not in repr(deleted_ref.raw)


@pytest.mark.asyncio
async def test_unknown_comment_visibility_requires_security_delete() -> None:
    issue = _issue(commentsCount=1)
    comment = _comment(
        "7-1",
        "unknown visibility marker",
        created=100,
        visibility={"$type": "FutureVisibility"},
    )
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [comment]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert "unknown visibility marker" not in fetched.reason


@pytest.mark.parametrize(
    ("field", "present", "value"),
    [
        pytest.param("deleted", False, None, id="missing-deleted"),
        pytest.param("deleted", True, None, id="null-deleted"),
        pytest.param("deleted", True, "false", id="string-deleted"),
        pytest.param("pinned", False, None, id="missing-pinned"),
        pytest.param("pinned", True, None, id="null-pinned"),
        pytest.param("pinned", True, "false", id="string-pinned"),
    ],
)
@pytest.mark.asyncio
async def test_ambiguous_public_comment_lifecycle_requires_security_delete(
    field: str,
    present: bool,
    value: object,
) -> None:
    issue = _issue(commentsCount=1)
    comment = _comment("7-private", "comment lifecycle secret", created=100)
    if present:
        comment[field] = value
    else:
        comment.pop(field)
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [comment]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert "comment lifecycle secret" not in repr(ref.raw)
    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert "comment lifecycle secret" not in fetched.reason


@pytest.mark.asyncio
async def test_attachment_ownership_is_unique_and_restricted_metadata_is_omitted() -> None:
    issue = _issue(commentsCount=1)
    comment = _comment("7-1", "attachment owner", created=100)
    attachments = [
        {
            "id": "10-1",
            "name": "parent.txt",
            "mimeType": "text/plain",
            "size": 10,
            "created": 100,
            "updated": 100,
            "draft": False,
            "removed": False,
            "comment": None,
            "visibility": {"$type": "UnlimitedVisibility"},
            "url": "https://must-not-enter.example/parent",
        },
        {
            "id": "10-2",
            "name": "comment.txt",
            "mimeType": "text/plain",
            "size": 20,
            "created": 100,
            "updated": 100,
            "draft": False,
            "removed": False,
            "comment": {"id": "7-1"},
            "visibility": {"$type": "UnlimitedVisibility"},
            "url": "https://must-not-enter.example/comment",
        },
        {
            "id": "10-3",
            "name": "restricted-name.txt",
            "comment": None,
            "visibility": {
                "$type": "LimitedVisibility",
                "permittedUsers": [{"id": "1-9", "login": "private-reader"}],
                "permittedGroups": [],
            },
        },
    ]
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [comment]},
        issue_attachments={"3-0": attachments},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    markdown = fetched.item.payload.decode("utf-8")
    assert markdown.count("parent.txt") == 1
    assert markdown.count("comment.txt") == 1
    assert "restricted-name.txt" not in markdown
    assert "private-reader" not in markdown
    assert "must-not-enter.example" not in markdown
    assert "restricted-name.txt" not in repr(ref.raw)
    assert "private-reader" not in repr(ref.raw)
    assert "must-not-enter.example" not in repr(ref.raw)


@pytest.mark.parametrize(
    ("comment_present", "comment_value"),
    [
        pytest.param(False, None, id="missing"),
        pytest.param(True, {}, id="missing-id"),
        pytest.param(True, "7-1", id="wrong-type"),
        pytest.param(True, [], id="list"),
    ],
)
@pytest.mark.asyncio
async def test_ambiguous_attachment_comment_owner_requires_security_delete(
    comment_present: bool,
    comment_value: object,
) -> None:
    issue = _issue()
    attachment: dict[str, object] = {
        "id": "10-private",
        "name": "restricted-comment-secret.txt",
        "mimeType": "text/plain",
        "size": 12,
        "visibility": {"$type": "UnlimitedVisibility"},
    }
    if comment_present:
        attachment["comment"] = comment_value
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_attachments={"3-0": [attachment]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert "restricted-comment-secret.txt" not in repr(ref.raw)
    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert "restricted-comment-secret.txt" not in fetched.reason


@pytest.mark.parametrize(
    ("field", "present", "value"),
    [
        pytest.param("draft", False, None, id="missing-draft"),
        pytest.param("draft", True, None, id="null-draft"),
        pytest.param("draft", True, "false", id="string-draft"),
        pytest.param("removed", False, None, id="missing-removed"),
        pytest.param("removed", True, None, id="null-removed"),
        pytest.param("removed", True, "false", id="string-removed"),
    ],
)
@pytest.mark.asyncio
async def test_ambiguous_public_attachment_lifecycle_requires_security_delete(
    field: str,
    present: bool,
    value: object,
) -> None:
    issue = _issue()
    attachment: dict[str, object] = {
        "id": "10-private",
        "name": "attachment lifecycle secret.txt",
        "mimeType": "text/plain",
        "size": 12,
        "draft": False,
        "removed": False,
        "comment": None,
        "visibility": {"$type": "UnlimitedVisibility"},
    }
    if present:
        attachment[field] = value
    else:
        attachment.pop(field)
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_attachments={"3-0": [attachment]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert "attachment lifecycle secret.txt" not in repr(ref.raw)
    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert "attachment lifecycle secret.txt" not in fetched.reason


_LIMITED_VISIBILITY: dict[str, object] = {
    "$type": "LimitedVisibility",
    "permittedUsers": [],
    "permittedGroups": [{"id": "3-9"}],
}
_CHILD_MARKER = "stale child marker"
_CHILD_SECRETS = {"comment": _CHILD_MARKER, "attachment": "diagram.png", "link-target": "IRG-84"}


def _child_transition_adapter(kind: str) -> tuple[FakeYouTrackClient, YouTrackAdapter]:
    if kind == "comment":
        issue = _issue(commentsCount=1)
        client = FakeYouTrackClient(
            [],
            {},
            issue_summaries=[issue],
            issue_details={"3-0": issue},
            issue_comments={"3-0": [_comment("7-1", _CHILD_MARKER, created=100)]},
        )
        return client, _adapter(_settings(), client=client)
    if kind == "attachment":
        article = _article()
        client = FakeYouTrackClient([article], {"226-0": article})
        return client, _adapter(_settings(), client=client)
    issue = _issue()
    link = {
        "id": "80-1",
        "direction": "OUTWARD",
        "linkType": {"id": "70-1", "name": "Depend"},
        "issues": [{"id": "3-1"}],
    }
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_links={"3-0": [link]},
        issue_link_targets={("3-0", "80-1"): [_link_target("3-1", "IRG-84")]},
    )
    return client, _adapter(_settings(youtrack_projects="IRG"), client=client)


def _tighten_child(kind: str, transition: str, client: FakeYouTrackClient) -> None:
    if kind == "comment":
        client.issue_comments["3-0"] = [
            _comment(
                "7-1",
                _CHILD_MARKER,
                created=100,
                deleted=transition == "deleted",
                visibility=None if transition == "deleted" else dict(_LIMITED_VISIBILITY),
            )
        ]
    elif kind == "attachment":
        attachment = client.article_attachments["226-0"][0]
        if transition == "removed":
            attachment["removed"] = True
        else:
            attachment["visibility"] = dict(_LIMITED_VISIBILITY)
    else:
        target = client.issue_link_targets[("3-0", "80-1")][0]
        if transition == "ineligible":
            target["project"] = {
                "id": "0-9",
                "shortName": "OTHER",
                "name": "Other",
                "archived": False,
            }
        else:
            target["visibility"] = dict(_LIMITED_VISIBILITY)


@pytest.mark.parametrize(
    ("kind", "transition", "exclusion"),
    [
        pytest.param(
            "comment", "limited", SourceExclusion.LIMITED_VISIBILITY, id="comment-limited"
        ),
        pytest.param("comment", "deleted", SourceExclusion.NONE, id="comment-deleted"),
        pytest.param(
            "attachment", "limited", SourceExclusion.LIMITED_VISIBILITY, id="attachment-limited"
        ),
        pytest.param("attachment", "removed", SourceExclusion.NONE, id="attachment-removed"),
        pytest.param(
            "link-target", "limited", SourceExclusion.LIMITED_VISIBILITY, id="link-target-limited"
        ),
        pytest.param(
            "link-target", "ineligible", SourceExclusion.NONE, id="link-target-ineligible"
        ),
    ],
)
@pytest.mark.asyncio
async def test_child_loss_during_fetch_classifies_visibility_exclusion(
    kind: str,
    transition: str,
    exclusion: SourceExclusion,
) -> None:
    client, adapter = _child_transition_adapter(kind)
    ref = await _first(adapter.iter_items())
    _tighten_child(kind, transition, client)

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is False
    assert fetched.exclusion is exclusion
    assert _CHILD_SECRETS[kind] not in fetched.reason


@pytest.mark.asyncio
async def test_issue_links_render_only_targets_with_unrestricted_visibility() -> None:
    issue = _issue()
    link = {
        "id": "80-1",
        "direction": "OUTWARD",
        "linkType": {
            "id": "70-1",
            "name": "Depend",
            "sourceToTarget": "depends on",
            "targetToSource": "is required for",
        },
        "issues": [{"id": "3-1"}],
    }
    targets = [
        _link_target("3-1", "IRG-84"),
        {
            "id": "3-2",
            "idReadable": "PRIVATE-1",
            "project": {
                "id": "0-9",
                "shortName": "PRIVATE",
                "name": "Private",
                "archived": False,
            },
            "tags": [],
            "visibility": {
                "$type": "LimitedVisibility",
                "permittedUsers": [],
                "permittedGroups": [{"id": "3-9", "name": "private-group"}],
            },
        },
    ]
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_links={"3-0": [link]},
        issue_link_targets={("3-0", "80-1"): targets},
    )
    adapter = _adapter(_settings(youtrack_projects="IRG"), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED
    markdown = fetched.item.payload.decode("utf-8")
    assert "depends on" in markdown
    assert "IRG-84" in markdown
    assert "PRIVATE-1" not in markdown
    assert "private-group" not in markdown
    assert "PRIVATE-1" not in repr(ref.raw)
    assert "private-group" not in repr(ref.raw)


@pytest.mark.parametrize(
    ("issues_present", "issues_value"),
    [
        pytest.param(False, None, id="missing"),
        pytest.param(True, {}, id="object"),
        pytest.param(True, [None], id="non-object-row"),
        pytest.param(True, [{}], id="missing-target-id"),
        pytest.param(True, [{"id": "3-1"}, {"id": "3-1"}], id="duplicate-target-id"),
    ],
)
@pytest.mark.asyncio
async def test_ambiguous_embedded_link_targets_require_security_delete(
    issues_present: bool,
    issues_value: object,
) -> None:
    issue = _issue()
    link: dict[str, object] = {
        "id": "80-private",
        "direction": "OUTWARD",
        "linkType": {"id": "70-1", "name": "Depend"},
    }
    if issues_present:
        link["issues"] = issues_value
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_links={"3-0": [link]},
    )
    adapter = _adapter(_settings(), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED


@pytest.mark.asyncio
async def test_embedded_link_target_missing_from_paginated_snapshot_requires_delete() -> None:
    issue = _issue()
    link = {
        "id": "80-raced",
        "direction": "OUTWARD",
        "linkType": {"id": "70-1", "name": "Depend"},
        "issues": [{"id": "3-before"}],
    }
    replacement = _link_target("3-after", "IRG-NEW")
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_links={"3-0": [link]},
        issue_link_targets={("3-0", "80-raced"): [replacement]},
    )
    adapter = _adapter(_settings(youtrack_projects="IRG"), client=client)

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED


@pytest.mark.asyncio
async def test_related_collections_must_stabilize_before_ref_is_yielded() -> None:
    issue = _issue(commentsCount=1)

    class DriftingCommentsClient(FakeYouTrackClient):
        def __init__(self) -> None:
            super().__init__([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
            self._comment_reads = 0

        async def list_issue_comments(self, issue_id: str) -> AsyncIterator[dict[str, object]]:
            self._comment_reads += 1
            yield _comment(
                "7-1",
                f"version-{self._comment_reads}",
                created=100,
                updated=self._comment_reads,
            )

    adapter = _adapter(_settings(), client=DriftingCommentsClient())

    with pytest.raises(YouTrackError, match="related collections did not stabilize"):
        _ = [ref async for ref in adapter.iter_items()]


@pytest.mark.asyncio
async def test_public_comment_becoming_inaccessible_during_fetch_requires_delete() -> None:
    issue = _issue(commentsCount=1)
    visible = _comment("7-1", "stale inaccessible marker", created=100)
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_comments={"3-0": [visible]},
    )
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())
    # YouTrack's comments endpoint promises all comments accessible to the
    # caller, so a visibility tightening can remove the comment from this
    # collection while Issue.commentsCount remains one.
    client.issue_comments["3-0"] = []

    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert "stale inaccessible marker" not in fetched.reason


def _comment_owned_attachments() -> list[dict[str, object]]:
    return [
        {
            "id": "10-1",
            "name": "parent.txt",
            "mimeType": "text/plain",
            "size": 10,
            "created": 100,
            "updated": 100,
            "draft": False,
            "removed": False,
            "comment": None,
            "visibility": {"$type": "UnlimitedVisibility"},
        },
        {
            "id": "10-2",
            "name": "comment-owned.txt",
            "mimeType": "text/plain",
            "size": 20,
            "created": 100,
            "updated": 100,
            "draft": False,
            "removed": False,
            "comment": {"id": "7-1"},
            "visibility": {"$type": "UnlimitedVisibility"},
        },
    ]


def _both_kinds_client(**overrides: object) -> FakeYouTrackClient:
    article = _article(attachments=[])
    issue = _issue(commentsCount=1)
    return FakeYouTrackClient(
        [article],
        {"226-0": article},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        article_comments={"226-0": [_comment("8-1", "article comment marker", created=100)]},
        issue_comments={"3-0": [_comment("7-1", "issue comment marker", created=100)]},
        **overrides,
    )


async def _payloads_by_kind(adapter: YouTrackAdapter) -> dict[str, str]:
    payloads: dict[str, str] = {}
    async for ref in adapter.iter_items():
        fetched = await adapter.fetch(ref)
        assert isinstance(fetched, SourceFetchMaterialized), fetched
        payloads[ref.kind] = fetched.item.payload.decode("utf-8")
    return payloads


@pytest.mark.asyncio
async def test_disabled_issue_comments_skip_issue_comment_reads_but_keep_article_comments() -> None:
    client = _both_kinds_client()
    adapter = _adapter(_settings(), client=client, policies=_policies(issue=False))

    payloads = await _payloads_by_kind(adapter)

    assert {kind for kind, _ in client.comment_reads} == {KIND_ARTICLE}
    assert "issue comment marker" not in payloads[KIND_ISSUE]
    assert "## Comments" not in payloads[KIND_ISSUE]
    assert "commenter" not in payloads[KIND_ISSUE]
    assert "article comment marker" in payloads[KIND_ARTICLE]
    assert "Comment `8-1`" in payloads[KIND_ARTICLE]


@pytest.mark.asyncio
async def test_disabled_article_comments_skip_article_comment_reads_but_keep_issue_comments() -> (
    None
):
    client = _both_kinds_client()
    adapter = _adapter(_settings(), client=client, policies=_policies(article=False))

    payloads = await _payloads_by_kind(adapter)

    assert {kind for kind, _ in client.comment_reads} == {KIND_ISSUE}
    assert "article comment marker" not in payloads[KIND_ARTICLE]
    assert "## Comments" not in payloads[KIND_ARTICLE]
    assert "issue comment marker" in payloads[KIND_ISSUE]
    assert "Comment `7-1`" in payloads[KIND_ISSUE]


@pytest.mark.asyncio
async def test_disabled_issue_comments_leave_comment_count_out_of_the_token() -> None:
    def adapter_for(count: int, *, include_comments: bool) -> YouTrackAdapter:
        issue = _issue(commentsCount=count)
        return _adapter(
            _settings(),
            client=FakeYouTrackClient(
                [], {}, issue_summaries=[issue], issue_details={"3-0": issue}
            ),
            policies=_policies(issue=include_comments),
        )

    disabled_one = await _first(adapter_for(1, include_comments=False).iter_items())
    disabled_five = await _first(adapter_for(5, include_comments=False).iter_items())
    enabled_zero = await _first(adapter_for(0, include_comments=True).iter_items())

    assert disabled_one.change_token == disabled_five.change_token
    # Toggling the setting is a projection change: the next sweep rematerializes.
    assert disabled_one.change_token != enabled_zero.change_token


@pytest.mark.asyncio
async def test_disabled_issue_comments_do_not_gate_on_comment_count_completeness() -> None:
    # commentsCount says 3 but no comment is read: with comments disabled that
    # is not an incomplete readable collection, so nothing is deleted.
    issue = _issue(commentsCount=3)
    client = FakeYouTrackClient([], {}, issue_summaries=[issue], issue_details={"3-0": issue})
    adapter = _adapter(_settings(), client=client, policies=_policies(issue=False))

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    assert fetched.enforcement is SourceMaterializationEnforcement.REQUIRED


@pytest.mark.asyncio
async def test_disabled_issue_comments_drop_comment_owned_attachments() -> None:
    issue = _issue(commentsCount=1)
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_attachments={"3-0": _comment_owned_attachments()},
    )
    adapter = _adapter(_settings(), client=client, policies=_policies(issue=False))

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    markdown = fetched.item.payload.decode("utf-8")
    assert "parent.txt" in markdown
    assert "comment-owned.txt" not in markdown
    assert "comment-owned.txt" not in repr(ref.raw)


@pytest.mark.asyncio
async def test_disabled_article_comments_drop_comment_owned_attachments() -> None:
    article = _article(attachments=[])
    client = FakeYouTrackClient(
        [article],
        {"226-0": article},
        article_attachments={"226-0": _comment_owned_attachments()},
    )
    adapter = _adapter(_settings(), client=client, policies=_policies(article=False))

    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)

    assert isinstance(fetched, SourceFetchMaterialized)
    markdown = fetched.item.payload.decode("utf-8")
    assert "parent.txt" in markdown
    assert "comment-owned.txt" not in markdown


@pytest.mark.asyncio
async def test_article_comment_edit_changes_parent_token_without_parent_update() -> None:
    article = _article(attachments=[])
    original = _comment("8-1", "before marker", created=100, updated=100)
    edited = _comment("8-1", "after marker", created=100, updated=200)
    first = _adapter(
        _settings(),
        client=FakeYouTrackClient(
            [article], {"226-0": article}, article_comments={"226-0": [original]}
        ),
    )
    second = _adapter(
        _settings(),
        client=FakeYouTrackClient(
            [article], {"226-0": article}, article_comments={"226-0": [edited]}
        ),
    )

    first_ref = await _first(first.iter_items())
    second_ref = await _first(second.iter_items())
    fetched = await second.fetch(second_ref)

    assert first_ref.change_token != second_ref.change_token
    assert isinstance(fetched, SourceFetchMaterialized)
    markdown = fetched.item.payload.decode("utf-8")
    assert "after marker" in markdown
    assert "before marker" not in markdown
    assert "commenter" in markdown


@pytest.mark.asyncio
async def test_disabled_comments_never_request_comment_endpoints() -> None:
    issue = _issue(commentsCount=1)
    article = _article(attachments=[])
    requested_paths: list[bytes] = []

    def handler(request: httpx.Request) -> httpx.Response:
        path = request.url.raw_path.split(b"?", 1)[0]
        requested_paths.append(path)
        skip = int(request.url.params.get("$skip", "0"))
        if path == b"/youtrack/api/users/me":
            return httpx.Response(200, json={"id": "1-2", "login": "writer", "guest": False})
        if path == b"/youtrack/api/articles":
            return httpx.Response(200, json=[article] if skip == 0 else [])
        if path == b"/youtrack/api/articles/226-0":
            return httpx.Response(200, json=article)
        if path == b"/youtrack/api/issues":
            return httpx.Response(200, json=[issue] if skip == 0 else [])
        if path == b"/youtrack/api/issues/3-0":
            return httpx.Response(200, json=issue)
        if path.endswith(b"/attachments") or path.endswith(b"/links"):
            return httpx.Response(200, json=[])
        raise AssertionError(f"unexpected request {path!r}")

    http = httpx.AsyncClient(
        base_url="https://tracker.example.com/youtrack/",
        transport=httpx.MockTransport(handler),
    )
    settings = _settings(youtrack_min_request_interval_seconds=0)
    adapter = _adapter(
        settings,
        client=YouTrackClient(settings, client=http),
        policies=_policies(issue=False, article=False),
    )

    payloads = await _payloads_by_kind(adapter)
    await http.aclose()

    assert set(payloads) == {KIND_ARTICLE, KIND_ISSUE}
    assert not any(path.endswith(b"/comments") for path in requested_paths)
    assert b"/youtrack/api/issues/3-0/attachments" in requested_paths
    assert b"/youtrack/api/articles/226-0/attachments" in requested_paths


_LIBRARY_REF = "default/youtrack-knowledge-base"


def _live_policy_table(tmp_path: Path) -> tuple[Path, PolicyOverrides, RoutingReloader]:
    """The framework wiring an adapter really runs under: file, table, reloader."""
    routing_path = tmp_path / "routing.yaml"
    routing_path.write_text(f"default: {{ library: {_LIBRARY_REF} }}\n", encoding="utf-8")
    target = ResolvedLibraryTarget(
        library_ref=_LIBRARY_REF,
        workspace_id=UUID("00000000-0000-0000-0000-000000000099"),
        library_id=UUID("00000000-0000-0000-0000-000000000001"),
    )
    router = Router(load_routing_config(routing_path), resolved_targets={_LIBRARY_REF: target})
    policies = router.build_policies(PushPolicy())

    async def resolve(refs: set[str]) -> dict[str, ResolvedLibraryTarget]:
        return {ref: target for ref in refs}

    reloader = RoutingReloader(
        path=routing_path,
        router=router,
        policies=policies,
        defaults=PushPolicy(),
        resolver=resolve,
        kinds=YouTrackAdapter.kinds,
    )
    return routing_path, policies, reloader


def _routing_with(kind: str, *, include_comments: bool) -> str:
    return (
        f"default: {{ library: {_LIBRARY_REF} }}\n"
        f"policies:\n  {kind}:\n    include_comments: {str(include_comments).lower()}\n"
    )


def _rewrite_routing(path: Path, body: str) -> None:
    path.write_text(body, encoding="utf-8")
    bumped = path.stat().st_mtime_ns + 1_000_000
    os.utime(path, ns=(bumped, bumped))


@pytest.mark.asyncio
async def test_routing_hot_reload_flips_comment_sync_and_change_tokens(tmp_path: Path) -> None:
    routing_path, policies, reloader = _live_policy_table(tmp_path)
    client = _both_kinds_client()
    adapter = _adapter(_settings(), client=client, policies=policies)

    before = {ref.kind: ref async for ref in adapter.iter_items()}
    assert {kind for kind, _ in client.comment_reads} == {KIND_ARTICLE, KIND_ISSUE}

    _rewrite_routing(routing_path, _routing_with(KIND_ISSUE, include_comments=False))
    assert await reloader.reload_if_changed() is True
    client.comment_reads.clear()

    after = {ref.kind: ref async for ref in adapter.iter_items()}

    # The same adapter instance now skips issue comments without a restart ...
    assert {kind for kind, _ in client.comment_reads} == {KIND_ARTICLE}
    # ... and the issue token moved so the next sweep rematerializes it,
    # while the untouched article keeps its token.
    assert after[KIND_ISSUE].change_token != before[KIND_ISSUE].change_token
    assert after[KIND_ARTICLE].change_token == before[KIND_ARTICLE].change_token
    payloads = await _payloads_by_kind(adapter)
    assert "issue comment marker" not in payloads[KIND_ISSUE]
    assert "article comment marker" in payloads[KIND_ARTICLE]


@pytest.mark.asyncio
async def test_rejected_routing_reload_keeps_comment_sync_unchanged(tmp_path: Path) -> None:
    routing_path, policies, reloader = _live_policy_table(tmp_path)
    client = _both_kinds_client()
    adapter = _adapter(_settings(), client=client, policies=policies)

    # A kind this adapter never emits is a typo that would otherwise leave
    # comments silently enabled; the reload is refused and the table stands.
    _rewrite_routing(routing_path, _routing_with("isue", include_comments=False))
    assert await reloader.reload_if_changed() is False
    assert policies.for_kind(KIND_ISSUE).include_comments is True

    payloads = await _payloads_by_kind(adapter)
    assert {kind for kind, _ in client.comment_reads} == {KIND_ARTICLE, KIND_ISSUE}
    assert "issue comment marker" in payloads[KIND_ISSUE]


@pytest.mark.asyncio
async def test_comment_sync_disabled_between_enumeration_and_fetch_fails_closed(
    tmp_path: Path,
) -> None:
    routing_path, policies, reloader = _live_policy_table(tmp_path)
    client = _both_kinds_client()
    adapter = _adapter(_settings(), client=client, policies=policies)
    refs = {ref.kind: ref async for ref in adapter.iter_items()}

    _rewrite_routing(routing_path, _routing_with(KIND_ISSUE, include_comments=False))
    assert await reloader.reload_if_changed() is True

    fetched = await adapter.fetch(refs[KIND_ISSUE])

    # Comments the enumerated ref proved indexable are gone from the live
    # projection. That is the same shape as a comment losing readability, and
    # it gets the same answer: nothing with comments is materialized under a
    # token that now means "no comments", the composite is removed now, and
    # the next sweep recreates it from the new token.
    assert isinstance(fetched, SourceFetchDelete)
    assert fetched.enforcement is SourceDeleteEnforcement.REQUIRED
    assert fetched.cacheable is False
    assert isinstance(await adapter.fetch(refs[KIND_ARTICLE]), SourceFetchMaterialized)


async def _single_ref(article: dict[str, object]):
    client = FakeYouTrackClient([article], {str(article["id"]): article})
    adapter = _adapter(_settings(), client=client)
    return await _first(adapter.iter_items())


async def _single_markdown(article: dict[str, object]) -> str:
    client = FakeYouTrackClient([article], {str(article["id"]): article})
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)
    assert isinstance(fetched, SourceFetchMaterialized)
    return fetched.item.payload.decode("utf-8")


def _parent_attachment(**overrides: object) -> dict[str, object]:
    attachment: dict[str, object] = {
        "id": "10-1",
        "name": "attachment.txt",
        "mimeType": "text/plain",
        "size": 1,
        "draft": False,
        "removed": False,
        "comment": None,
        "visibility": {"$type": "UnlimitedVisibility"},
    }
    attachment.update(overrides)
    return attachment


def _issue_attachment_adapter(attachment: dict[str, object]) -> YouTrackAdapter:
    issue = _issue()
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={"3-0": issue},
        issue_attachments={"3-0": [attachment]},
    )
    return _adapter(_settings(), client=client)


async def _issue_attachment_markdown(attachment: dict[str, object]) -> str:
    adapter = _issue_attachment_adapter(attachment)
    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)
    assert isinstance(fetched, SourceFetchMaterialized)
    return fetched.item.payload.decode("utf-8")


async def _issue_markdown(issue: dict[str, object], comments: list[dict[str, object]]) -> str:
    client = FakeYouTrackClient(
        [],
        {},
        issue_summaries=[issue],
        issue_details={str(issue["id"]): issue},
        issue_comments={str(issue["id"]): comments},
    )
    adapter = _adapter(_settings(), client=client)
    ref = await _first(adapter.iter_items())
    fetched = await adapter.fetch(ref)
    assert isinstance(fetched, SourceFetchMaterialized)
    return fetched.item.payload.decode("utf-8")


async def _first(iterator: AsyncIterator[object]):
    async for item in iterator:
        return item
    raise AssertionError("expected one item")
