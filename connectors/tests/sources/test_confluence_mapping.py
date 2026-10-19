from __future__ import annotations

from ironrag_connector.sources.confluence.mapping import (
    attachment_external_key,
    page_external_key,
    parse_external_key,
)


def test_mapping_roundtrip() -> None:
    key = page_external_key("12345")
    assert key == "confluence:page:12345"
    assert parse_external_key(key) == ("page", "12345")


def test_attachment_key() -> None:
    key = attachment_external_key("att-99")
    assert key == "confluence:attachment:att-99"
    assert parse_external_key(key) == ("attachment", "att-99")


def test_parse_external_key_rejects_other_prefix() -> None:
    assert parse_external_key("bookstack:page:1") is None
