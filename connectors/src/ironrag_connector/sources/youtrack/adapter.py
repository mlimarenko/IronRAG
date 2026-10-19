from __future__ import annotations

import hashlib
import json
import re
from collections.abc import AsyncIterator
from copy import deepcopy
from dataclasses import dataclass
from datetime import UTC, datetime
from typing import Any, Protocol
from urllib.parse import quote

from ironrag_connector import (
    PolicyOverrides,
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceExclusion,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
    SourceMaterializationEnforcement,
)
from ironrag_connector.observability import get_logger

from .config import YouTrackSettings
from .mapping import (
    CONNECTOR_NAME,
    KIND_ARTICLE,
    KIND_ISSUE,
    KINDS,
    article_external_key,
    build_external_key,
    issue_external_key,
    parse_external_key,
)
from .youtrack import YouTrackClient, YouTrackError, YouTrackNotFoundError

_FILENAME_UNSAFE_RE = re.compile(r"[^a-zA-Z0-9._-]+")
_PROJECTION_KEY = "_ironrag_projection"
log = get_logger(__name__)
# Bump whenever the admitted projection, its enforcement semantics, or the
# rendered Markdown change. The prefix deliberately cannot equal an unversioned
# pre-contract cursor, and the same version is hashed into both source-kind
# snapshots.
_YOUTRACK_PROJECTION_VERSION = 3
_CUSTOM_FIELDS_COLLECTION_ERROR = "YouTrack customFields was not a collection of objects"
_TYPE_KEY = "$type"


class YouTrackApi(Protocol):
    async def validate_credentials(self) -> dict[str, Any]: ...

    def list_articles(self) -> AsyncIterator[dict[str, Any]]: ...

    async def get_article(self, article_id: str) -> dict[str, Any]: ...

    async def get_article_ancestor_descriptor(self, article_id: str) -> dict[str, Any]: ...

    def list_article_comments(self, article_id: str) -> AsyncIterator[dict[str, Any]]: ...

    def list_article_attachments(self, article_id: str) -> AsyncIterator[dict[str, Any]]: ...

    def list_issues(self) -> AsyncIterator[dict[str, Any]]: ...

    async def get_issue(self, issue_id: str) -> dict[str, Any]: ...

    def list_issue_comments(self, issue_id: str) -> AsyncIterator[dict[str, Any]]: ...

    def list_issue_attachments(self, issue_id: str) -> AsyncIterator[dict[str, Any]]: ...

    def list_issue_links(self, issue_id: str) -> AsyncIterator[dict[str, Any]]: ...

    def list_issue_link_targets(
        self, issue_id: str, link_id: str
    ) -> AsyncIterator[dict[str, Any]]: ...

    async def aclose(self) -> None: ...


