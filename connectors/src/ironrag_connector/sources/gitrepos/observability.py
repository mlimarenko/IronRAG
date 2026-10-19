"""Re-export the framework's logger so the adapter package has a single
canonical entry point. Keeping this thin avoids duplicate structlog
setup if downstream extensions ever swap the logger backend."""

from __future__ import annotations

from ironrag_connector.observability import get_logger

__all__ = ["get_logger"]
