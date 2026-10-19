"""External-key vocabulary for the BookStack connector.

Three independent ``kind`` values, all under the ``bookstack:`` prefix:

* ``bookstack:page:<page_id>`` — one IronRAG document per wiki page.
* ``bookstack:attachment:<attachment_id>`` — one document per attachment.
* ``bookstack:image:<sha256_hex_prefix_16>`` — content-addressed; identical
  bytes referenced across multiple pages collapse onto a single IronRAG
  document.
"""

from __future__ import annotations

CONNECTOR_NAME = "bookstack"
KIND_PAGE = "page"
KIND_ATTACHMENT = "attachment"
KIND_IMAGE = "image"
KINDS: tuple[str, ...] = (KIND_PAGE, KIND_ATTACHMENT, KIND_IMAGE)

PAGE_PREFIX = f"{CONNECTOR_NAME}:{KIND_PAGE}:"
ATTACHMENT_PREFIX = f"{CONNECTOR_NAME}:{KIND_ATTACHMENT}:"
IMAGE_PREFIX = f"{CONNECTOR_NAME}:{KIND_IMAGE}:"


def page_external_key(page_id: int) -> str:
    if page_id <= 0:
        raise ValueError("page_id must be a positive integer")
    return f"{PAGE_PREFIX}{page_id}"


def attachment_external_key(attachment_id: int) -> str:
    if attachment_id <= 0:
        raise ValueError("attachment_id must be a positive integer")
    return f"{ATTACHMENT_PREFIX}{attachment_id}"


def image_external_key(content_sha256_hex: str) -> str:
    if not content_sha256_hex or len(content_sha256_hex) < 16:
        raise ValueError("content sha256 hex must be >= 16 chars")
    return f"{IMAGE_PREFIX}{content_sha256_hex[:16]}"


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
