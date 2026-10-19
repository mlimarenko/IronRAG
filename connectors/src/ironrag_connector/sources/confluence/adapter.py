"""SourceAdapter implementation for Confluence.

Pages are the primary kind; attachments are dependents of the page fetch
that materializes them. A page's ``dependents_token`` is derived from the
instance-wide attachment index (one ``type=attachment`` CQL walk per sweep:
attachment id and version, grouped by container page), so an added, removed
or re-uploaded attachment moves the token and the framework re-proves the
page's dependent set. A failure to read the index fails the enumeration.

The materialized attachment tuple is the page's complete dependent set: a
list or download that cannot be read defers the page, so the framework keeps
the previously proven set. A download that returns no bytes is such a read,
not a deletion, and defers too. Only a deliberate exclusion leaves an
attachment out of the set and so proves that no copy of it is expected:
today that is exceeding ``confluence_attachment_max_bytes``.
"""

from __future__ import annotations

import re
from collections.abc import AsyncIterator
from typing import Any
from urllib.parse import urlsplit

import httpx

from ironrag_connector import (
    IncompleteCensusError,
    SourceAdapter,
    SourceDeleteEnforcement,
    SourceFetchDeferred,
    SourceFetchDelete,
    SourceFetchMaterialized,
    SourceFetchResult,
    SourceItem,
    SourceItemRef,
    dependents_token_from,
)
from ironrag_connector.observability import get_logger

from .config import ConfluenceSettings
from .confluence import ConfluenceClient, ConfluenceError, ConfluenceNotFoundError
from .mapping import (
    CONNECTOR_NAME,
    KIND_ATTACHMENT,
    KIND_PAGE,
    KINDS,
    attachment_external_key,
    build_external_key,
    page_external_key,
    parse_external_key,
)

log = get_logger(__name__)

_FILENAME_SANITIZE_RE = re.compile(r"[^a-zA-Z0-9._-]+")


