from __future__ import annotations

import pytest

from ironrag_connector.sources.bookstack.mapping import (
    attachment_external_key,
    build_external_key,
    image_external_key,
    page_external_key,
    parse_external_key,
)


def test_page_external_key_round_trip() -> None:
    key = page_external_key(42)
    assert key == "bookstack:page:42"
    assert parse_external_key(key) == ("page", "42")


def test_attachment_external_key_round_trip() -> None:
    key = attachment_external_key(7)
    assert key == "bookstack:attachment:7"
    assert parse_external_key(key) == ("attachment", "7")


def test_image_external_key_prefix_only() -> None:
    digest = "abcd1234deadbeef0123456789abcdef0123456789abcdef0123456789abcdef"
    key = image_external_key(digest)
    assert key == "bookstack:image:abcd1234deadbeef"
    assert parse_external_key(key) == ("image", "abcd1234deadbeef")


def test_page_external_key_rejects_non_positive() -> None:
    with pytest.raises(ValueError):
        page_external_key(0)
    with pytest.raises(ValueError):
        page_external_key(-1)


def test_image_external_key_rejects_short_digest() -> None:
    with pytest.raises(ValueError):
        image_external_key("abcd")


def test_parse_external_key_rejects_foreign_prefix() -> None:
    assert parse_external_key("confluence:page:1") is None
    assert parse_external_key("bookstack:") is None
    assert parse_external_key("bookstack:page:") is None


def test_build_external_key_matches_specific_helpers() -> None:
    assert build_external_key("page", "42") == page_external_key(42)
    assert build_external_key("attachment", "7") == attachment_external_key(7)
