"""Retry backoff and the request throttle, on a clock the test controls."""

from __future__ import annotations

import asyncio

import httpx
import pytest

from ironrag_connector.http_retry import RequestThrottle, backoff_delay, retry_delay


def test_backoff_doubles_and_is_capped() -> None:
    delays = [
        backoff_delay(attempt, backoff_seconds=1.0, max_sleep_seconds=8.0) for attempt in range(6)
    ]
    assert delays == [1.0, 2.0, 4.0, 8.0, 8.0, 8.0]


def test_retry_after_seconds_wins_over_backoff() -> None:
    headers = httpx.Headers({"Retry-After": "3"})
    assert retry_delay(headers, 5, backoff_seconds=1.0, max_sleep_seconds=60.0) == 3.0


def test_retry_after_is_capped_like_backoff() -> None:
    headers = httpx.Headers({"Retry-After": "3600"})
    assert retry_delay(headers, 0, backoff_seconds=1.0, max_sleep_seconds=30.0) == 30.0


def test_retry_after_http_date_is_measured_against_the_injected_clock() -> None:
    headers = httpx.Headers({"Retry-After": "Wed, 21 Oct 2026 07:28:10 GMT"})
    target = 1792567690.0  # the same instant, as a POSIX timestamp
    delay = retry_delay(
        headers,
        0,
        backoff_seconds=1.0,
        max_sleep_seconds=600.0,
        wall_clock=lambda: target - 42.0,
    )
    assert delay == pytest.approx(42.0)


def test_a_date_already_past_does_not_go_negative() -> None:
    headers = httpx.Headers({"Retry-After": "Wed, 21 Oct 2026 07:28:10 GMT"})
    delay = retry_delay(
        headers,
        0,
        backoff_seconds=1.0,
        max_sleep_seconds=600.0,
        wall_clock=lambda: 1792567690.0 + 5.0,
    )
    assert delay == 0.0


def test_unparseable_retry_after_falls_back_to_backoff() -> None:
    headers = httpx.Headers({"Retry-After": "soon-ish"})
    assert retry_delay(headers, 2, backoff_seconds=1.0, max_sleep_seconds=60.0) == 4.0


class FakeClock:
    """A monotonic clock that only moves when a sleep is awaited."""

    def __init__(self) -> None:
        self.now = 0.0
        self.slept: list[float] = []

    def monotonic(self) -> float:
        return self.now

    async def sleep(self, seconds: float) -> None:
        self.slept.append(seconds)
        self.now += seconds


async def test_throttle_waits_out_the_remaining_interval() -> None:
    clock = FakeClock()
    throttle = RequestThrottle(2.0, monotonic=clock.monotonic, sleep=clock.sleep)

    async with throttle:
        pass
    clock.now += 0.5
    async with throttle:
        pass

    assert clock.slept == [pytest.approx(1.5)]


async def test_throttle_does_not_sleep_when_the_interval_has_passed() -> None:
    clock = FakeClock()
    throttle = RequestThrottle(1.0, monotonic=clock.monotonic, sleep=clock.sleep)

    async with throttle:
        pass
    clock.now += 10.0
    async with throttle:
        pass

    assert clock.slept == []


async def test_zero_interval_never_sleeps() -> None:
    clock = FakeClock()
    throttle = RequestThrottle(0.0, monotonic=clock.monotonic, sleep=clock.sleep)
    for _ in range(3):
        async with throttle:
            pass
    assert clock.slept == []


async def test_concurrent_callers_get_staggered_start_slots() -> None:
    clock = FakeClock()
    throttle = RequestThrottle(1.0, monotonic=clock.monotonic, sleep=clock.sleep)
    order: list[str] = []

    async def call(name: str) -> None:
        async with throttle:
            order.append(name)

    await asyncio.gather(call("a"), call("b"), call("c"))

    assert sorted(order) == ["a", "b", "c"]
    # The first caller does not wait; the two behind it each wait out the interval.
    assert len(clock.slept) == 2


async def test_throttle_spaces_starts_and_keeps_the_minimum_interval() -> None:
    clock = FakeClock()
    throttle = RequestThrottle(1.0, monotonic=clock.monotonic, sleep=clock.sleep)
    starts: list[float] = []

    async def call() -> None:
        async with throttle:
            starts.append(clock.monotonic())

    await asyncio.gather(call(), call(), call())

    assert starts == [pytest.approx(0.0), pytest.approx(1.0), pytest.approx(2.0)]


async def test_lock_is_not_held_across_the_caller_body() -> None:
    """A second caller enters while the first body is still open.

    On the serialising implementation the second caller blocks on the throttle
    lock until the first body exits, so ``second_inside`` is never set.
    """
    clock = FakeClock()
    throttle = RequestThrottle(1.0, monotonic=clock.monotonic, sleep=clock.sleep)
    first_inside = asyncio.Event()
    release_first = asyncio.Event()
    second_inside = asyncio.Event()

    async def first() -> None:
        async with throttle:
            first_inside.set()
            await release_first.wait()

    async def second() -> None:
        await first_inside.wait()
        async with throttle:
            second_inside.set()

    first_task = asyncio.create_task(first())
    second_task = asyncio.create_task(second())
    try:
        await asyncio.wait_for(second_inside.wait(), timeout=5.0)
    finally:
        release_first.set()
        await asyncio.gather(first_task, second_task, return_exceptions=True)

    assert clock.slept == [pytest.approx(1.0)]


async def test_cancelling_a_caller_waiting_for_its_slot_releases_the_lock() -> None:
    """A cancelled interval wait must not leave the throttle lock held.

    The slot is released in ``__aenter__``'s ``finally``. If cancellation
    skipped that release, every later caller would deadlock.
    """
    clock = FakeClock()
    sleeps = 0
    waiting = asyncio.Event()

    async def blocking_sleep(seconds: float) -> None:
        nonlocal sleeps
        sleeps += 1
        if sleeps == 1:
            waiting.set()
            await asyncio.Event().wait()  # released only by cancellation
        clock.now += seconds

    throttle = RequestThrottle(1.0, monotonic=clock.monotonic, sleep=blocking_sleep)

    async with throttle:
        pass  # first caller takes its slot without waiting

    async def waiter() -> None:
        async with throttle:
            pass

    pending = asyncio.create_task(waiter())
    await waiting.wait()
    pending.cancel()
    with pytest.raises(asyncio.CancelledError):
        await pending

    # The cancelled wait released the lock, so the next caller still gets a slot.
    await asyncio.wait_for(waiter(), timeout=5.0)
    assert sleeps == 2