class ConfluenceAdapter(SourceAdapter):
    name = CONNECTOR_NAME
    kinds = KINDS
    primary_kinds = (KIND_PAGE,)

    def __init__(self, settings: ConfluenceSettings) -> None:
        self._settings = settings
        self._client = ConfluenceClient(settings)
        self._base_url = settings.confluence_base_url.rstrip("/")

    async def close(self) -> None:
        await self._client.aclose()

    def external_key(self, kind: str, item_id: str) -> str:
        return build_external_key(kind, item_id)

    def parse_external_key(self, external_key: str) -> tuple[str, str] | None:
        return parse_external_key(external_key)

    def missing_enforcement(self, kind: str) -> SourceDeleteEnforcement:
        if kind in (KIND_PAGE, KIND_ATTACHMENT):
            return SourceDeleteEnforcement.POLICY
        raise ValueError(f"unsupported {self.name} item kind: {kind}")

    async def iter_items(self) -> AsyncIterator[SourceItemRef]:
        attachment_index = await self._attachment_index()
        empty_set_token = dependents_token_from(())
        malformed_records = 0
        async for page in self._client.list_pages():
            page_id = page.get("id")
            if not isinstance(page_id, str) or not page_id.strip():
                # Keep enumerating so well-formed pages still ship; the census
                # itself is no longer proof of absence.
                malformed_records += 1
                continue
            facts = self._routing_facts_for_page(page)
            yield SourceItemRef(
                item_id=page_id,
                kind=KIND_PAGE,
                external_key=page_external_key(page_id),
                change_token=_extract_change_token(page),
                dependents_token=attachment_index.get(page_id, empty_set_token),
                routing_facts=facts,
                raw=page,
            )
        if malformed_records:
            raise IncompleteCensusError(
                f"Confluence page census contained {malformed_records} record(s) with an invalid id"
            )

    async def _attachment_index(self) -> dict[str, str]:
        """Per-page dependents token from the instance-wide attachment listing."""
        if not self._settings.confluence_include_attachments:
            # The proven dependent set of every page is empty; the index would
            # only move tokens for attachments that are never materialized.
            return {}
        entries: dict[str, list[str]] = {}
        async for attachment in self._client.list_attachment_index():
            container_id = attachment["container"]["id"]
            entries.setdefault(container_id, []).append(
                f"{attachment['id']}:{_extract_change_token(attachment)}"
            )
        return {page_id: dependents_token_from(lines) for page_id, lines in entries.items()}

    async def fetch(self, ref: SourceItemRef) -> SourceFetchResult:
        if ref.kind != KIND_PAGE:
            return SourceFetchDeferred(f"unexpected ref kind: {ref.kind}")
        try:
            page = await self._client.get_page(ref.item_id)
        except ConfluenceNotFoundError:
            return SourceFetchDelete(
                SourceDeleteEnforcement.POLICY,
                "Confluence page no longer exists",
                cacheable=False,
            )
        returned_page_id = page.get("id")
        if returned_page_id != ref.item_id:
            log.warning(
                "confluence.fetch.identity_mismatch",
                requested_page_id=ref.item_id,
                returned_page_id=returned_page_id,
            )
            return SourceFetchDeferred("Confluence page identity did not match the requested page")

        title = page.get("title") if isinstance(page.get("title"), str) else None
        html = _extract_best_html(page)
        facts = self._routing_facts_for_page(page)
        page_ref = SourceItemRef(
            item_id=ref.item_id,
            kind=KIND_PAGE,
            external_key=page_external_key(ref.item_id),
            change_token=_extract_change_token(page),
            dependents_token=ref.dependents_token,
            routing_facts=facts,
            raw=page,
        )

        # Build attachment dependents before deciding whether the page is
        # ingestable. A page can carry no readable body of its own yet still
        # host attachments (specs, PDFs, images) that are the real content —
        # dropping the page on an empty body would silently lose every one of
        # those attachment documents.
        dependents = await self._build_attachment_dependents(page_ref, page)
        if isinstance(dependents, SourceFetchDeferred):
            return dependents

        if not html:
            if not dependents:
                log.warning("confluence.fetch.empty_body", page_id=ref.item_id)
                return SourceFetchDelete(
                    SourceDeleteEnforcement.REQUIRED,
                    "Confluence page has no ingestable body or attachments",
                    # The page revision does not advance when an attachment
                    # appears or attachment ingestion is enabled later.
                    cacheable=False,
                )
            log.info(
                "confluence.fetch.empty_body_with_attachments",
                page_id=ref.item_id,
                attachment_count=len(dependents),
            )
            html = _synthesize_attachment_index_html(dependents)

        wrapped_html = _wrap_html_document(title or f"Page {ref.item_id}", html)
        item = SourceItem(
            ref=page_ref,
            payload=wrapped_html.encode("utf-8"),
            mime_type="text/html",
            file_name=f"{_safe_filename(title or f'page-{ref.item_id}')}.html",
            title=title,
            document_hint=self._document_hint(page),
            dependents=dependents,
        )
        return SourceFetchMaterialized(item)

    async def _build_attachment_dependents(
        self,
        page_ref: SourceItemRef,
        page: dict[str, Any],
    ) -> tuple[SourceItem, ...] | SourceFetchDeferred:
        if not self._settings.confluence_include_attachments:
            return ()

        page_id = page_ref.item_id
        page_title = page.get("title") if isinstance(page.get("title"), str) else "Page"
        try:
            attachments = await self._client.list_attachments(page_id)
        except ConfluenceError as exc:
            log.warning("confluence.attachments.list_failed", page_id=page_id, error=str(exc))
            return SourceFetchDeferred("Confluence attachment list could not be read completely")

        dependents: list[SourceItem] = []
        for attachment in attachments:
            attachment_id = attachment.get("id")
            if not isinstance(attachment_id, str) or not attachment_id.strip():
                return SourceFetchDeferred("Confluence attachment metadata contained an invalid id")
            links = attachment.get("_links")
            if not isinstance(links, dict):
                return SourceFetchDeferred("Confluence attachment metadata contained invalid links")
            download_url = links.get("download")
            if not isinstance(download_url, str) or not download_url.strip():
                return SourceFetchDeferred(
                    "Confluence attachment metadata contained an invalid download link"
                )

            try:
                payload, mime_type = await self._client.download(download_url)
            except ConfluenceNotFoundError as exc:
                log.info(
                    "confluence.attachments.download_snapshot_changed",
                    page_id=page_id,
                    attachment_id=attachment_id,
                    error=str(exc),
                )
                return SourceFetchDeferred(
                    "Confluence attachment snapshot changed during materialization"
                )
            except (ConfluenceError, httpx.HTTPError) as exc:
                # httpx.HTTPError covers the transport failures the client does
                # not wrap (read timeout, connection reset): a fetch that ends
                # in one has not enumerated this page's attachments, so it must
                # defer instead of raising past the census contract.
                log.warning(
                    "confluence.attachments.download_failed",
                    page_id=page_id,
                    attachment_id=attachment_id,
                    error_type=type(exc).__name__,
                    error=str(exc) or repr(exc),
                )
                return SourceFetchDeferred(
                    "Confluence attachment bytes could not be read completely"
                )

            if not payload:
                # A 2xx with no body is a read that returned nothing, not
                # Confluence saying the attachment is gone; proving an absence
                # from it would delete a live attachment.
                log.warning(
                    "confluence.attachments.empty_payload",
                    page_id=page_id,
                    attachment_id=attachment_id,
                    mime_type=mime_type,
                )
                return SourceFetchDeferred("Confluence attachment download returned no bytes")
            if len(payload) > self._settings.confluence_attachment_max_bytes:
                log.info(
                    "confluence.attachments.skipped_too_large",
                    attachment_id=attachment_id,
                    bytes=len(payload),
                    max_bytes=self._settings.confluence_attachment_max_bytes,
                )
                continue

            att_title = attachment.get("title")
            att_name = att_title if isinstance(att_title, str) else f"attachment-{attachment_id}"
            att_ref = SourceItemRef(
                item_id=attachment_id,
                kind=KIND_ATTACHMENT,
                external_key=attachment_external_key(attachment_id),
                change_token=_extract_change_token(attachment),
                routing_facts=page_ref.routing_facts,
                raw=attachment,
            )
            dependents.append(
                SourceItem(
                    ref=att_ref,
                    payload=payload,
                    mime_type=mime_type or "application/octet-stream",
                    file_name=_safe_filename(att_name),
                    title=f"{page_title}: {att_name}",
                    document_hint=self._attachment_hint(download_url),
                )
            )
        return tuple(dependents)

    def _routing_facts_for_page(self, page: dict[str, Any]) -> dict[str, Any]:
        space = page.get("space")
        space_key = None
        space_name = None
        if isinstance(space, dict):
            if isinstance(space.get("key"), str):
                space_key = space["key"]
            if isinstance(space.get("name"), str):
                space_name = space["name"]

        ancestors = page.get("ancestors", [])
        ancestor_titles: list[str] = []
        ancestor_ids: list[str] = []
        if isinstance(ancestors, list):
            for ancestor in ancestors:
                if not isinstance(ancestor, dict):
                    continue
                aid = ancestor.get("id")
                if isinstance(aid, str):
                    ancestor_ids.append(aid)
                atitle = ancestor.get("title")
                if isinstance(atitle, str):
                    ancestor_titles.append(atitle)

        return {
            "space": space_key,
            "space_key": space_key,
            "space_name": space_name,
            "ancestor_id": ancestor_ids,
            "ancestor_title": ancestor_titles,
            "status": page.get("status") if isinstance(page.get("status"), str) else None,
        }

    def _document_hint(self, page: dict[str, Any]) -> str | None:
        links = page.get("_links")
        if not isinstance(links, dict):
            return None
        webui = links.get("webui")
        if isinstance(webui, str) and webui:
            if urlsplit(webui).scheme in {"http", "https"}:
                return webui
            return f"{self._base_url}{webui}"
        return None

    def _attachment_hint(self, download_url: str) -> str:
        if urlsplit(download_url).scheme in {"http", "https"}:
            return download_url
        return f"{self._base_url}{download_url}"


