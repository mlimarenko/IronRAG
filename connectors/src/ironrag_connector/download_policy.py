"""Same-origin policy for downloads made with a vendor's authenticated client.

Every vendor client carries its credentials on the client object itself:
BookStack as a default ``Authorization`` header, Confluence as a header or as
``auth=``. httpx sends both on every request that client makes, whatever host
the URL names; only on a *redirect* that leaves the origin does it drop
``Authorization``. A URL that arrived in source data -- an external attachment
link, an image reference in page markdown, an attachment download link -- would
therefore hand the connector's credentials to whoever controls that URL, and
would let that URL point the connector at any network address the pod reaches.

This module is the one place that decides where such a URL may go: the source's
own origin (scheme, host, port), nowhere else. Every reference is resolved to
the absolute URL the client would actually request *before* anything is sent,
so a client whose ``base_url`` is not the source origin cannot leak either.
Redirects are followed here, hop by hop, under the same check; the client's own
redirect following is switched off for these requests because it checks nothing
but the header.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from dataclasses import dataclass

import httpx

MAX_REDIRECTS = 5

_DEFAULT_PORTS = {"http": 80, "https": 443}

# One GET of an absolute URL, redirects not followed. Each client passes its
# own request method so a download still goes through its throttle and retry.
SendOnce = Callable[[str], Awaitable[httpx.Response]]


class DownloadPolicyError(ValueError):
    """The download cannot proceed under the same-origin policy."""


class CrossOriginDownloadError(DownloadPolicyError):
    """The URL, or a redirect it led to, leaves the source origin."""


@dataclass(frozen=True, slots=True)
class Origin:
    scheme: str
    host: str
    port: int

    @classmethod
    def of(cls, url: str) -> Origin:
        parsed = _parse(url)
        scheme = parsed.scheme.lower()
        if scheme not in _DEFAULT_PORTS:
            raise DownloadPolicyError(f"download URL scheme must be http or https, got {scheme!r}")
        if not parsed.host:
            raise DownloadPolicyError("download URL has no host")
        port = parsed.port if parsed.port is not None else _DEFAULT_PORTS[scheme]
        return cls(scheme, parsed.host.lower(), port)

    def __str__(self) -> str:
        return f"{self.scheme}://{self.host}:{self.port}"


def _parse(url: str) -> httpx.URL:
    try:
        return httpx.URL(url)
    except httpx.InvalidURL as exc:
        raise DownloadPolicyError(f"download URL is malformed: {exc}") from exc


def require_same_origin(origin: Origin, url: str) -> None:
    target = Origin.of(url)
    if target != origin:
        raise CrossOriginDownloadError(
            f"download target on {target} is outside the source origin {origin}"
        )


def _reject_ambiguous(reference: str) -> None:
    # Neither form has a use in connector data, and both make the request that
    # httpx will actually send hard to reason about: an authority-relative
    # reference ("//host/x") is silently turned into a path on the client's
    # host, and userinfo in the URL replaces the client's Authorization header
    # with attacker-chosen Basic credentials.
    if reference.lstrip().startswith(("//", "\\\\")):
        raise DownloadPolicyError("authority-relative download URL is not allowed")
    if _parse(reference).userinfo:
        raise DownloadPolicyError("download URL with userinfo is not allowed")


def _resolve(client: httpx.AsyncClient, origin: Origin, reference: str) -> str:
    """The absolute URL ``client`` would request for ``reference``, if it stays on ``origin``."""
    _reject_ambiguous(reference)
    try:
        absolute = str(client.build_request("GET", reference).url)
    except httpx.InvalidURL as exc:
        raise DownloadPolicyError(f"download URL is malformed: {exc}") from exc
    _reject_ambiguous(absolute)
    require_same_origin(origin, absolute)
    return absolute


async def fetch_within_origin(
    client: httpx.AsyncClient,
    base_url: str,
    path_or_url: str,
    *,
    send: SendOnce,
    max_redirects: int = MAX_REDIRECTS,
) -> httpx.Response:
    """GET ``path_or_url`` with ``send``, refusing to leave ``base_url``'s origin.

    ``path_or_url`` is resolved with ``client`` (its ``base_url`` merging) to
    the absolute URL that would go on the wire, and that URL is checked before
    ``send`` is called with it. After every response the URL actually requested
    is checked again, and each redirect hop is resolved and checked the same
    way before it is followed. ``send`` must not follow redirects itself.
    """
    origin = Origin.of(base_url)
    url = _resolve(client, origin, path_or_url)
    for _ in range(max_redirects + 1):
        response = await send(url)
        require_same_origin(origin, str(response.request.url))
        if not response.has_redirect_location:
            return response
        # httpx has already canonicalised Location into next_request; the raw
        # header is what decides whether the reference was ambiguous.
        _reject_ambiguous(response.headers["location"])
        next_request = response.next_request
        if next_request is None:
            raise DownloadPolicyError("redirect response carries no next request")
        url = _resolve(client, origin, str(next_request.url))
    raise DownloadPolicyError(f"more than {max_redirects} redirects while downloading")