class YouTrackAdapter(SourceAdapter):
    name = CONNECTOR_NAME
    kinds = KINDS
    # article and issue are independent top-level collections (unlike
    # BookStack's page->attachment/image parent-child shape): neither has a
    # parent among the other, both must be enumerated and reaped on their
    # own, so both are primary kinds.
    primary_kinds = (KIND_ARTICLE, KIND_ISSUE)

    def __init__(
        self,
        settings: YouTrackSettings,
        policies: PolicyOverrides,
        client: YouTrackApi | None = None,
    ) -> None:
        self._settings = settings
        self._policies = policies
        self._client: YouTrackApi = client or YouTrackClient(settings)
        self._base_url = settings.youtrack_base_url.rstrip("/")
        self._included_projects = set(settings.included_projects())
        self._excluded_projects = set(settings.excluded_projects())
        self._indexed_custom_fields = frozenset(settings.indexed_custom_fields())
        self._visibility_census: dict[str, dict[str, Any]] = {}

    async def close(self) -> None:
        await self._client.aclose()

    def external_key(self, kind: str, item_id: str) -> str:
        return build_external_key(kind, item_id)

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return parse_external_key(external_key)

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind not in self.kinds:
            raise ValueError(f"unsupported YouTrack item kind: {kind}")
        # Both top-level kinds are complete censuses. A missing parent can no
        # longer contribute a safe composite document, regardless of the
        # operator's retention preference.
        return SourceDeleteEnforcement.REQUIRED

    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        initial_identity = await self._client.validate_credentials()
        initial_identity_id = _required_string(initial_identity, "id")

        article_snapshot = await self._stable_article_snapshot()
        issue_snapshot = await self._stable_issue_snapshot()

        article_refs: list[SourceItemRef] = []
        article_census = _VisibilityCensus(KIND_ARTICLE)
        for article_id, article in article_snapshot.items():
            if not self._include_article(article):
                continue
            article_census.in_scope += 1
            chain = _article_access_chain(article_id, article_snapshot)
            if _access_chain_is_unlimited(chain):
                related = await self._stable_related_snapshot(KIND_ARTICLE, article_id)
            else:
                article_census.restricted_parents += 1
                related = _empty_related_snapshot()
            article_census.observe_related(related)
            projected = _with_projection(article, chain=chain, related=related)
            article_refs.append(self._article_ref(article_id, projected))

        issue_refs: list[SourceItemRef] = []
        issue_census = _VisibilityCensus(KIND_ISSUE)
        for issue_id, issue in issue_snapshot.items():
            if not self._include_issue(issue):
                continue
            issue_census.in_scope += 1
            access = ({"item_id": issue_id, **_visibility_descriptor(issue.get("visibility"))},)
            if _access_chain_is_unlimited(access):
                related = await self._stable_related_snapshot(
                    KIND_ISSUE,
                    issue_id,
                    expected_comment_count=self._issue_expected_comment_count(issue),
                )
            else:
                issue_census.restricted_parents += 1
                related = _empty_related_snapshot()
            issue_census.observe_related(related)
            projected = _with_projection(issue, chain=access, related=related)
            issue_refs.append(self._issue_ref(issue_id, projected))

        self._visibility_census = {
            KIND_ARTICLE: article_census.as_log_fields(),
            KIND_ISSUE: issue_census.as_log_fields(),
        }
        log.info("youtrack.visibility_census", **self._visibility_census[KIND_ARTICLE])
        log.info("youtrack.visibility_census", **self._visibility_census[KIND_ISSUE])

        final_identity = await self._client.validate_credentials()
        if _required_string(final_identity, "id") != initial_identity_id:
            raise YouTrackError(
                "YouTrack authenticated identity changed during snapshot validation; "
                "retry next sweep"
            )

        for ref in article_refs:
            yield ref
        for ref in issue_refs:
            yield ref

    async def _stable_article_snapshot(self) -> dict[str, dict[str, Any]]:
        # Articles change rarely enough that requiring the *exact* content
        # token to hold across every validation pass is the right bar: any
        # drift means a concurrent edit raced the sweep, and it is cheap to
        # just retry next time. See `_stable_issue_snapshot` for why issues
        # cannot use this same bar.
        previous_tokens: dict[str, str] | None = None
        snapshot: dict[str, dict[str, Any]] = {}
        for _ in range(self._settings.youtrack_snapshot_validation_passes):
            current: dict[str, dict[str, Any]] = {}
            async for article in self._client.list_articles():
                article_id = _required_string(article, "id")
                current[article_id] = article
            current_tokens = {
                article_id: _article_listing_token(article)
                for article_id, article in current.items()
            }
            if previous_tokens is not None and current_tokens != previous_tokens:
                raise YouTrackError(
                    "YouTrack article snapshot changed between validation passes; retry next sweep"
                )
            previous_tokens = current_tokens
            snapshot = current
        return snapshot

    async def _stable_issue_snapshot(self) -> dict[str, dict[str, Any]]:
        # Issues churn constantly (comments, state, priority) even while a
        # sweep is enumerating them, and `list_issues` pins `created asc` so
        # a live edit cannot reorder pages -- but it can still change an
        # issue's own token between two validation passes with zero actual
        # data loss. Requiring token equality here would make every sweep
        # against a live tracker fail. Only the *id set* needs to hold
        # across passes; `fetch()` re-validates each item's token/routing
        # facts against the ref before materializing it, so a between-pass
        # edit is still caught, just not by aborting the whole sweep.
        previous_ids: set[str] | None = None
        snapshot: dict[str, dict[str, Any]] = {}
        for _ in range(self._settings.youtrack_snapshot_validation_passes):
            current: dict[str, dict[str, Any]] = {}
            async for issue in self._client.list_issues():
                issue_id = _required_string(issue, "id")
                current[issue_id] = self._with_allowed_custom_fields(issue)
            current_ids = set(current)
            if previous_ids is not None and current_ids != previous_ids:
                raise YouTrackError(
                    "YouTrack issue snapshot changed between validation passes; retry next sweep"
                )
            previous_ids = current_ids
            snapshot = current
        return snapshot

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        if ref.kind == KIND_ARTICLE:
            return await self._fetch_article(ref)
        if ref.kind == KIND_ISSUE:
            return await self._fetch_issue(ref)
        raise YouTrackError(f"unsupported YouTrack item kind: {ref.kind}")

    async def _fetch_article(self, ref: SourceItemRef) -> SourceFetchResult:
        try:
            article = await self._client.get_article(ref.item_id)
        except YouTrackNotFoundError:
            # Deliberate: a 404 cannot prove whether the cause was deletion or
            # lost readability, so exclusion stays NONE; calling a genuine
            # deletion a visibility exclusion would be a false positive.
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="article can no longer be read after enumeration",
                cacheable=False,
            )

        article_id = _optional_string(article.get("id"))
        if article_id != ref.item_id:
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="article identity can no longer be proven after enumeration",
                cacheable=False,
            )
        if not self._include_article(article):
            return SourceFetchDeferred("article scope changed during materialization")
        try:
            chain = await self._live_article_access_chain(article)
        except YouTrackNotFoundError:
            # Deliberate: a 404 cannot prove whether the cause was deletion or
            # lost readability, so exclusion stays NONE; calling a genuine
            # deletion a visibility exclusion would be a false positive.
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="article ancestry can no longer be proven readable",
                cacheable=False,
            )
        related = (
            await self._stable_related_snapshot(KIND_ARTICLE, article_id)
            if _access_chain_is_unlimited(chain)
            else _empty_related_snapshot()
        )
        projected = _with_projection(article, chain=chain, related=related)
        current_ref = self._article_ref(article_id, projected)
        current_matches_ref = _ref_state_matches(current_ref, ref)
        visibility_cannot_be_represented = _projection_visibility_cannot_be_represented(projected)
        if _projection_requires_security_delete(projected):
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="article visibility cannot be represented safely",
                cacheable=current_matches_ref,
                exclusion=(
                    SourceExclusion.LIMITED_VISIBILITY
                    if visibility_cannot_be_represented
                    else SourceExclusion.NONE
                ),
            )
        if _projection_security_tightened(ref.raw, projected):
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="article child readability tightened during materialization",
                cacheable=False,
                exclusion=(
                    SourceExclusion.LIMITED_VISIBILITY
                    if _projection_security_tightened_by_visibility(ref.raw, projected)
                    else SourceExclusion.NONE
                ),
            )
        if not current_matches_ref:
            return SourceFetchDeferred("article changed during materialization")
        title = _article_title(article)
        readable_id = _optional_string(article.get("idReadable")) or article_id
        payload = _render_article_markdown(title, projected)
        return SourceFetchMaterialized(
            SourceItem(
                ref=current_ref,
                payload=payload.encode("utf-8"),
                mime_type="text/markdown",
                file_name=f"{_safe_filename(readable_id, default='article')}.md",
                title=title,
                document_hint=(f"{self._base_url}/articles/{quote(readable_id, safe='')}"),
            ),
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )

    async def _fetch_issue(self, ref: SourceItemRef) -> SourceFetchResult:
        try:
            issue = await self._client.get_issue(ref.item_id)
        except YouTrackNotFoundError:
            # Deliberate: a 404 cannot prove whether the cause was deletion or
            # lost readability, so exclusion stays NONE; calling a genuine
            # deletion a visibility exclusion would be a false positive.
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="issue can no longer be read after enumeration",
                cacheable=False,
            )

        issue_id = _required_string(issue, "id")
        if issue_id != ref.item_id:
            raise YouTrackError(f"YouTrack returned issue {issue_id} while fetching {ref.item_id}")
        issue = self._with_allowed_custom_fields(issue)
        if not self._include_issue(issue):
            return SourceFetchDeferred("issue scope changed during materialization")
        access = ({"item_id": issue_id, **_visibility_descriptor(issue.get("visibility"))},)
        related = (
            await self._stable_related_snapshot(
                KIND_ISSUE,
                issue_id,
                expected_comment_count=self._issue_expected_comment_count(issue),
            )
            if _access_chain_is_unlimited(access)
            else _empty_related_snapshot()
        )
        projected = _with_projection(issue, chain=access, related=related)
        current_ref = self._issue_ref(issue_id, projected)
        current_matches_ref = _ref_state_matches(current_ref, ref)
        visibility_cannot_be_represented = _projection_visibility_cannot_be_represented(projected)
        if _projection_requires_security_delete(projected):
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason=(
                    "issue visibility or related-content completeness cannot be represented safely"
                ),
                cacheable=current_matches_ref,
                exclusion=(
                    SourceExclusion.LIMITED_VISIBILITY
                    if visibility_cannot_be_represented
                    else SourceExclusion.NONE
                ),
            )
        if _projection_security_tightened(ref.raw, projected):
            return SourceFetchDelete(
                enforcement=SourceDeleteEnforcement.REQUIRED,
                reason="issue child readability tightened during materialization",
                cacheable=False,
                exclusion=(
                    SourceExclusion.LIMITED_VISIBILITY
                    if _projection_security_tightened_by_visibility(ref.raw, projected)
                    else SourceExclusion.NONE
                ),
            )
        if not current_matches_ref:
            return SourceFetchDeferred("issue changed during materialization")
        title = _issue_title(issue)
        readable_id = _optional_string(issue.get("idReadable")) or issue_id
        payload = _render_issue_markdown(title, readable_id, projected)
        return SourceFetchMaterialized(
            SourceItem(
                ref=current_ref,
                payload=payload.encode("utf-8"),
                mime_type="text/markdown",
                file_name=f"{_safe_filename(readable_id, default='issue')}.md",
                title=title,
                document_hint=(f"{self._base_url}/issue/{quote(readable_id, safe='')}"),
            ),
            enforcement=SourceMaterializationEnforcement.REQUIRED,
        )

    async def _live_article_access_chain(
        self, article: dict[str, Any]
    ) -> tuple[dict[str, Any], ...]:
        chain: list[dict[str, Any]] = []
        seen: set[str] = set()
        current = article
        while True:
            current_id = _required_string(current, "id")
            if current_id in seen:
                chain.append({"item_id": current_id, "kind": "unknown"})
                return tuple(chain)
            seen.add(current_id)
            descriptor = _visibility_descriptor(current.get("visibility"))
            chain.append({"item_id": current_id, **descriptor})
            try:
                parent_id = _required_parent_article_id(current)
            except YouTrackError:
                # A structurally incomplete successful response is an
                # authoritative loss of ancestry proof, not a transient
                # transport failure. Preserve the expected identity while
                # making the access chain fail closed.
                chain[-1] = {"item_id": current_id, "kind": "unknown"}
                return tuple(chain)
            if parent_id is None:
                return tuple(chain)
            candidate = await self._client.get_article_ancestor_descriptor(parent_id)
            returned_id = _optional_string(candidate.get("id"))
            if returned_id != parent_id:
                chain.append({"item_id": parent_id, "kind": "unknown"})
                return tuple(chain)
            current = candidate

    async def _stable_related_snapshot(
        self,
        kind: str,
        item_id: str,
        *,
        expected_comment_count: int | None = None,
    ) -> dict[str, Any]:
        previous: dict[str, Any] | None = None
        for _ in range(self._settings.youtrack_snapshot_validation_passes):
            current = await self._read_related_snapshot(
                kind,
                item_id,
                expected_comment_count=expected_comment_count,
            )
            if previous is not None and current == previous:
                return current
            previous = current
        raise YouTrackError(
            f"YouTrack {kind} {item_id} related collections did not stabilize; retry next sweep"
        )

    async def _read_related_snapshot(
        self,
        kind: str,
        item_id: str,
        *,
        expected_comment_count: int | None,
    ) -> dict[str, Any]:
        if kind == KIND_ARTICLE:
            return await self._read_article_related_snapshot(item_id, expected_comment_count)
        if kind != KIND_ISSUE:
            raise YouTrackError(f"unsupported YouTrack item kind: {kind}")
        if (expected_comment_count is None) == self._comments_indexed(KIND_ISSUE):
            raise YouTrackError(
                "issue related snapshot commentsCount does not match the comment sync policy"
            )
        return await self._read_issue_related_snapshot(item_id, expected_comment_count)

    def _comments_indexed(self, kind: str) -> bool:
        """``policies.<kind>.include_comments`` from routing.yaml, read live.

        The table is the framework's own object and is replaced in place on a
        routing hot reload, so the next enumeration and fetch see the new
        value without a restart.
        """
        return self._policies.for_kind(kind).include_comments

    def _issue_expected_comment_count(self, issue: dict[str, Any]) -> int | None:
        """The completeness gate applies only while issue comments are indexed."""
        if not self._comments_indexed(KIND_ISSUE):
            return None
        return _required_issue_comments_count(issue)

    async def _read_comments(self, kind: str, item_id: str) -> list[dict[str, Any]]:
        if not self._comments_indexed(kind):
            return []
        listing = (
            self._client.list_article_comments(item_id)
            if kind == KIND_ARTICLE
            else self._client.list_issue_comments(item_id)
        )
        return [comment async for comment in listing]

    async def _read_article_related_snapshot(
        self,
        item_id: str,
        expected_comment_count: int | None,
    ) -> dict[str, Any]:
        if expected_comment_count is not None:
            raise YouTrackError("article related snapshot received an issue comment count")
        comments = await self._read_comments(KIND_ARTICLE, item_id)
        attachments = [
            attachment async for attachment in self._client.list_article_attachments(item_id)
        ]
        return _normalize_related_snapshot(
            comments=comments,
            attachments=attachments,
            links=[],
            link_targets={},
            issue_comments=False,
            comments_indexed=self._comments_indexed(KIND_ARTICLE),
            expected_comment_count=None,
        )

    async def _read_issue_related_snapshot(
        self,
        item_id: str,
        expected_comment_count: int | None,
    ) -> dict[str, Any]:
        comments = await self._read_comments(KIND_ISSUE, item_id)
        attachments = [
            attachment async for attachment in self._client.list_issue_attachments(item_id)
        ]
        links = [link async for link in self._client.list_issue_links(item_id)]
        link_targets: dict[str, list[dict[str, Any]] | None] = {}
        for link in links:
            link_id = _required_string(link, "id")
            link_targets[link_id] = await self._read_issue_link_targets(item_id, link_id, link)
        return _normalize_related_snapshot(
            comments=comments,
            attachments=attachments,
            links=links,
            link_targets=link_targets,
            issue_comments=True,
            comments_indexed=self._comments_indexed(KIND_ISSUE),
            expected_comment_count=expected_comment_count,
        )

    async def _read_issue_link_targets(
        self,
        issue_id: str,
        link_id: str,
        link: dict[str, Any],
    ) -> list[dict[str, Any]] | None:
        embedded_target_ids = _embedded_link_target_ids(link)
        if embedded_target_ids is None:
            # The requested ``issues(id)`` projection is the only proof that
            # this link's target collection is genuinely empty. A malformed
            # value can be an ACL/redaction race, so fail the projection closed.
            return None
        if not embedded_target_ids:
            return []
        targets: list[dict[str, Any]] = []
        async for target in self._client.list_issue_link_targets(issue_id, link_id):
            candidate = deepcopy(target)
            try:
                candidate["_ironrag_eligible"] = self._include_issue(candidate)
            except YouTrackError:
                candidate["_ironrag_eligible"] = None
            targets.append(candidate)
        returned_target_ids = {_required_string(candidate, "id") for candidate in targets}
        return targets if embedded_target_ids.issubset(returned_target_ids) else None

    def _article_ref(self, article_id: str, article: dict[str, Any]) -> SourceItemRef:
        return SourceItemRef(
            item_id=article_id,
            kind=KIND_ARTICLE,
            external_key=article_external_key(article_id),
            change_token=_article_change_token(article),
            routing_facts=_article_routing_facts(article),
            raw=_projection_only_raw(article),
            materialization_enforcement=SourceMaterializationEnforcement.REQUIRED,
        )

    def _issue_ref(self, issue_id: str, issue: dict[str, Any]) -> SourceItemRef:
        return SourceItemRef(
            item_id=issue_id,
            kind=KIND_ISSUE,
            external_key=issue_external_key(issue_id),
            change_token=_issue_change_token(
                issue, comments_count=self._issue_expected_comment_count(issue)
            ),
            routing_facts=_issue_routing_facts(issue),
            raw=_projection_only_raw(issue),
            materialization_enforcement=SourceMaterializationEnforcement.REQUIRED,
        )

    def _with_allowed_custom_fields(self, issue: dict[str, Any]) -> dict[str, Any]:
        """Copy an issue while admitting only explicitly configured fields.

        Request projection is an optimization, not a trust boundary: a server
        or test double can still return extra fields. Validate the collection
        shape, then discard unconfigured entries before the object can affect
        a change token, routing facts, or rendered markdown. ``ref.raw`` is
        narrower still: it retains only the normalized access/child projection.
        """
        fields = _allowed_custom_fields(issue.get("customFields"), self._indexed_custom_fields)
        # Exclude the vendor collection before copying the rest of the
        # response. Otherwise ``deepcopy(issue)`` would transiently copy every
        # unconfigured value before the filtered collection replaced it.
        projected = {key: deepcopy(value) for key, value in issue.items() if key != "customFields"}
        projected["customFields"] = fields
        return projected

    def _include_article(self, article: dict[str, Any]) -> bool:
        if not self._passes_project_filter(article, item_label="article"):
            return False
        return self._passes_require_tag(article)

    def _include_issue(self, issue: dict[str, Any]) -> bool:
        if not self._passes_project_filter(issue, item_label="issue"):
            return False
        return self._passes_require_tag(issue)

    def _passes_project_filter(self, item: dict[str, Any], *, item_label: str) -> bool:
        project = item.get("project")
        if not isinstance(project, dict):
            raise YouTrackError(f"YouTrack {item_label} did not contain project metadata")
        selectors = {
            value
            for value in (
                _optional_string(project.get("id")),
                _optional_string(project.get("shortName")),
            )
            if value is not None
        }
        if not selectors:
            raise YouTrackError(f"YouTrack {item_label} project did not contain an id")
        if self._included_projects and not (selectors & self._included_projects):
            return False
        if selectors & self._excluded_projects:
            return False
        archived = project.get("archived")
        if not isinstance(archived, bool):
            raise YouTrackError(
                f"YouTrack {item_label} project did not contain a boolean archived flag"
            )
        return not (archived and not self._settings.youtrack_include_archived_projects)

    def _passes_require_tag(self, item: dict[str, Any]) -> bool:
        required = self._settings.youtrack_require_tag
        if not required:
            return True
        names = {tag["name"] for tag in _tag_descriptors(item.get("tags"))}
        return required in names


