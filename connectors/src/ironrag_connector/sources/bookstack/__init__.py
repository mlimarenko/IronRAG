"""BookStack connector built on the ironrag-connector framework.

Only this package owns BookStack vocabulary (pages, books, shelves,
attachments, image URLs in markdown). Everything else — IronRAG transport,
routing, policy, sync loop, state cursor, FastAPI server, pidfile,
observability — is reused from ``ironrag_connector``.
"""

from __future__ import annotations

from ...registry import SourcePlugin
from ...routing import PolicyOverrides
from .adapter import BookStackAdapter
from .config import BookStackSettings
from .webhook import make_bookstack_handler


def build_bookstack_adapter(
    settings: BookStackSettings,
    _policies: PolicyOverrides,
) -> BookStackAdapter:
    """The registry factory; this source reads no per-kind policy."""
    return BookStackAdapter(settings)


PLUGIN = SourcePlugin(
    "bookstack",
    BookStackSettings,
    build_bookstack_adapter,
    make_bookstack_handler,
)

__all__ = ["PLUGIN", "BookStackAdapter", "BookStackSettings"]
