"""The connector log record contract: identity, redaction and fingerprint."""

from __future__ import annotations

from typing import Any

import structlog

from ironrag_connector.observability import (
    add_error_fingerprint,
    add_service_identity,
    configure_logging,
    redact_event,
)
from ironrag_connector.redaction import fingerprint
from ironrag_connector.version import connector_version


def test_redaction_processor_and_service_binding() -> None:
    configure_logging("info")
    context = structlog.contextvars.get_contextvars()
    assert context["service"] == "ironrag-connector"
    assert context["release"] == connector_version()

    event: Any = {
        "event": "sync.item.failed",
        "endpoint": "https://user:secret@example.test/hook",
        "exc_info": (RuntimeError, RuntimeError("upstream rejected token=hunter2"), None),
    }
    event = add_service_identity(None, "error", event)
    event = add_error_fingerprint(None, "error", event)
    event = structlog.processors.format_exc_info(None, "error", event)
    event = redact_event(None, "error", event)

    assert event["service"] == "ironrag-connector"
    assert event["release"] == connector_version()
    assert "[REDACTED:token]" in event["exception"]
    assert "hunter2" not in event["exception"]
    assert "[REDACTED:credentials]" in event["endpoint"]
    assert "secret" not in event["endpoint"]
    assert len(event["error_fingerprint"]) == 16
    assert fingerprint("RuntimeError", "failed after 3 attempts") == fingerprint(
        "RuntimeError", "failed after 7 attempts"
    )


def test_error_fingerprint_reads_the_active_exception() -> None:
    """``exc_info=True`` means the exception being handled right now."""

    try:
        raise RuntimeError("upstream rejected the request")
    except RuntimeError:
        event = add_error_fingerprint(None, "error", {"event": "x", "exc_info": True})

    assert event["error_fingerprint"] == fingerprint(
        "RuntimeError", "upstream rejected the request"
    )


def test_error_fingerprint_accepts_a_bare_exception_value() -> None:
    event = add_error_fingerprint(
        None, "error", {"event": "x", "exc_info": PermissionError("denied")}
    )

    assert event["error_fingerprint"] == fingerprint("PermissionError", "denied")


def test_error_fingerprint_falls_back_outside_an_exception_scope() -> None:
    """``exc_info=True`` with no active exception still yields one stable key."""

    event = add_error_fingerprint(None, "error", {"event": "x", "exc_info": True})
    opaque = add_error_fingerprint(None, "error", {"event": "x", "exc_info": {"detail": 1}})

    assert event["error_fingerprint"] == fingerprint("unknown", "unknown")
    assert opaque["error_fingerprint"] == fingerprint("exception", "unknown")


def test_redaction_walks_tuple_values() -> None:
    """A tuple is traversed like a list, so nested strings are still scrubbed."""

    event = redact_event(
        None,
        "error",
        {"event": "x", "pair": ("token=abc123", "plain")},
    )

    assert event["pair"] == ("[REDACTED:token]", "plain")