def _allowed_custom_fields(
    raw_fields: Any,
    indexed_names: frozenset[str],
) -> list[dict[str, Any]]:
    if raw_fields is None:
        if indexed_names:
            raise YouTrackError(
                "YouTrack issue did not contain the requested customFields collection"
            )
        return []
    if not isinstance(raw_fields, list):
        raise YouTrackError(_CUSTOM_FIELDS_COLLECTION_ERROR)
    fields: list[dict[str, Any]] = []
    for raw_field in raw_fields:
        normalized = _allowed_custom_field(raw_field, indexed_names)
        if normalized is not None:
            fields.append(normalized)
    return fields


def _allowed_custom_field(
    raw_field: Any,
    indexed_names: frozenset[str],
) -> dict[str, Any] | None:
    if not isinstance(raw_field, dict):
        raise YouTrackError(_CUSTOM_FIELDS_COLLECTION_ERROR)
    name = _optional_string(raw_field.get("name"))
    if name is None:
        raise YouTrackError("YouTrack custom field did not contain a name")
    if name not in indexed_names:
        # Do not inspect or copy the value of an unconfigured field. It is
        # outside the connector's source boundary.
        return None
    field_type = raw_field.get(_TYPE_KEY)
    if field_type is not None and not isinstance(field_type, str):
        raise YouTrackError(f"YouTrack {name} custom field type was not a string")
    normalized: dict[str, Any] = {
        "name": name,
        "value": _normalize_custom_field_value(raw_field.get("value"), field_name=name),
    }
    if field_type is not None:
        normalized[_TYPE_KEY] = field_type
    return normalized


