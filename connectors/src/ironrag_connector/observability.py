"""Structured logging setup.

Every record carries the service identity and the release, a correlation id is
bound per sync item by :mod:`ironrag_connector.sync`, secrets are scrubbed from
every string value, and an exception keeps its formatted traceback plus a
stable ``error_fingerprint`` an operator can group on.
"""

from __future__ import annotations

import logging
import sys
from collections.abc import Mapping, MutableMapping, Sequence
from typing import Any, cast

import structlog

from .redaction import fingerprint as error_fingerprint
from .redaction import scrub
from .version import connector_version

SERVICE_NAME = "ironrag-connector"

EventDict = MutableMapping[str, Any]


def add_service_identity(
    _logger: Any,
    _method_name: str,
    event_dict: EventDict,
) -> EventDict:
    """Bind the service name and the running release to every record."""
    event_dict.setdefault("service", SERVICE_NAME)
    event_dict.setdefault("release", connector_version())
    return event_dict


def add_error_fingerprint(
    _logger: Any,
    _method_name: str,
    event_dict: EventDict,
) -> EventDict:
    """Add a stable fingerprint before ``format_exc_info`` consumes ``exc_info``."""
    exc_info = event_dict.get("exc_info")
    if exc_info:
        event_dict.setdefault("error_fingerprint", _fingerprint_for(exc_info))
    return event_dict


def redact_event(
    _logger: Any,
    _method_name: str,
    event_dict: EventDict,
) -> EventDict:
    """Scrub every string value, including the formatted exception."""
    return cast(EventDict, _redact_value(event_dict))


def _fingerprint_for(exc_info: Any) -> str:
    if exc_info is True:
        exc_info = sys.exc_info()
    if isinstance(exc_info, BaseException):
        exc = exc_info
    elif isinstance(exc_info, Sequence) and len(exc_info) > 1:
        exc = exc_info[1]
    else:
        return error_fingerprint("exception", "unknown")
    kind = type(exc).__name__ if exc is not None else "unknown"
    message = str(exc) if exc is not None else "unknown"
    return error_fingerprint(kind, message)


def _redact_value(value: Any) -> Any:
    if isinstance(value, str):
        return scrub(value)
    if isinstance(value, Mapping):
        return {key: _redact_value(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_redact_value(item) for item in value]
    if isinstance(value, tuple):
        return tuple(_redact_value(item) for item in value)
    return value


def configure_logging(level: str = "info") -> None:
    log_level = getattr(logging, level.upper(), logging.INFO)
    logging.basicConfig(format="%(message)s", stream=sys.stderr, level=log_level)
    # Context variables are merged into every event by the first processor;
    # binding the identity here keeps it on records emitted from any task.
    structlog.contextvars.bind_contextvars(
        service=SERVICE_NAME,
        release=connector_version(),
    )
    structlog.configure(
        processors=[
            structlog.contextvars.merge_contextvars,
            structlog.processors.add_log_level,
            structlog.processors.TimeStamper(fmt="iso", utc=True),
            add_service_identity,
            structlog.processors.StackInfoRenderer(),
            add_error_fingerprint,
            structlog.processors.format_exc_info,
            redact_event,
            structlog.processors.JSONRenderer(),
        ],
        wrapper_class=structlog.make_filtering_bound_logger(log_level),
        logger_factory=structlog.PrintLoggerFactory(),
        cache_logger_on_first_use=True,
    )


def get_logger(name: str) -> structlog.stdlib.BoundLogger:
    return cast(structlog.stdlib.BoundLogger, structlog.get_logger(name))
