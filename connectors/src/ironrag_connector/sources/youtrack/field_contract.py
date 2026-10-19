"""Auditable disposition contract for every YouTrack REST field we request."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from .youtrack import (
    ARTICLE_ANCESTOR_DESCRIPTOR_FIELDS,
    ARTICLE_COMMENT_FIELDS,
    ARTICLE_DETAIL_FIELDS,
    ARTICLE_SUMMARY_FIELDS,
    ATTACHMENT_FIELDS,
    CREDENTIAL_FIELDS,
    ISSUE_COMMENT_FIELDS,
    ISSUE_DETAIL_FIELDS,
    ISSUE_LINK_FIELDS,
    ISSUE_LINK_TARGET_FIELDS,
    ISSUE_SUMMARY_BASE_FIELDS,
    ISSUE_SUMMARY_FIELDS,
)

type FieldDestination = Literal[
    "identity",
    "indexed_content",
    "indexed_metadata",
    "routing",
    "change_token",
    "security_gate",
    "service_only",
]

ALLOWED_DESTINATIONS: frozenset[FieldDestination] = frozenset(
    {
        "identity",
        "indexed_content",
        "indexed_metadata",
        "routing",
        "change_token",
        "security_gate",
        "service_only",
    }
)


@dataclass(frozen=True)
class FieldDisposition:
    """One exact projected leaf and every connector destination it reaches."""

    path: str
    destinations: frozenset[FieldDestination]


@dataclass(frozen=True)
class ExcludedFieldGroup:
    """Fields intentionally kept outside the YouTrack REST projection."""

    category: str
    wire_paths: tuple[str, ...]
    reason: str


_PROJECTION_SPECS: tuple[tuple[str, str, str], ...] = (
    ("CREDENTIAL_FIELDS", "credential", CREDENTIAL_FIELDS),
    ("ARTICLE_SUMMARY_FIELDS", "article", ARTICLE_SUMMARY_FIELDS),
    ("ARTICLE_DETAIL_FIELDS", "article", ARTICLE_DETAIL_FIELDS),
    (
        "ARTICLE_ANCESTOR_DESCRIPTOR_FIELDS",
        "article",
        ARTICLE_ANCESTOR_DESCRIPTOR_FIELDS,
    ),
    ("ISSUE_SUMMARY_BASE_FIELDS", "issue", ISSUE_SUMMARY_BASE_FIELDS),
    ("ISSUE_SUMMARY_FIELDS", "issue", ISSUE_SUMMARY_FIELDS),
    ("ISSUE_DETAIL_FIELDS", "issue", ISSUE_DETAIL_FIELDS),
    ("ARTICLE_COMMENT_FIELDS", "article_comment", ARTICLE_COMMENT_FIELDS),
    ("ISSUE_COMMENT_FIELDS", "issue_comment", ISSUE_COMMENT_FIELDS),
    ("ATTACHMENT_FIELDS", "attachment", ATTACHMENT_FIELDS),
    ("ISSUE_LINK_FIELDS", "issue_link", ISSUE_LINK_FIELDS),
    ("ISSUE_LINK_TARGET_FIELDS", "issue_link_target", ISSUE_LINK_TARGET_FIELDS),
)


class _ProjectionParser:
    def __init__(self, projection: str) -> None:
        self._projection = projection
        self._index = 0

    def parse(self) -> tuple[str, ...]:
        leaves = self._parse_level((), nested=False)
        if self._index != len(self._projection):
            self._fail()
        return tuple(leaves)

    def _parse_level(self, prefix: tuple[str, ...], *, nested: bool) -> list[str]:
        leaves: list[str] = []
        while True:
            name = self._take_name()
            path = (*prefix, name)
            if self._consume("("):
                leaves.extend(self._parse_level(path, nested=True))
            else:
                leaves.append(".".join(path))

            if self._index == len(self._projection):
                if nested:
                    self._fail()
                return leaves
            delimiter = self._projection[self._index]
            self._index += 1
            if delimiter == ",":
                continue
            if delimiter == ")" and nested:
                return leaves
            self._fail()

    def _take_name(self) -> str:
        start = self._index
        while self._index < len(self._projection) and self._projection[self._index] not in ",()":
            self._index += 1
        name = self._projection[start : self._index].strip()
        if not name:
            self._fail()
        return name

    def _consume(self, token: str) -> bool:
        if self._index == len(self._projection) or self._projection[self._index] != token:
            return False
        self._index += 1
        return True

    def _fail(self) -> None:
        raise ValueError(f"malformed YouTrack field projection: {self._projection!r}")


def _projection_leaves(projection: str) -> tuple[str, ...]:
    """Parse YouTrack's ``fields=a,b(c,d)`` grammar into dotted leaf paths."""

    return _ProjectionParser(projection).parse()


