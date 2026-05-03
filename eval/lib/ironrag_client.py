"""HTTP client for IronRAG grounded-answer endpoint used by the eval harness.

Per ADR-001 the eval harness reuses the QueryAnswer binding. The client treats the
backend as a black box: send query, receive answer + retrieved contexts + citations.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Any

import httpx
from tenacity import retry, stop_after_attempt, wait_exponential


@dataclass(frozen=True)
class GroundedAnswer:
    answer_text: str
    citation_chunk_ids: list[str]
    retrieved_contexts: list[str]
    runtime_execution_id: str
    verifier_verdict: str | None
    raw_response: dict[str, Any]


class IronRagClient:
    def __init__(
        self,
        base_url: str | None = None,
        token: str | None = None,
        timeout_s: float = 60.0,
    ) -> None:
        self._base = (base_url or os.environ["IRONRAG_EVAL_BASE_URL"]).rstrip("/")
        self._token = token or os.environ["IRONRAG_EVAL_TOKEN"]
        self._timeout = timeout_s

    @retry(stop=stop_after_attempt(3), wait=wait_exponential(min=1, max=8))
    def grounded_answer(self, library_id: str, question: str) -> GroundedAnswer:
        with httpx.Client(timeout=self._timeout) as client:
            session = client.post(
                f"{self._base}/v1/query/sessions",
                headers=self._auth(),
                json={"libraryId": library_id, "requestSurface": "eval"},
            )
            session.raise_for_status()
            session_id = session.json()["id"]

            turn = client.post(
                f"{self._base}/v1/query/sessions/{session_id}/turns",
                headers=self._auth(),
                json={"question": question, "stream": False},
            )
            turn.raise_for_status()
            payload = turn.json()

        chunk_ids = [c["chunkId"] for c in payload.get("citations", [])]
        contexts = [c.get("contextText", "") for c in payload.get("retrievedChunks", [])]
        return GroundedAnswer(
            answer_text=payload["answer"]["text"],
            citation_chunk_ids=chunk_ids,
            retrieved_contexts=contexts,
            runtime_execution_id=payload["runtimeExecutionId"],
            verifier_verdict=payload.get("verifier", {}).get("verdict"),
            raw_response=payload,
        )

    def _auth(self) -> dict[str, str]:
        return {"Authorization": f"Bearer {self._token}"}
