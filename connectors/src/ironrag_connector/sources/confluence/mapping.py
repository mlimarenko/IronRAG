from __future__ import annotations

CONNECTOR_NAME = "confluence"
KIND_PAGE = "page"
KIND_ATTACHMENT = "attachment"
KINDS: tuple[str, ...] = (KIND_PAGE, KIND_ATTACHMENT)

PAGE_PREFIX = f"{CONNECTOR_NAME}:{KIND_PAGE}:"
ATTACHMENT_PREFIX = f"{CONNECTOR_NAME}:{KIND_ATTACHMENT}:"


def page_external_key(page_id: str) -> str:
    return f"{PAGE_PREFIX}{page_id}"


def attachment_external_key(attachment_id: str) -> str:
    return f"{ATTACHMENT_PREFIX}{attachment_id}"


def build_external_key(kind: str, item_id: str) -> str:
    return f"{CONNECTOR_NAME}:{kind}:{item_id}"


def parse_external_key(external_key: str) -> tuple[str, str] | None:
    prefix = f"{CONNECTOR_NAME}:"
    if not external_key.startswith(prefix):
        return None
    rest = external_key[len(prefix) :]
    kind, _, item_id = rest.partition(":")
    if not kind or not item_id:
        return None
    return kind, item_id
