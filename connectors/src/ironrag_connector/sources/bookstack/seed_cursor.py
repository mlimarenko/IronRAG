"""Thin wrapper over the framework's generic seed_cursor.

The framework owns the IronRAG document-list walk + cursor write (the
list response already carries each document's external key); we only
hand it our adapter (which knows how to parse the external_key back
into `(kind, item_id)`).
"""

from __future__ import annotations

from ironrag_connector import seed_cursor

from .adapter import BookStackAdapter
from .config import BookStackSettings


def main() -> None:
    settings = BookStackSettings()
    adapter = BookStackAdapter(settings)
    counts = seed_cursor(settings, adapter)
    print(counts)


if __name__ == "__main__":
    main()