def _with_projection(
    item: dict[str, Any],
    *,
    chain: tuple[dict[str, Any], ...],
    related: dict[str, Any],
) -> dict[str, Any]:
    projected = deepcopy(item)
    projected[_PROJECTION_KEY] = {
        "access_chain": list(chain),
        "related": related,
    }
    return projected


def _projection(item: dict[str, Any]) -> dict[str, Any]:
    projection = item.get(_PROJECTION_KEY)
    if not isinstance(projection, dict):
        raise YouTrackError("YouTrack item did not contain normalized projection")
    if not isinstance(projection.get("access_chain"), list) or not isinstance(
        projection.get("related"), dict
    ):
        raise YouTrackError("YouTrack item contained malformed normalized projection")
    return projection


def _projection_only_raw(item: dict[str, Any]) -> dict[str, Any]:
    """Retain only the normalized child/access snapshot needed at fetch."""
    return {_PROJECTION_KEY: deepcopy(_projection(item))}


def _empty_related_snapshot() -> dict[str, Any]:
    return {
        "comments": [],
        "attachments": [],
        "links": [],
        "comments_complete": True,
        "unknown_visibility": False,
    }


def _visibility_descriptor(raw: Any) -> dict[str, Any]:
    if not isinstance(raw, dict):
        return {"kind": "unknown"}
    visibility_type = raw.get(_TYPE_KEY)
    if visibility_type == "UnlimitedVisibility":
        return {"kind": "unlimited"}
    if visibility_type != "LimitedVisibility":
        return {"kind": "unknown"}
    users = sorted(
        {
            user_id
            for user in _dict_items(raw.get("permittedUsers"))
            if (user_id := _optional_string(user.get("id"))) is not None
        }
    )
    groups = sorted(
        {
            group_id
            for group in _dict_items(raw.get("permittedGroups"))
            if (group_id := _optional_string(group.get("id"))) is not None
        }
    )
    return {"kind": "limited", "user_ids": users, "group_ids": groups}


def _article_access_chain(
    article_id: str, snapshot: dict[str, dict[str, Any]]
) -> tuple[dict[str, Any], ...]:
    chain: list[dict[str, Any]] = []
    seen: set[str] = set()
    current_id = article_id
    while True:
        if current_id in seen:
            chain.append({"item_id": current_id, "kind": "unknown"})
            return tuple(chain)
        seen.add(current_id)
        current = snapshot.get(current_id)
        if current is None:
            chain.append({"item_id": current_id, "kind": "unknown"})
            return tuple(chain)
        chain.append({"item_id": current_id, **_visibility_descriptor(current.get("visibility"))})
        try:
            parent_id = _required_parent_article_id(current)
        except YouTrackError:
            chain[-1] = {"item_id": current_id, "kind": "unknown"}
            return tuple(chain)
        if parent_id is None:
            return tuple(chain)
        current_id = parent_id


def _access_chain_is_unlimited(chain: tuple[dict[str, Any], ...]) -> bool:
    return bool(chain) and all(item.get("kind") == "unlimited" for item in chain)


@dataclass
class _VisibilityCensus:
    """Counts one kind's sweep admitted, to correlate source with graph.

    Every field is a count or the kind name; no YouTrack text, id, user or
    group crosses this boundary, so the structure is safe to log at INFO. A
    different access chain yields no children at all (the related snapshot is
    empty), so ``restricted_parents`` must be read together with the child
    counts: children are only ever counted for provably unlimited parents.
    """

    kind: str
    in_scope: int = 0
    restricted_parents: int = 0
    comments: int = 0
    restricted_comments: int = 0
    attachments: int = 0
    restricted_attachments: int = 0
    link_targets: int = 0
    restricted_link_targets: int = 0
    unknown_visibility_items: int = 0

    def observe_related(self, related: dict[str, Any]) -> None:
        for comment in related["comments"]:
            self.comments += 1
            if comment["visibility"]["kind"] != "unlimited":
                self.restricted_comments += 1
            for attachment in comment["attachments"]:
                self._observe_attachment(attachment)
        for attachment in related["attachments"]:
            self._observe_attachment(attachment)
        for link in related["links"]:
            for target in link["targets"]:
                self.link_targets += 1
                if target["visibility"]["kind"] != "unlimited":
                    self.restricted_link_targets += 1
        if related.get("unknown_visibility") is True:
            self.unknown_visibility_items += 1

    def _observe_attachment(self, attachment: dict[str, Any]) -> None:
        self.attachments += 1
        if attachment["visibility"]["kind"] != "unlimited":
            self.restricted_attachments += 1

    def as_log_fields(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "in_scope": self.in_scope,
            "restricted_parents": self.restricted_parents,
            "comments": self.comments,
            "restricted_comments": self.restricted_comments,
            "attachments": self.attachments,
            "restricted_attachments": self.restricted_attachments,
            "link_targets": self.link_targets,
            "restricted_link_targets": self.restricted_link_targets,
            "unknown_visibility_items": self.unknown_visibility_items,
        }


def _required_parent_article_id(article: dict[str, Any]) -> str | None:
    """Parse the minimal parent descriptor without treating absence as root."""
    if "parentArticle" not in article:
        raise YouTrackError("YouTrack article descriptor did not contain parentArticle")
    parent = article["parentArticle"]
    if parent is None:
        return None
    if not isinstance(parent, dict):
        raise YouTrackError("YouTrack article parentArticle was not an object or null")
    parent_id = _optional_string(parent.get("id"))
    if parent_id is None:
        raise YouTrackError("YouTrack article parentArticle did not contain an id")
    return parent_id


def _projection_visibility_cannot_be_represented(item: dict[str, Any]) -> bool:
    """The projection has no provably safe public visibility for its access chain or children."""
    projection = _projection(item)
    return (
        not all(
            isinstance(entry, dict) and entry.get("kind") == "unlimited"
            for entry in projection["access_chain"]
        )
        or projection["related"].get("unknown_visibility") is True
    )


def _projection_requires_security_delete(item: dict[str, Any]) -> bool:
    projection = _projection(item)
    return (
        _projection_visibility_cannot_be_represented(item)
        or projection["related"].get("comments_complete") is not True
    )


def _ref_state_matches(current: SourceItemRef, captured: SourceItemRef) -> bool:
    return (
        current.change_token == captured.change_token
        and current.routing_facts == captured.routing_facts
    )


def _previously_indexable_children(
    related: dict[str, Any],
) -> dict[tuple[str, object], dict[str, Any]]:
    """Children of ``related`` that the previous projection could index, by kind and identity."""

    children: dict[tuple[str, object], dict[str, Any]] = {}
    for comment in related["comments"]:
        if _comment_is_indexable(comment):
            children[("comment", comment["id"])] = comment
    for attachment in _all_related_attachments(related):
        if _attachment_is_indexable(attachment):
            children[("attachment", attachment["id"])] = attachment
    for link in related["links"]:
        for target in link["targets"]:
            if _link_target_is_indexable(target):
                children[("link_target", (link["id"], target["id"]))] = target
    return children


