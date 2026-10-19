"""Shared vendor retry backoff and request pacing with injectable clocks."""

from __future__ import annotations

import asyncio
import time
from collections.abc import Callable
from datetime import UTC
from email.utils import parsedate_to_datetime

import httpx


def backoff_delay(attempt: int, *, backoff_seconds: float, max_sleep_seconds: float) -> float:
    """Exponential backoff for `attempt`, capped."""
    return float(min(backoff_seconds * (2**attempt), max_sleep_seconds))


def retry_delay(
    headers: httpx.Headers,
    attempt: int,
    *,
    backoff_seconds: float,
    max_sleep_seconds: float,
    wall_clock: Callable[[], float] = time.time,
) -> float:
    """How long to wait before retrying, honouring `Retry-After` when present.

    `Retry-After` is either a delay in seconds or an HTTP date. An unparseable
    value falls back to exponential backoff rather than failing the request:
    a malformed header from a vendor must not be able to end a sweep.
    """
    retry_after = headers.get("Retry-After")
    if retry_after:
        stripped = retry_after.strip()
        if stripped.isdigit():
            return min(float(stripped), max_sleep_seconds)
        try:
            target = parsedate_to_datetime(stripped)
            if target.tzinfo is None:
                target = target.replace(tzinfo=UTC)
            delta = max(0.0, target.timestamp() - wall_clock())
            return min(delta, max_sleep_seconds)
        except (TypeError, ValueError, OverflowError):
            pass
    return backoff_delay(
        attempt, backoff_seconds=backoff_seconds, max_sleep_seconds=max_sleep_seconds
    )


class RequestThrottle:
    """Spaces request starts apart, without serialising the exchanges.

    The lock is held only across the interval wait. Concurrent callers still
    queue for distinct start times, so the minimum interval between request
    starts is unchanged; once a caller has taken its slot the lock is released
    and the HTTP exchange runs on its own. Holding the lock through the whole
    exchange serialised every request and made sync concurrency useless
    (IRG-125).
    """

    def __init__(
        self,
        min_interval_seconds: float,
        *,
        monotonic: Callable[[], float] = time.monotonic,
        sleep: Callable[[float], object] = asyncio.sleep,
    ) -> None:
        self._min_interval = min_interval_seconds
        self._monotonic = monotonic
        self._sleep = sleep
        self._lock = asyncio.Lock()
        # None rather than 0.0: with a real monotonic clock the difference never
        # showed, because monotonic() is large. It does show the moment the
        # clock is injected, and "no request has been made yet" must not be
        # read as "a request was made at time zero" — that made the very first
        # request of a process wait out an interval it never used.
        self._last_request_at: float | None = None

    async def __aenter__(self) -> RequestThrottle:
        await self._lock.acquire()
        try:
            if self._min_interval > 0 and self._last_request_at is not None:
                wait = self._min_interval - (self._monotonic() - self._last_request_at)
                if wait > 0:
                    await self._sleep(wait)  # type: ignore[misc]
            self._last_request_at = self._monotonic()
        finally:
            self._lock.release()
        return self

    async def __aexit__(self, *_exc: object) -> None:
        """No-op: ``__aenter__`` released the start slot before the exchange."""
        return None
