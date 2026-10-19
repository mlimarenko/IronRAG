"""Confluence connector package."""

from __future__ import annotations

from ...registry import SourcePlugin
from ...routing import PolicyOverrides
from .adapter import ConfluenceAdapter
from .config import ConfluenceSettings
from .webhook import make_confluence_handler


def build_confluence_adapter(
    settings: ConfluenceSettings,
    _policies: PolicyOverrides,
) -> ConfluenceAdapter:
    """The registry factory; this source reads no per-kind policy."""
    return ConfluenceAdapter(settings)


PLUGIN = SourcePlugin(
    "confluence",
    ConfluenceSettings,
    build_confluence_adapter,
    make_confluence_handler,
)

__all__ = ["PLUGIN", "ConfluenceAdapter", "ConfluenceSettings"]