def _currently_readable_children(
    related: dict[str, Any],
) -> dict[tuple[str, object], dict[str, Any]]:
    """Every child the current collection endpoint still returns, by kind and identity."""

    children: dict[tuple[str, object], dict[str, Any]] = {}
    for comment in related["comments"]:
        children[("comment", comment["id"])] = comment
    for attachment in _all_related_attachments(related):
        children[("attachment", attachment["id"])] = attachment
    for link in related["links"]:
        for target in link["targets"]:
            children[("link_target", (link["id"], target["id"]))] = target
    return children


def _is_indexable(kind: str, child: dict[str, Any]) -> bool:
    if kind == "comment":
        return _comment_is_indexable(child)
    if kind == "attachment":
        return _attachment_is_indexable(child)
    return _link_target_is_indexable(child)


def _projection_security_tightened(previous: dict[str, Any], current: dict[str, Any]) -> bool:
    """Detect a child that may have disappeared from the readable projection.

    YouTrack collection endpoints expose what the connector account can read.
    Consequently, a previously indexable child that is now absent is
    indistinguishable from one whose visibility was tightened. Preserve no
    stale bytes in either case: delete the composite parent and let a later
    stable sweep recreate its still-readable projection.
    """

    previous_children = _previously_indexable_children(_projection(previous)["related"])
    current_children = _currently_readable_children(_projection(current)["related"])
    return any(
        child_key not in current_children
        or not _is_indexable(child_key[0], current_children[child_key])
        for child_key in previous_children
    )


def _projection_security_tightened_by_visibility(
    previous: dict[str, Any], current: dict[str, Any]
) -> bool:
    """Whether a still-present indexable child lost its ``unlimited`` visibility.

    ``_projection_security_tightened`` also fires when a child is simply gone or
    newly deleted, removed or ineligible, and a collection endpoint cannot say
    which. Absence is deliberately not attributed to visibility here: only a
    child that was indexable in ``previous`` and is still present in ``current``
    with a visibility kind other than ``unlimited`` is a visibility transition.
    """

    previous_children = _previously_indexable_children(_projection(previous)["related"])
    current_children = _currently_readable_children(_projection(current)["related"])
    return any(
        child_key in current_children
        and current_children[child_key]["visibility"]["kind"] != "unlimited"
        for child_key in previous_children
    )


def _all_related_attachments(related: dict[str, Any]) -> list[dict[str, Any]]:
    return [
        *related["attachments"],
        *(attachment for comment in related["comments"] for attachment in comment["attachments"]),
    ]


def _comment_is_indexable(comment: dict[str, Any]) -> bool:
    return comment["visibility"]["kind"] == "unlimited" and comment["deleted"] is not True


def _link_target_is_indexable(target: dict[str, Any]) -> bool:
    return target["visibility"]["kind"] == "unlimited" and target.get("eligible") is True


def _article_listing_token(article: dict[str, Any]) -> str:
    project = _dict_or_empty(article.get("project"))
    parent = _dict_or_empty(article.get("parentArticle"))
    reporter = _dict_or_empty(article.get("reporter"))
    updated_by = _dict_or_empty(article.get("updatedBy"))
    snapshot = {
        "id": article.get("id"),
        "id_readable": article.get("idReadable"),
        "created": article.get("created"),
        "updated": article.get("updated"),
        "summary": article.get("summary"),
        "project": {
            "id": project.get("id"),
            "short_name": project.get("shortName"),
            "name": project.get("name"),
            "archived": project.get("archived"),
        },
        "parent": {"id": parent.get("id"), "id_readable": parent.get("idReadable")},
        "reporter": {"id": reporter.get("id"), "login": reporter.get("login")},
        "updated_by": {"id": updated_by.get("id"), "login": updated_by.get("login")},
        "tags": _tag_descriptors(article.get("tags")),
        "visibility": _visibility_descriptor(article.get("visibility")),
    }
    return _hash_json(snapshot)


def _article_change_token(article: dict[str, Any]) -> str:
    project = _dict_or_empty(article.get("project"))
    parent = _dict_or_empty(article.get("parentArticle"))
    reporter = _dict_or_empty(article.get("reporter"))
    updated_by = _dict_or_empty(article.get("updatedBy"))
    projection = _projection(article)
    snapshot = {
        "projection_version": _YOUTRACK_PROJECTION_VERSION,
        "created": article.get("created"),
        "updated": article.get("updated"),
        "summary": article.get("summary"),
        "project": {
            "id": project.get("id"),
            "shortName": project.get("shortName"),
            "name": project.get("name"),
        },
        "parent": {
            "id": parent.get("id"),
            "idReadable": parent.get("idReadable"),
        },
        "reporter": {"id": reporter.get("id"), "login": reporter.get("login")},
        "updated_by": {
            "id": updated_by.get("id"),
            "login": updated_by.get("login"),
        },
        "tags": _tag_descriptors(article.get("tags")),
        "access_chain": projection["access_chain"],
        "related": projection["related"],
    }
    return f"youtrack-composite-v{_YOUTRACK_PROJECTION_VERSION}:{_hash_json(snapshot)}"


def _article_routing_facts(article: dict[str, Any]) -> dict[str, Any]:
    project = _dict_or_empty(article.get("project"))
    parent = _dict_or_empty(article.get("parentArticle"))
    reporter = _dict_or_empty(article.get("reporter"))
    tags = _sorted_tag_names(article.get("tags"))
    return {
        "kind": KIND_ARTICLE,
        "article_id": _optional_string(article.get("id")),
        "article_id_readable": _optional_string(article.get("idReadable")),
        "project_id": _optional_string(project.get("id")),
        "project": _optional_string(project.get("shortName")),
        "project_short_name": _optional_string(project.get("shortName")),
        "project_name": _optional_string(project.get("name")),
        "parent_article_id": _optional_string(parent.get("id")),
        "parent_article_id_readable": _optional_string(parent.get("idReadable")),
        "tag": tags,
        "reporter_login": _optional_string(reporter.get("login")),
    }


def _issue_change_token(issue: dict[str, Any], *, comments_count: int | None) -> str:
    """Hash the projection; ``comments_count`` is ``None`` when comments are not indexed."""
    project = _dict_or_empty(issue.get("project"))
    reporter = _dict_or_empty(issue.get("reporter"))
    updater = _dict_or_empty(issue.get("updater"))
    projection = _projection(issue)
    snapshot = {
        "projection_version": _YOUTRACK_PROJECTION_VERSION,
        "created": issue.get("created"),
        "updated": issue.get("updated"),
        "resolved": issue.get("resolved"),
        "comments_count": comments_count,
        "summary": issue.get("summary"),
        "project": {
            "id": project.get("id"),
            "shortName": project.get("shortName"),
            "name": project.get("name"),
        },
        "reporter": {"id": reporter.get("id"), "login": reporter.get("login")},
        "updater": {"id": updater.get("id"), "login": updater.get("login")},
        "tags": _tag_descriptors(issue.get("tags")),
        "custom_fields": _normalized_custom_fields(issue.get("customFields")),
        "access_chain": projection["access_chain"],
        "related": projection["related"],
    }
    return f"youtrack-composite-v{_YOUTRACK_PROJECTION_VERSION}:{_hash_json(snapshot)}"


def _issue_routing_facts(issue: dict[str, Any]) -> dict[str, Any]:
    project = _dict_or_empty(issue.get("project"))
    reporter = _dict_or_empty(issue.get("reporter"))
    fields = _normalized_custom_fields(issue.get("customFields"))
    tags = _sorted_tag_names(issue.get("tags"))
    return {
        "kind": KIND_ISSUE,
        "issue_id": _optional_string(issue.get("id")),
        "issue_id_readable": _optional_string(issue.get("idReadable")),
        "project_id": _optional_string(project.get("id")),
        "project": _optional_string(project.get("shortName")),
        "project_short_name": _optional_string(project.get("shortName")),
        "project_name": _optional_string(project.get("name")),
        "tag": tags,
        "reporter_login": _optional_string(reporter.get("login")),
        **{
            f"custom_field:{name}": value
            for name, field in fields.items()
            if (value := _routing_custom_field_value(field)) is not None
        },
    }


