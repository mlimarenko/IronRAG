"""Artifact-coverage gate.

Sprint A acceptance per ralplan v4: each benchmark question expects 2-of-3 reference
artifacts (concrete entities like banks, parameters, INI section names, file paths) to
appear in the generated answer AND the matching chunk to appear in citations. Ragas
faithfulness alone does not catch missing artifact regressions.
"""

from __future__ import annotations

from dataclasses import dataclass


@dataclass(frozen=True)
class ArtifactCheck:
    sample_id: str
    artifacts_in_answer: int
    artifacts_total: int
    cited_chunks_present: int
    cited_chunks_required: int
    passed: bool


def check_sample(
    sample_id: str,
    answer_text: str,
    citation_chunk_ids: list[str],
    expected_artifacts: list[str],
    reference_chunk_ids: list[str],
    artifact_min_match: int = 2,
) -> ArtifactCheck:
    answer_lc = answer_text.lower()
    matched = sum(1 for a in expected_artifacts if a.lower() in answer_lc)
    citation_set = set(citation_chunk_ids)
    cited = sum(1 for c in reference_chunk_ids if c in citation_set)
    artifact_pass = matched >= min(artifact_min_match, len(expected_artifacts))
    citation_pass = cited >= 1 if reference_chunk_ids else True
    return ArtifactCheck(
        sample_id=sample_id,
        artifacts_in_answer=matched,
        artifacts_total=len(expected_artifacts),
        cited_chunks_present=cited,
        cited_chunks_required=len(reference_chunk_ids),
        passed=artifact_pass and citation_pass,
    )


def coverage_ratio(checks: list[ArtifactCheck]) -> float:
    if not checks:
        return 0.0
    return sum(1 for c in checks if c.passed) / len(checks)