def _extract_best_html(page: dict[str, Any]) -> str | None:
    body = page.get("body")
    if not isinstance(body, dict):
        return None
    export_view = body.get("export_view")
    if isinstance(export_view, dict):
        value = export_view.get("value")
        if isinstance(value, str) and value.strip():
            return value
    storage = body.get("storage")
    if isinstance(storage, dict):
        value = storage.get("value")
        if isinstance(value, str) and value.strip():
            return value
    return None


def _synthesize_attachment_index_html(dependents: tuple[SourceItem, ...]) -> str:
    """Build a minimal body for a page that has no readable text of its own
    but carries attachments. The body lists each attachment so the page
    document is a useful index that links to its attachment documents,
    instead of being dropped entirely."""
    rows: list[str] = []
    for dep in dependents:
        label = dep.title or dep.file_name
        href = dep.document_hint
        if href:
            rows.append(f'<li><a href="{_escape_html(href)}">{_escape_html(label)}</a></li>')
        else:
            rows.append(f"<li>{_escape_html(label)}</li>")
    return "<ul>" + "".join(rows) + "</ul>"


def _extract_change_token(item: dict[str, Any]) -> str | None:
    version = item.get("version")
    if isinstance(version, dict):
        when = version.get("when")
        if isinstance(when, str) and when:
            return when
        number = version.get("number")
        if isinstance(number, int):
            return str(number)
    return None


def _safe_filename(raw: str) -> str:
    collapsed = _FILENAME_SANITIZE_RE.sub("-", raw.strip())
    collapsed = collapsed.strip("-.")
    return collapsed or "file"


def _wrap_html_document(title: str, html: str) -> str:
    return (
        "<!doctype html>"
        "<html>"
        "<head>"
        '<meta charset="utf-8"/>'
        f"<title>{_escape_html(title)}</title>"
        "</head>"
        "<body>"
        f"{html}"
        "</body>"
        "</html>"
    )


def _escape_html(value: str) -> str:
    return (
        value.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;").replace('"', "&quot;")
    )
