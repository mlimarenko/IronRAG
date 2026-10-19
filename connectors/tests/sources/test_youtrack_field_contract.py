from __future__ import annotations

from collections import Counter

import ironrag_connector.sources.youtrack.field_contract as field_contract
import ironrag_connector.sources.youtrack.youtrack as youtrack_client
from ironrag_connector.sources.youtrack.field_contract import (
    ALLOWED_DESTINATIONS,
    EXCLUDED_FIELD_GROUPS,
    FIELD_DISPOSITIONS,
    requested_projection_leaf_paths,
)

# These are nested fragments used to compose endpoint projections; they are
# never sent as a complete REST ``fields`` parameter. Keeping the exemptions
# named makes a new ``*_FIELDS`` export fail until it is deliberately classified.
_PROJECTION_FRAGMENT_EXPORTS = frozenset(
    {
        "CUSTOM_FIELD_VALUE_FIELDS",
        "PROJECT_FIELDS",
        "TAG_FIELDS",
        "USER_IDENTITY_FIELDS",
        "VISIBILITY_FIELDS",
    }
)


def test_every_exported_endpoint_projection_is_registered_once() -> None:
    exported_field_constants = {
        name
        for name, value in vars(youtrack_client).items()
        if name.endswith("_FIELDS") and isinstance(value, str)
    }
    registered = [name for name, _, _ in field_contract._PROJECTION_SPECS]

    assert exported_field_constants >= _PROJECTION_FRAGMENT_EXPORTS
    assert exported_field_constants - _PROJECTION_FRAGMENT_EXPORTS == set(registered)
    assert {name for name, count in Counter(registered).items() if count != 1} == set()
    assert all(
        getattr(youtrack_client, name) == projection
        for name, _, projection in field_contract._PROJECTION_SPECS
    )


def test_article_ancestor_descriptor_projection_is_minimal_and_classified() -> None:
    specs = {
        name: (entity, projection) for name, entity, projection in field_contract._PROJECTION_SPECS
    }
    entity, projection = specs["ARTICLE_ANCESTOR_DESCRIPTOR_FIELDS"]
    ancestor_paths = {f"{entity}.{leaf}" for leaf in field_contract._projection_leaves(projection)}
    dispositions = {row.path: row.destinations for row in FIELD_DISPOSITIONS}

    assert ancestor_paths == {
        "article.id",
        "article.parentArticle.id",
        "article.visibility.$type",
    }
    assert ancestor_paths <= dispositions.keys()
    assert dispositions["article.parentArticle.id"] >= {"identity", "security_gate"}
    assert dispositions["article.visibility.$type"] >= {"change_token", "security_gate"}


def test_credential_projection_is_minimal_and_service_only() -> None:
    specs = {
        name: (entity, projection) for name, entity, projection in field_contract._PROJECTION_SPECS
    }
    entity, projection = specs["CREDENTIAL_FIELDS"]
    credential_paths = {
        f"{entity}.{leaf}" for leaf in field_contract._projection_leaves(projection)
    }
    dispositions = {row.path: row.destinations for row in FIELD_DISPOSITIONS}

    assert credential_paths == {
        "credential.id",
        "credential.login",
        "credential.guest",
    }
    assert all("service_only" in dispositions[path] for path in credential_paths)
    assert all("indexed_content" not in dispositions[path] for path in credential_paths)
    assert all("indexed_metadata" not in dispositions[path] for path in credential_paths)


def test_every_requested_field_has_one_explicit_disposition() -> None:
    requested = requested_projection_leaf_paths()
    classified = Counter(row.path for row in FIELD_DISPOSITIONS)

    assert requested - classified.keys() == set(), "unclassified YouTrack REST projection leaves"
    assert classified.keys() - requested == set(), "stale YouTrack field disposition rows"
    assert {path for path, count in classified.items() if count != 1} == set()
    assert all(row.destinations for row in FIELD_DISPOSITIONS)
    assert all(row.destinations <= ALLOWED_DESTINATIONS for row in FIELD_DISPOSITIONS)

    dispositions = {row.path: row.destinations for row in FIELD_DISPOSITIONS}
    assert dispositions["article.parentArticle.idReadable"] == {
        "indexed_metadata",
        "routing",
        "change_token",
    }
    assert dispositions["issue.commentsCount"] == {
        "change_token",
        "security_gate",
        "service_only",
    }
    assert dispositions["attachment.created"] == {"indexed_metadata", "change_token"}
    assert dispositions["attachment.updated"] == {"indexed_metadata", "change_token"}
    # The internal project id is never rendered; only the short-name/name label is.
    assert dispositions["article.project.id"] == {"identity", "routing", "change_token"}
    assert dispositions["issue.project.id"] == {"identity", "routing", "change_token"}


def test_sensitive_and_derived_fields_stay_out_of_rest_projections() -> None:
    requested = requested_projection_leaf_paths()
    requested_segments = {segment for path in requested for segment in path.split(".")}
    forbidden_leaf_names = {
        "email",
        "fullName",
        "avatarUrl",
        "profile",
        "url",
        "base64Content",
        "thumbnailURL",
        "reactions",
        "voters",
        "watchers",
        "workItems",
        "textPreview",
        "wikifiedContent",
        "wikifiedDescription",
        "externalArticle",
        "externalIssue",
        "sharing",
    }
    explicitly_excluded = {path for group in EXCLUDED_FIELD_GROUPS for path in group.wire_paths}

    assert requested_segments.isdisjoint(forbidden_leaf_names)
    assert {
        "attachment.content",
        "article_comment.reactions",
        "issue.watchers",
        "article.wikifiedContent",
        "issue.externalIssue",
        "article.tags.sharing",
        "issue_link_target.tags.sharing",
    } <= explicitly_excluded
    assert all(group.reason for group in EXCLUDED_FIELD_GROUPS)
