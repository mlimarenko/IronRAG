"""BookStack webhook intake glued to the framework's orchestrator.

BookStack POSTs `page_create / page_update / page_delete` envelopes to a
single endpoint. The framework's :func:`build_app` mounts this as
``/webhook/bookstack`` via a :class:`WebhookHandler`. The handler does
HMAC verification on top of the framework's admin-bearer check, parses
the envelope into a :class:`SourceItemRef`, and dispatches it through
``Orchestrator.push_ref`` so the webhook path is identical to the
sync-loop path.
"""

from __future__ import annotations

import hmac
from enum import StrEnum
from hashlib import sha256
from typing import Any

from fastapi import HTTPException, Request, status

from ironrag_connector import Orchestrator, SourceItemRef
from ironrag_connector.observability import get_logger
from ironrag_connector.server import WebhookHandler

from .adapter import BookStackAdapter
from .config import BookStackSettings
from .mapping import KIND_PAGE, page_external_key

log = get_logger(__name__)


class BookStackEventKind(StrEnum):
    PAGE_CREATED = "page_create"
    PAGE_UPDATED = "page_update"
    PAGE_DELETED = "page_delete"


def make_bookstack_handler(
    settings: BookStackSettings,
    adapter: BookStackAdapter,
    orchestrator: Orchestrator,
) -> WebhookHandler:
    def verify_hmac(request: Request, body: bytes) -> None:
        secret = settings.bookstack_webhook_secret
        if not secret:
            return
        provided = request.headers.get("x-bookstack-signature")
        if not provided:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="missing X-BookStack-Signature",
            )
        expected = hmac.new(secret.encode("utf-8"), body, sha256).hexdigest()
        if not hmac.compare_digest(
            provided.strip().lower().encode("utf-8"),
            expected.lower().encode("utf-8"),
        ):
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="signature mismatch",
            )

    async def handle(payload: dict[str, Any]) -> dict[str, Any]:
        try:
            kind = BookStackEventKind(payload.get("event", ""))
        except ValueError:
            return {"action": "ignored", "reason": "non-page event"}

        item = payload.get("related_item") or {}
        page_id = item.get("id")
        if not isinstance(page_id, int) or page_id <= 0:
            return {"action": "ignored", "reason": "missing related_item.id"}

        ref = SourceItemRef(
            item_id=str(page_id),
            kind=KIND_PAGE,
            external_key=page_external_key(page_id),
            change_token=None,
            routing_facts=await adapter._routing_facts_for_page(page_id, item),
            raw=item,
        )

        if kind is BookStackEventKind.PAGE_DELETED:
            del_outcome = await orchestrator.delete_by_ref(ref)
            return {
                "action": del_outcome.action,
                "page_id": page_id,
                "ironrag_document_id": del_outcome.ironrag_document_id,
                "detail": del_outcome.detail,
            }

        outcome = await orchestrator.push_ref(ref)
        return {
            "action": outcome.action,
            "page_id": page_id,
            "external_key": outcome.ref.external_key,
            "library_id": str(outcome.library_id) if outcome.library_id else None,
            "rule": outcome.rule_description,
            "ironrag_document_id": outcome.ironrag_document_id,
            "detail": outcome.detail,
        }

    return WebhookHandler(name="bookstack", handler=handle, extra_auth=verify_hmac)