def requested_projection_leaf_paths() -> frozenset[str]:
    """Return the exact entity-qualified leaves sent in REST ``fields`` params."""

    return frozenset(
        f"{entity}.{leaf}"
        for _, entity, projection in _PROJECTION_SPECS
        for leaf in _projection_leaves(projection)
    )


def _field(path: str, *destinations: FieldDestination) -> FieldDisposition:
    return FieldDisposition(path=path, destinations=frozenset(destinations))


# Every row names one wire leaf. A new REST leaf must not acquire a disposition
# by matching a wildcard or by inheriting the treatment of its parent object.
FIELD_DISPOSITIONS: tuple[FieldDisposition, ...] = (
    _field("credential.id", "identity", "security_gate", "service_only"),
    _field("credential.login", "security_gate", "service_only"),
    _field("credential.guest", "security_gate", "service_only"),
    _field("article.id", "identity", "indexed_metadata", "routing", "change_token"),
    _field("article.idReadable", "indexed_metadata", "routing", "change_token"),
    _field("article.summary", "indexed_content", "change_token"),
    _field("article.created", "indexed_metadata", "change_token"),
    _field("article.updated", "indexed_metadata", "change_token"),
    _field("article.project.id", "identity", "routing", "change_token"),
    _field("article.project.name", "indexed_metadata", "routing", "change_token"),
    _field("article.project.shortName", "indexed_metadata", "routing", "change_token"),
    _field("article.project.archived", "security_gate"),
    _field("article.reporter.id", "identity", "change_token"),
    _field("article.reporter.login", "indexed_metadata", "routing", "change_token"),
    _field("article.updatedBy.id", "identity", "change_token"),
    _field("article.updatedBy.login", "indexed_metadata", "change_token"),
    _field("article.parentArticle.id", "identity", "routing", "change_token", "security_gate"),
    _field("article.parentArticle.idReadable", "indexed_metadata", "routing", "change_token"),
    _field("article.tags.id", "identity", "change_token"),
    _field("article.tags.name", "routing", "change_token", "security_gate"),
    _field("article.visibility.$type", "change_token", "security_gate"),
    _field("article.content", "indexed_content"),
    _field("issue.id", "identity", "indexed_metadata", "routing", "change_token"),
    _field("issue.idReadable", "indexed_metadata", "routing", "change_token"),
    _field("issue.summary", "indexed_content", "change_token"),
    _field("issue.created", "indexed_metadata", "change_token"),
    _field("issue.updated", "indexed_metadata", "change_token"),
    _field("issue.resolved", "indexed_metadata", "change_token"),
    _field("issue.commentsCount", "change_token", "security_gate", "service_only"),
    _field("issue.project.id", "identity", "routing", "change_token"),
    _field("issue.project.name", "indexed_metadata", "routing", "change_token"),
    _field("issue.project.shortName", "indexed_metadata", "routing", "change_token"),
    _field("issue.project.archived", "security_gate"),
    _field("issue.reporter.id", "identity", "change_token"),
    _field("issue.reporter.login", "indexed_metadata", "routing", "change_token"),
    _field("issue.updater.id", "identity", "change_token"),
    _field("issue.updater.login", "indexed_metadata", "change_token"),
    _field("issue.tags.id", "identity", "change_token"),
    _field("issue.tags.name", "routing", "change_token", "security_gate"),
    _field("issue.visibility.$type", "change_token", "security_gate"),
    _field("issue.customFields.name", "indexed_metadata", "routing", "change_token"),
    _field("issue.customFields.$type", "change_token"),
    _field(
        "issue.customFields.value.id",
        "identity",
        "indexed_metadata",
        "routing",
        "change_token",
    ),
    _field("issue.customFields.value.name", "indexed_metadata", "routing", "change_token"),
    _field("issue.customFields.value.login", "indexed_metadata", "routing", "change_token"),
    _field("issue.description", "indexed_content"),
    _field("article_comment.id", "identity", "indexed_metadata", "change_token"),
    _field("article_comment.text", "indexed_content", "change_token", "security_gate"),
    _field("article_comment.created", "indexed_metadata", "change_token"),
    _field("article_comment.updated", "indexed_metadata", "change_token"),
    _field("article_comment.pinned", "indexed_metadata", "change_token"),
    _field("article_comment.author.id", "identity", "change_token"),
    _field("article_comment.author.login", "indexed_metadata", "change_token"),
    _field("article_comment.visibility.$type", "change_token", "security_gate"),
    _field("issue_comment.id", "identity", "indexed_metadata", "change_token"),
    _field("issue_comment.text", "indexed_content", "change_token", "security_gate"),
    _field("issue_comment.created", "indexed_metadata", "change_token"),
    _field("issue_comment.updated", "indexed_metadata", "change_token"),
    _field("issue_comment.deleted", "change_token", "security_gate"),
    _field("issue_comment.pinned", "indexed_metadata", "change_token"),
    _field("issue_comment.author.id", "identity", "change_token"),
    _field("issue_comment.author.login", "indexed_metadata", "change_token"),
    _field("issue_comment.visibility.$type", "change_token", "security_gate"),
    _field("attachment.id", "identity", "indexed_metadata", "change_token"),
    _field("attachment.name", "indexed_metadata", "change_token", "security_gate"),
    _field("attachment.created", "indexed_metadata", "change_token"),
    _field("attachment.updated", "indexed_metadata", "change_token"),
    _field("attachment.size", "indexed_metadata", "change_token", "security_gate"),
    _field("attachment.mimeType", "indexed_metadata", "change_token", "security_gate"),
    _field("attachment.draft", "change_token", "security_gate"),
    _field("attachment.removed", "change_token", "security_gate"),
    _field("attachment.comment.id", "identity", "change_token", "service_only"),
    _field("attachment.visibility.$type", "change_token", "security_gate"),
    _field("issue_link.id", "identity", "indexed_metadata", "change_token"),
    _field("issue_link.direction", "indexed_metadata", "change_token"),
    _field("issue_link.linkType.id", "identity", "change_token"),
    _field("issue_link.linkType.name", "indexed_metadata", "change_token"),
    _field("issue_link.linkType.sourceToTarget", "indexed_metadata", "change_token"),
    _field("issue_link.linkType.targetToSource", "indexed_metadata", "change_token"),
    _field("issue_link.issues.id", "service_only"),
    _field("issue_link_target.id", "identity", "indexed_metadata", "change_token"),
    _field("issue_link_target.idReadable", "indexed_metadata", "change_token"),
    _field("issue_link_target.project.id", "identity", "service_only", "security_gate"),
    _field("issue_link_target.project.name", "service_only", "security_gate"),
    _field("issue_link_target.project.shortName", "service_only", "security_gate"),
    _field("issue_link_target.project.archived", "service_only", "security_gate"),
    _field("issue_link_target.tags.id", "service_only", "security_gate"),
    _field("issue_link_target.tags.name", "service_only", "security_gate"),
    _field("issue_link_target.visibility.$type", "change_token", "security_gate"),
)


