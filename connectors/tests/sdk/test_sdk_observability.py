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
