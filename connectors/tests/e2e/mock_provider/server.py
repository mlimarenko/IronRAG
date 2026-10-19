"""Deterministic, dependency-free OpenAI-compatible server for local E2E tests.

This service deliberately implements only the endpoints exercised by IronRAG.
It never calls an external model and never logs request bodies, which keeps the
full-stack connector test reproducible and prevents fixture content from being
copied into test logs.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import unicodedata
from collections import Counter
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any
from urllib.parse import urlsplit

SERVICE_NAME = "ironrag-e2e-mock-provider"
CHAT_MODEL = "qwen3:0.6b"
EMBEDDING_MODEL = "qwen3-embedding:0.6b"
VECTOR_DIMENSIONS = 32
MAX_REQUEST_BYTES = 8 * 1024 * 1024
GRAPH_JSON = json.dumps(
    {
        "entities": [
            {
                "label": "Initial marker",
                "node_type": "concept",
                "sub_type": "fixture",
                "aliases": [],
                "summary": "Initial marker fixture.",
            }
        ],
        "relations": [],
    },
    ensure_ascii=False,
    separators=(",", ":"),
)


def _normalise_text(value: str) -> str:
    return unicodedata.normalize("NFKC", value).casefold()


def _embedding(value: str) -> list[float]:
    """Return a stable feature-hashed vector with useful lexical similarity."""

    normalised = _normalise_text(value)
    vector = [0.0] * VECTOR_DIMENSIONS

    # Token hashing gives queries and chunks containing the same formal tokens a
    # positive similarity without relying on a language- or domain-specific list.
    frequencies = Counter(re.findall(r"\w+", normalised, flags=re.UNICODE))
    for token, frequency in frequencies.items():
        digest = hashlib.sha256(token.encode("utf-8")).digest()
        weight = 1.0 + math.log(float(frequency))
        for feature in range(4):
            offset = feature * 2
            index = int.from_bytes(digest[offset : offset + 2], "big") % VECTOR_DIMENSIONS
            sign = 1.0 if digest[16 + feature] & 1 else -1.0
            vector[index] += sign * weight

    # A small whole-value fingerprint makes empty and punctuation-only inputs
    # non-zero and breaks otherwise identical token bags deterministically.
    fingerprint = hashlib.sha256(b"ironrag-youtrack-e2e\0" + normalised.encode("utf-8")).digest()
    for index in range(VECTOR_DIMENSIONS):
        vector[index] += ((fingerprint[index] - 127.5) / 127.5) * 0.05

    norm = math.sqrt(sum(component * component for component in vector))
    if norm == 0.0:  # Defensive: the fingerprint above should make this impossible.
        vector[0] = 1.0
        norm = 1.0
    return [round(component / norm, 8) for component in vector]


def _usage_tokens(value: str) -> int:
    return max(1, (len(value.encode("utf-8")) + 3) // 4)


class MockProviderHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "IronRagE2EMock/0.0.1"
    sys_version = ""

    def do_HEAD(self) -> None:
        path = urlsplit(self.path).path
        if path in {"/health", "/v1/health"}:
            self._send_json(HTTPStatus.OK, self._health_payload(), head_only=True)
            return
        self._send_error(HTTPStatus.NOT_FOUND, "endpoint not found", head_only=True)

    def do_GET(self) -> None:
        path = urlsplit(self.path).path
        if path in {"/health", "/v1/health"}:
            self._send_json(HTTPStatus.OK, self._health_payload())
            return
        if path == "/v1/models":
            self._send_json(
                HTTPStatus.OK,
                {
                    "object": "list",
                    "data": [
                        {
                            "id": CHAT_MODEL,
                            "object": "model",
                            "owned_by": SERVICE_NAME,
                        },
                        {
                            "id": EMBEDDING_MODEL,
                            "object": "model",
                            "owned_by": SERVICE_NAME,
                        },
                    ],
                },
            )
            return
        self._send_error(HTTPStatus.NOT_FOUND, "endpoint not found")

    def do_POST(self) -> None:
        path = urlsplit(self.path).path
        if path not in {"/v1/embeddings", "/v1/chat/completions"}:
            self._send_error(HTTPStatus.NOT_FOUND, "endpoint not found")
            return

        payload = self._read_json_object()
        if payload is None:
            return
        if path == "/v1/embeddings":
            self._handle_embeddings(payload)
            return
        self._handle_chat_completions(payload)

    def _read_json_object(self) -> dict[str, Any] | None:
        raw_length = self.headers.get("Content-Length")
        if raw_length is None:
            self.close_connection = True
            self._send_error(HTTPStatus.LENGTH_REQUIRED, "Content-Length is required")
            return None
        try:
            length = int(raw_length)
        except ValueError:
            self.close_connection = True
            self._send_error(HTTPStatus.BAD_REQUEST, "invalid Content-Length")
            return None
        if length < 0:
            self.close_connection = True
            self._send_error(HTTPStatus.BAD_REQUEST, "invalid Content-Length")
            return None
        if length > MAX_REQUEST_BYTES:
            self.close_connection = True
            self._send_error(HTTPStatus.REQUEST_ENTITY_TOO_LARGE, "request body is too large")
            return None

        try:
            payload = json.loads(self.rfile.read(length))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self._send_error(HTTPStatus.BAD_REQUEST, "request body must be valid JSON")
            return None
        if not isinstance(payload, dict):
            self._send_error(HTTPStatus.BAD_REQUEST, "request body must be a JSON object")
            return None
        return payload

    def _handle_embeddings(self, payload: dict[str, Any]) -> None:
        model = payload.get("model")
        inputs = payload.get("input")
        if not isinstance(model, str) or not model.strip():
            self._send_error(HTTPStatus.BAD_REQUEST, "model must be a non-empty string")
            return
        if isinstance(inputs, str):
            values = [inputs]
        elif isinstance(inputs, list) and all(isinstance(value, str) for value in inputs):
            values = inputs
        else:
            self._send_error(HTTPStatus.BAD_REQUEST, "input must be a string or string array")
            return

        self._send_json(
            HTTPStatus.OK,
            {
                "object": "list",
                "data": [
                    {
                        "object": "embedding",
                        "index": index,
                        "embedding": _embedding(value),
                    }
                    for index, value in enumerate(values)
                ],
                "model": model,
                "usage": {
                    "prompt_tokens": sum(_usage_tokens(value) for value in values),
                    "total_tokens": sum(_usage_tokens(value) for value in values),
                },
            },
        )

    def _handle_chat_completions(self, payload: dict[str, Any]) -> None:
        model = payload.get("model")
        messages = payload.get("messages")
        if not isinstance(model, str) or not model.strip():
            self._send_error(HTTPStatus.BAD_REQUEST, "model must be a non-empty string")
            return
        if not isinstance(messages, list) or not all(
            isinstance(message, dict) for message in messages
        ):
            self._send_error(HTTPStatus.BAD_REQUEST, "messages must be an object array")
            return

        prompt_tokens = _usage_tokens(
            json.dumps(messages, ensure_ascii=False, separators=(",", ":"))
        )
        if payload.get("stream") is True:
            self._send_chat_stream(model=model, prompt_tokens=prompt_tokens)
            return

        self._send_json(
            HTTPStatus.OK,
            {
                "id": "chatcmpl-ironrag-e2e",
                "object": "chat.completion",
                "created": 0,
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": GRAPH_JSON},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": 1,
                    "total_tokens": prompt_tokens + 1,
                },
            },
        )

    def _send_chat_stream(self, *, model: str, prompt_tokens: int) -> None:
        chunks = [
            {
                "id": "chatcmpl-ironrag-e2e",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "delta": {"role": "assistant", "content": GRAPH_JSON},
                        "finish_reason": None,
                    }
                ],
            },
            {
                "id": "chatcmpl-ironrag-e2e",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {
                    "prompt_tokens": prompt_tokens,
                    "completion_tokens": 1,
                    "total_tokens": prompt_tokens + 1,
                },
            },
        ]
        body = (
            b"".join(
                b"data: "
                + json.dumps(chunk, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
                + b"\n\n"
                for chunk in chunks
            )
            + b"data: [DONE]\n\n"
        )
        self.send_response(HTTPStatus.OK)
        self.send_header("Content-Type", "text/event-stream; charset=utf-8")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    @staticmethod
    def _health_payload() -> dict[str, str]:
        return {"status": "ok", "service": SERVICE_NAME}

    def _send_error(
        self,
        status: HTTPStatus,
        message: str,
        *,
        head_only: bool = False,
    ) -> None:
        self._send_json(
            status,
            {"error": {"message": message, "type": "invalid_request_error"}},
            head_only=head_only,
        )

    def _send_json(
        self,
        status: HTTPStatus,
        payload: dict[str, Any],
        *,
        head_only: bool = False,
    ) -> None:
        body = json.dumps(payload, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if not head_only:
            self.wfile.write(body)

    def log_message(self, format: str, *args: object) -> None:
        # Do not let request bodies, query strings, or fixture data reach CI logs.
        del format, args


class MockProviderServer(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main() -> None:
    host = os.environ.get("HOST", "0.0.0.0")
    raw_port = os.environ.get("PORT", "8080")
    try:
        port = int(raw_port)
    except ValueError as error:
        raise SystemExit("PORT must be an integer") from error
    if not 1 <= port <= 65535:
        raise SystemExit("PORT must be between 1 and 65535")

    server = MockProviderServer((host, port), MockProviderHandler)
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
