from __future__ import annotations

from ironrag_connector.sources.youtrack.mapping import (
    article_external_key,
    build_external_key,
    issue_external_key,
    parse_external_key,
)


def test_article_mapping_round_trip_uses_database_id() -> None:
    key = article_external_key("226-0")

    assert key == "youtrack:article:226-0"
    assert parse_external_key(key) == ("article", "226-0")


def test_issue_mapping_round_trip_uses_readable_id() -> None:
    key = issue_external_key("IRG-83")

    assert key == "youtrack:issue:IRG-83"
    assert parse_external_key(key) == ("issue", "IRG-83")


def test_mapping_rejects_foreign_malformed_and_unknown_keys() -> None:
    assert parse_external_key("confluence:page:1") is None
    assert parse_external_key("youtrack:article:") is None
    assert parse_external_key("youtrack:attachment:42") is None


def test_build_external_key_rejects_unknown_kind() -> None:
    try:
        build_external_key("comment", "42")
    except ValueError as exc:
        assert "comment" in str(exc)
    else:  # pragma: no cover - makes the failure message explicit
        raise AssertionError("unknown kinds must not mint connector-owned keys")


def test_build_external_key_supports_framework_prefix_scan() -> None:
    assert build_external_key("article", "") == "youtrack:article:"