def _sorted_tag_names(raw: Any) -> list[str]:
    return sorted({tag["name"] for tag in _tag_descriptors(raw)})


def _normalized_custom_fields(raw: Any) -> dict[str, Any]:
    """Normalize only the explicitly requested YouTrack custom fields."""

    if raw is None:
        return {}
    if not isinstance(raw, list) or any(not isinstance(entry, dict) for entry in raw):
        raise YouTrackError(_CUSTOM_FIELDS_COLLECTION_ERROR)
    fields: dict[str, Any] = {}
    for entry in raw:
        name = _required_string(entry, "name")
        if name in fields:
            raise YouTrackError(f"YouTrack customFields contained duplicate field {name}")
        field_type = entry.get(_TYPE_KEY)
        if field_type is not None and not isinstance(field_type, str):
            raise YouTrackError(f"YouTrack {name} custom field type was not a string")
        fields[name] = {
            "type": field_type,
            "value": _normalize_custom_field_value(entry.get("value"), field_name=name),
        }
    return {name: fields[name] for name in sorted(fields)}


def _normalize_custom_field_value(value: Any, *, field_name: str) -> Any:
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, dict):
        result: dict[str, str] = {}
        for key in ("id", "name", "login"):
            candidate = value.get(key)
            if candidate is not None:
                if not isinstance(candidate, str):
                    raise YouTrackError(
                        f"YouTrack {field_name} custom field {key} was not a string"
                    )
                result[key] = candidate
        if not result:
            raise YouTrackError(
                f"YouTrack {field_name} custom field object had no supported identity"
            )
        return result
    if isinstance(value, list):
        normalized = [_normalize_custom_field_value(item, field_name=field_name) for item in value]
        return sorted(
            normalized,
            key=lambda item: json.dumps(
                item, ensure_ascii=False, sort_keys=True, separators=(",", ":")
            ),
        )
    raise YouTrackError(
        f"YouTrack {field_name} custom field had an unexpected shape: {type(value).__name__}"
    )


def _display_custom_field_value(field: Any) -> str | None:
    if not isinstance(field, dict) or "value" not in field:
        return None
    return _display_normalized_value(field["value"])


def _routing_custom_field_value(field: Any) -> str | list[str] | None:
    """Keep scalar vs multi-value shape for generic routing membership."""
    if not isinstance(field, dict) or "value" not in field:
        return None
    value = field["value"]
    if isinstance(value, list):
        return [
            rendered for item in value if (rendered := _display_normalized_value(item)) is not None
        ]
    return _display_normalized_value(value)


def _display_normalized_value(value: Any) -> str | None:
    if value is None:
        return None
    if isinstance(value, bool):
        return str(value).lower()
    if isinstance(value, (str, int, float)):
        return str(value)
    if isinstance(value, dict):
        for key in ("login", "name", "id"):
            candidate = value.get(key)
            if isinstance(candidate, str) and candidate:
                return candidate
        return None
    if isinstance(value, list):
        parts = [part for item in value if (part := _display_normalized_value(item))]
        return ", ".join(parts) or None
    raise AssertionError("custom field normalization admitted an unsupported value")


def _partition_related_attachments(
    attachments: list[dict[str, Any]],
    *,
    issue_comments: bool,
    comments_indexed: bool,
) -> tuple[list[dict[str, Any]], dict[str, list[dict[str, Any]]], list[dict[str, Any]]]:
    """Split descriptors into comment-owned and parent-owned.

    A descriptor whose proven owner is a comment leaves the projection entirely
    when that kind's comments are not indexed: it belongs to a document section
    that does not exist. One whose owner cannot be proven keeps its unknown
    visibility either way, because it may still be the parent's.
    """
    normalized_attachments: list[dict[str, Any]] = []
    by_comment: dict[str, list[dict[str, Any]]] = {}
    parent_attachments: list[dict[str, Any]] = []
    for item in attachments:
        attachment = _normalize_attachment(item, require_comment_owner=issue_comments)
        owner_id = attachment.get("comment_id")
        if isinstance(owner_id, str):
            if not comments_indexed:
                continue
            by_comment.setdefault(owner_id, []).append(attachment)
        else:
            parent_attachments.append(attachment)
        normalized_attachments.append(attachment)
    return normalized_attachments, by_comment, parent_attachments


def _normalized_related_comments(
    comments: list[dict[str, Any]],
    *,
    by_comment: dict[str, list[dict[str, Any]]],
    issue_comments: bool,
) -> list[dict[str, Any]]:
    normalized_comments = []
    comment_ids: set[str] = set()
    for comment in comments:
        comment_id = _required_string(comment, "id")
        if comment_id in comment_ids:
            raise YouTrackError("YouTrack related snapshot contained a duplicate comment id")
        comment_ids.add(comment_id)
        normalized_comments.append(
            _normalize_comment(
                comment,
                attachments=by_comment.pop(comment_id, []),
                issue_comment=issue_comments,
            )
        )
    normalized_comments.sort(key=lambda item: (item["created"], item["id"]))
    return normalized_comments


def _normalized_related_links(
    links: list[dict[str, Any]],
    *,
    link_targets: dict[str, list[dict[str, Any]] | None],
) -> list[dict[str, Any]]:
    normalized_links: list[dict[str, Any]] = []
    link_ids: set[str] = set()
    for link in links:
        link_id = _required_string(link, "id")
        if link_id in link_ids:
            raise YouTrackError("YouTrack related snapshot contained a duplicate link id")
        link_ids.add(link_id)
        normalized_links.append(_normalize_link(link, link_targets.get(link_id, [])))
    normalized_links.sort(key=lambda item: item["id"])
    return normalized_links


def _related_visibility_is_unknown(
    *,
    orphaned_comment_attachments: bool,
    attachments: list[dict[str, Any]],
    comments: list[dict[str, Any]],
    links: list[dict[str, Any]],
) -> bool:
    if orphaned_comment_attachments or any(
        item["visibility"]["kind"] == "unknown" for item in attachments
    ):
        return True
    if any(item["visibility"]["kind"] == "unknown" for item in comments):
        return True
    return any(
        link.get("targets_complete") is not True
        or any(
            target["visibility"]["kind"] == "unknown" or target.get("eligible") is None
            for target in link["targets"]
        )
        for link in links
    )


def _normalize_related_snapshot(
    *,
    comments: list[dict[str, Any]],
    attachments: list[dict[str, Any]],
    links: list[dict[str, Any]],
    link_targets: dict[str, list[dict[str, Any]] | None],
    issue_comments: bool,
    comments_indexed: bool,
    expected_comment_count: int | None,
) -> dict[str, Any]:
    normalized_attachments, by_comment, parent_attachments = _partition_related_attachments(
        attachments,
        issue_comments=issue_comments,
        comments_indexed=comments_indexed,
    )
    normalized_comments = _normalized_related_comments(
        comments,
        by_comment=by_comment,
        issue_comments=issue_comments,
    )
    normalized_links = _normalized_related_links(links, link_targets=link_targets)
    unknown_visibility = _related_visibility_is_unknown(
        orphaned_comment_attachments=bool(by_comment),
        attachments=normalized_attachments,
        comments=normalized_comments,
        links=normalized_links,
    )
    return {
        "comments": normalized_comments,
        "attachments": sorted(parent_attachments, key=lambda item: item["id"]),
        "links": normalized_links,
        "comments_complete": (
            expected_comment_count is None or len(normalized_comments) == expected_comment_count
        ),
        "unknown_visibility": unknown_visibility,
    }


