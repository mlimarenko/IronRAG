from __future__ import annotations

import hmac
from hashlib import sha256
from typing import Any

from fastapi import HTTPException, Request, status

from ironrag_connector import Orchestrator, SourceItemRef
from ironrag_connector.server import WebhookHandler

from .adapter import ConfluenceAdapter
from .config import ConfluenceSettings
from .mapping import KIND_PAGE, page_external_key

DELETE_EVENTS = {"page_removed", "page_deleted", "content_removed", "content_deleted"}
UPSERT_EVENTS = {"page_created", "page_updated", "content_created", "content_updated"}


def make_confluence_handler(
    settings: ConfluenceSettings,
    adapter: ConfluenceAdapter,
    orchestrator: Orchestrator,
) -> WebhookHandler:
    def verify_signature(request: Request, body: bytes) -> None:
        secret = settings.confluence_webhook_secret
        if not secret:
            return
        provided = request.headers.get("x-confluence-webhook-signature")
        if not provided:
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="missing X-Confluence-Webhook-Signature",
            )
        expected = hmac.new(secret.encode("utf-8"), body, sha256).hexdigest()
        if not hmac.compare_digest(provided.strip().lower(), expected.lower()):
            raise HTTPException(
                status_code=status.HTTP_401_UNAUTHORIZED,
                detail="signature mismatch",
            )

    async def handle(payload: dict[str, Any]) -> dict[str, Any]:
        event = payload.get("webhookEvent") or payload.get("event")
        if not isinstance(event, str):
            return {"action": "ignored", "reason": "missing event"}

        entity = payload.get("page")
        if not isinstance(entity, dict):
            entity = payload.get("content")
        if not isinstance(entity, dict):
            return {"action": "ignored", "reason": "missing page/content payload"}

        page_id = entity.get("id")
        if not isinstance(page_id, str) or not page_id:
            return {"action": "ignored", "reason": "missing page id"}

        ref = SourceItemRef(
            item_id=page_id,
            kind=KIND_PAGE,
            external_key=page_external_key(page_id),
            change_token=None,
            routing_facts=adapter._routing_facts_for_page(entity),
            raw=entity,
        )

        if event in DELETE_EVENTS:
            outcome = await orchestrator.delete_by_ref(ref)
        elif event in UPSERT_EVENTS:
            outcome = await orchestrator.push_ref(ref)
        else:
            return {"action": "ignored", "reason": f"unsupported event: {event}"}

        return {
            "event": event,
            "action": outcome.action,
            "page_id": page_id,
            "external_key": outcome.ref.external_key,
            "library_id": str(outcome.library_id) if outcome.library_id else None,
            "ironrag_document_id": outcome.ironrag_document_id,
            "detail": outcome.detail,
        }

    return WebhookHandler(
        name="confluence",
        handler=handle,
        extra_auth=verify_signature,
    )