EXCLUDED_FIELD_GROUPS: tuple[ExcludedFieldGroup, ...] = (
    ExcludedFieldGroup(
        category="user contact and profile data",
        wire_paths=(
            "article.reporter.email",
            "article.reporter.fullName",
            "article.reporter.avatarUrl",
            "article.reporter.profile",
            "article.updatedBy.email",
            "article.updatedBy.fullName",
            "article.updatedBy.avatarUrl",
            "article.updatedBy.profile",
            "issue.reporter.email",
            "issue.reporter.fullName",
            "issue.reporter.avatarUrl",
            "issue.reporter.profile",
            "issue.updater.email",
            "issue.updater.fullName",
            "issue.updater.avatarUrl",
            "issue.updater.profile",
            "article_comment.author.email",
            "article_comment.author.fullName",
            "article_comment.author.avatarUrl",
            "article_comment.author.profile",
            "issue_comment.author.email",
            "issue_comment.author.fullName",
            "issue_comment.author.avatarUrl",
            "issue_comment.author.profile",
            "issue.customFields.value.email",
            "issue.customFields.value.fullName",
            "issue.customFields.value.avatarUrl",
            "issue.customFields.value.profile",
        ),
        reason="Stable user ids and logins are sufficient; personal profile data is not indexed.",
    ),
    ExcludedFieldGroup(
        category="download and binary surfaces",
        wire_paths=(
            "attachment.url",
            "attachment.content",
            "attachment.base64Content",
            "attachment.thumbnailURL",
        ),
        reason="The parent document records safe attachment metadata, never bytes or signed URLs.",
    ),
    ExcludedFieldGroup(
        category="social and work activity",
        wire_paths=(
            "article_comment.reactions",
            "issue_comment.reactions",
            "issue.voters",
            "issue.watchers",
            "issue.workItems",
        ),
        reason=(
            "These collections expose identities or activity that the search corpus does not need."
        ),
    ),
    ExcludedFieldGroup(
        category="derived previews and HTML",
        wire_paths=(
            "article_comment.textPreview",
            "issue_comment.textPreview",
            "article.wikifiedContent",
            "issue.wikifiedDescription",
        ),
        reason=(
            "Canonical Markdown or plain text is indexed once; derived renderings "
            "would duplicate it."
        ),
    ),
    ExcludedFieldGroup(
        category="external integration records",
        wire_paths=("article.externalArticle", "issue.externalIssue"),
        reason="Integration payloads have a separate access model and are not search metadata.",
    ),
    ExcludedFieldGroup(
        category="tag sharing",
        wire_paths=(
            "article.tags.sharing",
            "issue.tags.sharing",
            "issue_link_target.tags.sharing",
        ),
        reason="Tag sharing is not enough to prove a reader can access the tagged parent.",
    ),
)