def _normalize_comment(
    comment: dict[str, Any],
    *,
    attachments: list[dict[str, Any]],
    issue_comment: bool,
) -> dict[str, Any]:
    comment_id = _required_string(comment, "id")
    created = _required_timestamp(comment.get("created"), field="comment.created")
    updated = _optional_timestamp(comment.get("updated"), field="comment.updated")
    visibility = _visibility_descriptor(comment.get("visibility"))
    raw_deleted = comment.get("deleted")
    deleted_known = not issue_comment or isinstance(raw_deleted, bool)
    deleted = raw_deleted if isinstance(raw_deleted, bool) else issue_comment
    raw_pinned = comment.get("pinned")
    pinned_known = isinstance(raw_pinned, bool)
    pinned = raw_pinned if pinned_known else False
    if visibility["kind"] == "unlimited" and (not deleted_known or not pinned_known):
        visibility = {"kind": "unknown"}
    text_value = comment.get("text")
    if visibility["kind"] == "unlimited" and not deleted:
        if text_value is not None and not isinstance(text_value, str):
            raise YouTrackError("YouTrack comment text was not a string or null")
        text = text_value or ""
    else:
        text = None
    author = _dict_or_empty(comment.get("author"))
    return {
        "id": comment_id,
        "author": {
            "id": _optional_string(author.get("id")),
            "login": _optional_string(author.get("login")),
        },
        "created": created,
        "updated": updated,
        "deleted": deleted,
        "pinned": pinned,
        "visibility": visibility,
        "text": text,
        "attachments": sorted(attachments, key=lambda item: item["id"]),
    }


def _normalize_attachment(
    attachment: dict[str, Any], *, require_comment_owner: bool
) -> dict[str, Any]:
    attachment_id = _required_string(attachment, "id")
    visibility = _visibility_descriptor(attachment.get("visibility"))
    comment_id, comment_owner_known = _attachment_comment_owner(
        attachment,
        require_comment_owner=require_comment_owner,
    )
    raw_draft = attachment.get("draft")
    raw_removed = attachment.get("removed")
    lifecycle_known = isinstance(raw_draft, bool) and isinstance(raw_removed, bool)
    draft = raw_draft if isinstance(raw_draft, bool) else True
    removed = raw_removed if isinstance(raw_removed, bool) else True
    if visibility["kind"] == "unlimited" and (not comment_owner_known or not lifecycle_known):
        # A missing/malformed ``comment(id)`` cannot prove that the attachment
        # belongs to the public parent rather than to a restricted comment.
        # Treat its effective visibility as unknown and suppress its metadata.
        visibility = {"kind": "unknown"}
    safe = visibility["kind"] == "unlimited"
    name = _optional_string(attachment.get("name")) if safe else None
    mime_type = _optional_string(attachment.get("mimeType")) if safe else None
    size = attachment.get("size") if safe else None
    if size is not None and (isinstance(size, bool) or not isinstance(size, int) or size < 0):
        raise YouTrackError("YouTrack attachment size was not a non-negative integer")
    return {
        "id": attachment_id,
        "comment_id": comment_id,
        "name": name,
        "mime_type": mime_type,
        "size": size,
        "created": _optional_timestamp(attachment.get("created"), field="attachment.created"),
        "updated": _optional_timestamp(attachment.get("updated"), field="attachment.updated"),
        "draft": draft,
        "removed": removed,
        "visibility": visibility,
    }


def _normalize_link(link: dict[str, Any], targets: list[dict[str, Any]] | None) -> dict[str, Any]:
    link_type = _dict_or_empty(link.get("linkType"))
    direction = _optional_string(link.get("direction"))
    normalized_targets: list[dict[str, Any]] = []
    target_ids: set[str] = set()
    for target in targets or []:
        target_id = _required_string(target, "id")
        if target_id in target_ids:
            raise YouTrackError("YouTrack related snapshot contained a duplicate link target id")
        target_ids.add(target_id)
        visibility = _visibility_descriptor(target.get("visibility"))
        eligible = target.get("_ironrag_eligible")
        if not isinstance(eligible, bool):
            eligible = None
        normalized_targets.append(
            {
                "id": target_id,
                "id_readable": (
                    _optional_string(target.get("idReadable"))
                    if visibility["kind"] == "unlimited" and eligible is True
                    else None
                ),
                "visibility": visibility,
                "eligible": eligible,
            }
        )
    return {
        "id": _required_string(link, "id"),
        "direction": direction,
        "type": {
            "id": _optional_string(link_type.get("id")),
            "name": _optional_string(link_type.get("name")),
            "source_to_target": _optional_string(link_type.get("sourceToTarget")),
            "target_to_source": _optional_string(link_type.get("targetToSource")),
        },
        "targets_complete": targets is not None,
        "targets": sorted(normalized_targets, key=lambda item: item["id"]),
    }


def _attachment_comment_owner(
    attachment: dict[str, Any], *, require_comment_owner: bool
) -> tuple[str | None, bool]:
    """Return the comment owner only when the requested projection proves it."""
    if "comment" not in attachment:
        return None, not require_comment_owner
    raw_comment = attachment["comment"]
    if raw_comment is None:
        return None, True
    if not isinstance(raw_comment, dict):
        return None, False
    comment_id = _optional_string(raw_comment.get("id"))
    return (comment_id, comment_id is not None)


def _embedded_link_target_ids(link: dict[str, Any]) -> set[str] | None:
    """Validate the requested embedded target sentinel for a link."""
    if "issues" not in link or not isinstance(link["issues"], list):
        return None
    target_ids: set[str] = set()
    for target in link["issues"]:
        if not isinstance(target, dict):
            return None
        target_id = _optional_string(target.get("id"))
        if target_id is None or target_id in target_ids:
            return None
        target_ids.add(target_id)
    return target_ids


def _render_article_markdown(title: str, article: dict[str, Any]) -> str:
    content = article.get("content")
    if content is not None and not isinstance(content, str):
        raise YouTrackError("YouTrack article content was not a string or null")
    readable_id = _optional_string(article.get("idReadable")) or _required_string(article, "id")
    sections = [f"# {' '.join(title.split()) or 'Untitled'}"]
    sections.append(_render_parent_metadata(article, readable_id=readable_id, kind="Article"))
    body = content.strip("\r\n") if isinstance(content, str) else ""
    if body.strip():
        sections.append(body)
    _append_related_sections(sections, article)
    return "\n\n".join(section for section in sections if section) + "\n"


def _issue_title(issue: dict[str, Any]) -> str:
    return (
        _optional_string(issue.get("summary"))
        or _optional_string(issue.get("idReadable"))
        or _required_string(issue, "id")
    )


def _render_issue_markdown(title: str, readable_id: str, issue: dict[str, Any]) -> str:
    heading = " ".join(title.split()) or "Untitled"
    description = issue.get("description")
    if description is not None and not isinstance(description, str):
        raise YouTrackError("YouTrack issue description was not a string or null")
    body = description.strip("\r\n") if isinstance(description, str) else ""

    sections: list[str] = [f"# {heading}"]
    sections.append(_render_parent_metadata(issue, readable_id=readable_id, kind="Issue"))
    fields = _normalized_custom_fields(issue.get("customFields"))
    field_lines = []
    for name, field in fields.items():
        value = _display_custom_field_value(field)
        if value is not None:
            field_lines.append(f"- **{_escape_markdown(name)}:** {_escape_markdown(value)}")
    if field_lines:
        sections.append("## Fields\n\n" + "\n".join(field_lines))
    if body.strip():
        sections.append(body)
    _append_related_sections(sections, issue)
    return "\n\n".join(sections) + "\n"


def _project_label(project: dict[str, Any]) -> str | None:
    short_name = _optional_string(project.get("shortName"))
    name = _optional_string(project.get("name"))
    if short_name and name and short_name != name:
        return f"{short_name} — {name}"
    return short_name or name


def _timestamp_metadata_line(label: str, raw: Any) -> str | None:
    timestamp = _optional_timestamp(raw, field=label.lower())
    if timestamp is None:
        return None
    return f"- **{label}:** {_format_timestamp(timestamp)}"


def _login_metadata_line(label: str, raw: dict[str, Any]) -> str | None:
    login = _optional_string(raw.get("login"))
    if login is None:
        return None
    return f"- **{label}:** `{_escape_markdown(login)}`"


