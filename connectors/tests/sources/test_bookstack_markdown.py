"""Unit tests for markdown.collect_bookstack_image_urls."""

from __future__ import annotations

from ironrag_connector.sources.bookstack.markdown import collect_bookstack_image_urls

BASE = "http://wiki.example.com"


class TestCollectBookstackImageUrls:
    async def test_collects_bookstack_url(self):
        """An image URL on the BookStack host is collected."""
        url = f"{BASE}/uploads/images/gallery/2023/foo.png"
        md = f"![alt]({url})"

        result = await collect_bookstack_image_urls(md, BASE)

        assert result == [url]

    async def test_ignores_external_url(self):
        """Image URLs on a different host are not collected."""
        external_url = "https://other.com/logo.png"
        md = f"![logo]({external_url})"

        result = await collect_bookstack_image_urls(md, BASE)

        assert result == []

    async def test_deduplicates_identical_urls(self):
        """The same URL appearing twice is only returned once."""
        url = f"{BASE}/uploads/images/gallery/x.png"
        md = f"![a]({url})\n\n![b]({url})"

        result = await collect_bookstack_image_urls(md, BASE)

        assert result == [url]

    async def test_mixed_internal_and_external(self):
        """Internal URL is collected; external URL is not."""
        internal_url = f"{BASE}/uploads/images/gallery/internal.png"
        external_url = "https://cdn.example.com/external.png"
        md = f"![i]({internal_url}) ![e]({external_url})"

        result = await collect_bookstack_image_urls(md, BASE)

        assert result == [internal_url]
        assert external_url not in result

    async def test_preserves_first_seen_order(self):
        """Multiple distinct internal URLs are returned in first-seen order."""
        url1 = f"{BASE}/uploads/images/gallery/a.png"
        url2 = f"{BASE}/uploads/images/gallery/b.png"
        url3 = f"{BASE}/uploads/images/gallery/c.png"
        md = f"![1]({url1})\n![2]({url2})\n![3]({url3})\n![dup]({url1})"

        result = await collect_bookstack_image_urls(md, BASE)

        assert result == [url1, url2, url3]

    async def test_empty_markdown_returns_empty(self):
        """No images → empty list."""
        result = await collect_bookstack_image_urls("# Heading\nNo images here.", BASE)
        assert result == []

    async def test_no_images_at_all(self):
        """Plain text with no markdown image syntax → empty list."""
        result = await collect_bookstack_image_urls("Just some text.", BASE)
        assert result == []

    async def test_base_url_with_trailing_slash(self):
        """Base URL with trailing slash is normalised correctly."""
        url = f"{BASE}/uploads/images/gallery/test.png"
        md = f"![x]({url})"

        result = await collect_bookstack_image_urls(md, BASE + "/")

        assert result == [url]

    async def test_does_not_mutate_markdown(self):
        """The original markdown string is returned unchanged (pure function)."""
        url = f"{BASE}/uploads/images/gallery/foo.png"
        md = f"![alt]({url})"

        await collect_bookstack_image_urls(md, BASE)

        # md must be unchanged
        assert md == f"![alt]({url})"
