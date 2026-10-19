from __future__ import annotations

import pytest

from ironrag_connector.sources.gitrepos.mapping import (
    build_external_key,
    file_external_key,
    file_item_id,
    parse_external_key,
    parse_file_item_id,
)


def test_file_external_key_round_trip() -> None:
    key = file_external_key("docs", "main", "intro.md")
    assert key == "gitrepos:file:docs:main:intro.md"
    parsed = parse_external_key(key)
    assert parsed is not None
    kind, item_id = parsed
    assert kind == "file"
    assert parse_file_item_id(item_id) == ("docs", "main", "intro.md")


def test_external_key_encodes_path_separators() -> None:
    key = file_external_key("api", "release/v1", "src/foo/bar.py")
    # path slashes get URL-encoded so the colon-separated key parses unambiguously
    assert key == "gitrepos:file:api:release%2Fv1:src%2Ffoo%2Fbar.py"
    parsed = parse_external_key(key)
    assert parsed is not None
    _, item_id = parsed
    assert parse_file_item_id(item_id) == ("api", "release/v1", "src/foo/bar.py")


def test_external_key_encodes_colon_in_branch() -> None:
    key = file_external_key("repo", "refs:custom", "a.md")
    parsed = parse_external_key(key)
    assert parsed is not None
    _, item_id = parsed
    assert parse_file_item_id(item_id) == ("repo", "refs:custom", "a.md")


def test_parse_external_key_rejects_foreign_prefix() -> None:
    assert parse_external_key("bookstack:page:1") is None
    assert parse_external_key("gitrepos:") is None
    assert parse_external_key("gitrepos:file:") is None
    assert parse_external_key("gitrepos:other:a:b:c") is None


def test_parse_external_key_rejects_missing_components() -> None:
    assert parse_external_key("gitrepos:file:repo:branch:") is None
    assert parse_external_key("gitrepos:file::branch:path") is None


def test_build_external_key_via_item_id() -> None:
    item_id = file_item_id("docs", "main", "intro.md")
    assert build_external_key("file", item_id) == "gitrepos:file:docs:main:intro.md"


def test_build_external_key_rejects_bad_kind() -> None:
    item_id = file_item_id("docs", "main", "x.md")
    with pytest.raises(ValueError):
        build_external_key("page", item_id)


def test_build_external_key_rejects_malformed_item_id() -> None:
    with pytest.raises(ValueError):
        build_external_key("file", "not-a-triple")


def test_build_external_key_empty_item_id_returns_kind_prefix() -> None:
    # The framework's reaper uses an empty item_id to ask for the
    # kind's prefix when it scans IronRAG for orphans.
    assert build_external_key("file", "") == "gitrepos:file:"