def _render_parent_metadata(item: dict[str, Any], *, readable_id: str, kind: str) -> str:
    # ``Updated``/``Updated by`` are deliberately not rendered: YouTrack bumps
    # ``updated`` and ``updater`` on every comment add, so a derived line would
    # rewrite the preamble of an otherwise unchanged document and forfeit the
    # server's checksum-identical chunk reuse. ``Updated`` remains in the change
    # token, so the revision is still pushed; only the rendered bytes stay stable.
    project = _dict_or_empty(item.get("project"))
    reporter = _dict_or_empty(item.get("reporter"))
    lines = [f"- **{kind} ID:** `{_escape_markdown(readable_id)}`"]
    internal_id = _optional_string(item.get("id"))
    if internal_id:
        lines.append(f"- **Source ID:** `{_escape_markdown(internal_id)}`")
    project_label = _project_label(project)
    if project_label:
        lines.append(f"- **Project:** {_escape_markdown(project_label)}")
    if line := _timestamp_metadata_line("Created", item.get("created")):
        lines.append(line)
    # ``Resolved`` is a real, rare semantic change and stays; ``updated`` is not.
    if item.get("resolved") is not None:
        resolved = _optional_timestamp(item.get("resolved"), field="resolved")
        if resolved is not None:
            lines.append(f"- **Resolved:** {_format_timestamp(resolved)}")
    if line := _login_metadata_line("Reporter", reporter):
        lines.append(line)
    parent = _dict_or_empty(item.get("parentArticle"))
    parent_readable = _optional_string(parent.get("idReadable"))
    if parent_readable:
        lines.append(f"- **Parent article:** `{_escape_markdown(parent_readable)}`")
    return "## Metadata\n\n" + "\n".join(lines)


def _related_attachment_lines(related: dict[str, Any]) -> list[str]:
    return [
        _render_attachment(attachment)
        for attachment in related["attachments"]
        if _attachment_is_indexable(attachment)
    ]


def _link_label(link: dict[str, Any]) -> Any:
    link_type = link["type"]
    directional = (
        link_type.get("source_to_target")
        if link.get("direction") == "OUTWARD"
        else link_type.get("target_to_source")
    )
    return directional or link_type.get("name") or link["id"]


def _related_link_lines(related: dict[str, Any]) -> list[str]:
    link_lines: list[str] = []
    for link in related["links"]:
        label = _link_label(link)
        for target in link["targets"]:
            if target["visibility"]["kind"] != "unlimited" or target.get("eligible") is not True:
                continue
            target_label = target.get("id_readable") or target["id"]
            link_lines.append(
                f"- **{_escape_markdown(str(label))}:** `{_escape_markdown(str(target_label))}`"
            )
    return link_lines


def _render_comment_section(comment: dict[str, Any]) -> str | None:
    if comment["visibility"]["kind"] != "unlimited" or comment["deleted"] is True:
        return None
    lines = [f"### Comment `{_escape_markdown(comment['id'])}`"]
    author_login = comment["author"].get("login")
    if author_login:
        lines.append(f"- **Author:** `{_escape_markdown(author_login)}`")
    created = comment["created"]
    lines.append(f"- **Created:** {_format_timestamp(created)}")
    # YouTrack sets a comment's ``updated`` equal to ``created`` on creation, so
    # rendering it unconditionally would emit a volatile line. Only a real edit
    # (a different timestamp) is worth a line; the bytes of an untouched comment
    # section stay stable across sweeps.
    updated = comment.get("updated")
    if updated is not None and updated != created:
        lines.append(f"- **Updated:** {_format_timestamp(updated)}")
    if comment.get("pinned") is True:
        lines.append("- **Pinned:** true")
    text = comment.get("text")
    if isinstance(text, str) and text.strip():
        lines.append("\n" + text.strip("\r\n"))
    attachment_lines = [
        _render_attachment(attachment)
        for attachment in comment["attachments"]
        if _attachment_is_indexable(attachment)
    ]
    if attachment_lines:
        lines.append("\nAttachments:\n" + "\n".join(attachment_lines))
    return "\n".join(lines)


def _related_comment_sections(related: dict[str, Any]) -> list[str]:
    return [
        section
        for comment in related["comments"]
        if (section := _render_comment_section(comment)) is not None
    ]


def _append_related_sections(sections: list[str], item: dict[str, Any]) -> None:
    related = _projection(item)["related"]
    if attachment_lines := _related_attachment_lines(related):
        sections.append("## Attachments\n\n" + "\n".join(attachment_lines))
    if link_lines := _related_link_lines(related):
        sections.append("## Links\n\n" + "\n".join(link_lines))
    comment_sections = _related_comment_sections(related)
    if comment_sections:
        sections.append("## Comments\n\n" + "\n\n".join(comment_sections))


def _attachment_is_indexable(attachment: dict[str, Any]) -> bool:
    return (
        attachment["visibility"]["kind"] == "unlimited"
        and attachment.get("removed") is not True
        and attachment.get("draft") is not True
        and isinstance(attachment.get("name"), str)
    )


def _render_attachment(attachment: dict[str, Any]) -> str:
    fields = [
        f"**{_escape_markdown(attachment['name'])}**",
        f"id `{_escape_markdown(attachment['id'])}`",
    ]
    mime_type = attachment.get("mime_type")
    if isinstance(mime_type, str):
        fields.append(f"`{mime_type.replace('`', '')}`")
    size = attachment.get("size")
    if isinstance(size, int):
        fields.append(f"{size} B")
    created = attachment.get("created")
    if isinstance(created, int):
        fields.append(f"created {_format_timestamp(created)}")
    updated = attachment.get("updated")
    if isinstance(updated, int) and updated != created:
        fields.append(f"updated {_format_timestamp(updated)}")
    return "- " + " — ".join(fields)


def _tag_descriptors(raw: Any) -> list[dict[str, Any]]:
    if not isinstance(raw, list) or any(not isinstance(item, dict) for item in raw):
        raise YouTrackError("YouTrack tags was not a collection of objects")
    descriptors: list[dict[str, Any]] = []
    seen_ids: set[str] = set()
    for item in raw:
        tag_id = _required_string(item, "id")
        tag_name = _required_string(item, "name")
        if tag_id in seen_ids:
            raise YouTrackError("YouTrack tags contained a duplicate id")
        seen_ids.add(tag_id)
        descriptors.append({"id": tag_id, "name": tag_name})
    return sorted(descriptors, key=lambda item: (item["id"], item["name"]))


def _dict_items(raw: Any) -> list[dict[str, Any]]:
    if not isinstance(raw, list):
        return []
    return [item for item in raw if isinstance(item, dict)]


def _dict_or_empty(raw: Any) -> dict[str, Any]:
    return raw if isinstance(raw, dict) else {}


def _article_title(article: dict[str, Any]) -> str:
    return (
        _optional_string(article.get("summary"))
        or _optional_string(article.get("idReadable"))
        or _required_string(article, "id")
    )


def _required_string(payload: dict[str, Any], key: str) -> str:
    value = _optional_string(payload.get(key))
    if value is None:
        raise YouTrackError(f"YouTrack payload did not contain {key}")
    return value


def _optional_string(value: Any) -> str | None:
    return value if isinstance(value, str) and value else None


def _required_timestamp(value: Any, *, field: str) -> int:
    timestamp = _optional_timestamp(value, field=field)
    if timestamp is None:
        raise YouTrackError(f"YouTrack {field} did not contain a timestamp")
    return timestamp


def _required_issue_comments_count(issue: dict[str, Any]) -> int:
    value = issue.get("commentsCount")
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise YouTrackError("YouTrack issue commentsCount was not a non-negative integer")
    return value


def _optional_timestamp(value: Any, *, field: str) -> int | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise YouTrackError(f"YouTrack {field} was not a non-negative integer timestamp")
    return value


def _format_timestamp(value: int) -> str:
    return datetime.fromtimestamp(value / 1000, tz=UTC).isoformat().replace("+00:00", "Z")


def _hash_json(value: Any) -> str:
    canonical = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return hashlib.sha256(canonical).hexdigest()


def _safe_filename(value: str, *, default: str) -> str:
    collapsed = _FILENAME_UNSAFE_RE.sub("-", value.strip()).strip("-.")
    return collapsed or default


def _escape_markdown(value: str) -> str:
    return value.replace("\\", "\\\\").replace("*", "\\*").replace("_", "\\_")
