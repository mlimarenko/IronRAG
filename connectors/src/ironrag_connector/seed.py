"""One-shot cursor seeder.

When you start a connector against an IronRAG library that already
contains documents owned by the same adapter (for example after a
manual bulk upload, or when migrating from another connector), the
SQLite cursor at ``STATE_DB_PATH`` is empty. The first sweep would
then attempt to ``create`` every existing item and trip IronRAG's
``(library_id, external_key)`` unique constraint with a 409.

This module walks every IronRAG document under each routing-target
library via the same :meth:`IronRagClient.list_documents` the rest of
the framework uses (the list response already carries ``externalKey``
per document -- no per-document detail round trip needed), asks the
adapter to parse each key back into ``(kind, item_id)``, and writes a
cursor row for each match. After it finishes, the periodic sweep can
short-circuit known items via the cursor.

Connectors expose this as a ``__main__``-compatible entry point:

    from ironrag_connector import seed_cursor

    def main() -> None:
        settings = MyConnectorSettings()
        adapter = MyAdapter(settings)
        seed_cursor(settings, adapter)

then run once with ``uv run python -m my_connector.seed`` (or whatever
script entry the connector ships).
"""

from __future__ import annotations

import asyncio

from .config import BaseConnectorSettings
from .ironrag import DocumentResource, IronRagClient, IronRagError
from .observability import configure_logging, get_logger
from .routing import Router, load_routing_config
from .source import SourceAdapter
from .state import StateStore

log = get_logger(__name__)


def _owned_document_identity(
    adapter: SourceAdapter,
    document: DocumentResource,
) -> tuple[str, str] | None:
    parsed = adapter.parse_external_key(document.external_key)
    if parsed is None:
        return None
    kind, item_id = parsed
    if kind not in adapter.kinds:
        log.warning(
            "seed.document.unowned_kind",
            external_key=document.external_key,
            parsed_kind=kind,
        )
        return None
    try:
        canonical_external_key = adapter.external_key(kind, item_id)
    except ValueError as exc:
        log.warning(
            "seed.document.invalid_identity",
            external_key=document.external_key,
            parsed_kind=kind,
            error=str(exc),
        )
        return None
    if canonical_external_key != document.external_key:
        log.warning(
            "seed.document.noncanonical_key",
            external_key=document.external_key,
            canonical_external_key=canonical_external_key,
        )
        return None
    return kind, item_id


async def _seed_async(settings: BaseConnectorSettings, adapter: SourceAdapter) -> dict[str, int]:
    router = Router(load_routing_config(settings.routing_config_path))
    state = StateStore(settings.state_db_path)
    counts: dict[str, int] = {"libraries": 0, "docs_scanned": 0, "rows_inserted": 0}

    ironrag = IronRagClient(settings)
    try:
        await router.initialize(ironrag.resolve_library_refs)
        for library_id in router.target_libraries():
            counts["libraries"] += 1
            log.info("seed.library.start", library_id=str(library_id))
            async for document in ironrag.list_documents(library_id):
                counts["docs_scanned"] += 1
                if document.library_id != library_id:
                    raise IronRagError(
                        "seed document library identity did not match requested target: "
                        f"requested={library_id}, returned={document.library_id}"
                    )
                identity = _owned_document_identity(adapter, document)
                if identity is None:
                    continue
                kind, item_id = identity
                state.upsert(
                    kind=kind,
                    item_id=item_id,
                    change_token=None,
                    external_key=document.external_key,
                    ironrag_document_id=str(document.id),
                    ironrag_library_id=str(library_id),
                )
                counts["rows_inserted"] += 1
            log.info(
                "seed.library.done",
                library_id=str(library_id),
                rows_inserted=counts["rows_inserted"],
            )
    finally:
        await ironrag.aclose()
        state.close()
    return counts


def seed_cursor(settings: BaseConnectorSettings, adapter: SourceAdapter) -> dict[str, int]:
    """Synchronous wrapper for use from a connector's ``main`` script.

    Returns a counts dict: ``libraries``, ``docs_scanned``,
    ``rows_inserted``. The cursor's ``change_token`` is left ``None`` for
    every seeded row -- the next sweep will pick up the live token from
    the source and decide whether to replace.
    """
    configure_logging(settings.log_level)
    return asyncio.run(_seed_async(settings, adapter))
