"""Collect BookStack-hosted image URLs from page markdown.

BookStack pages exported as markdown contain inline images like:

    ![alt](http://wiki.example.com/uploads/images/gallery/2023-06/foo.png)

This module collects those URLs so that callers can upload each referenced
image as a separate IronRAG document (instead of inlining base64 data-URIs,
which caused multi-MB markdown documents that timed out graph extraction).

Rules:
- Only collect URLs whose host matches the BookStack base URL.
- Leave external image URLs (other hosts) unchanged and uncollected.
"""

from __future__ import annotations

import re
from urllib.parse import urlparse

from ironrag_connector.observability import get_logger

log = get_logger(__name__)

_IMG_RE = re.compile(r"!\[([^\]]*)\]\(([^)]+)\)")


async def collect_bookstack_image_urls(
    markdown: str,
    bookstack_base_url: str,
) -> list[str]:
    """Return unique BookStack-hosted image URLs found in *markdown*.

    The markdown itself is left completely untouched — this function is
    purely a collector. Callers are responsible for downloading the images
    and uploading them as separate IronRAG documents.

    Args:
        markdown: Raw markdown text.
        bookstack_base_url: The BookStack instance base (e.g. ``http://wiki.example.com``).

    Returns:
        Ordered list of unique BookStack image URLs (first-seen order, no duplicates).
    """
    base_host = urlparse(bookstack_base_url.rstrip("/")).netloc

    seen: set[str] = set()
    result: list[str] = []
    for _alt, url in _IMG_RE.findall(markdown):
        if url in seen:
            continue
        parsed = urlparse(url)
        if parsed.netloc != base_host:
            # External host — skip.
            continue
        seen.add(url)
        result.append(url)

    return result
