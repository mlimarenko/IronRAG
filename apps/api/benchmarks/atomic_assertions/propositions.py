"""Decode first proposition outputs without repairing, grading or trusting them."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

FIELDS = {"subject", "assertion", "negation", "conditions", "time", "units", "scope"}


def parse_claims(text: str, reached_token_cap: bool) -> dict:
    if reached_token_cap:
        return {"state": "output_cap", "claims": []}
    try:
        data = json.loads(text)
    except json.JSONDecodeError:
        return {"state": "invalid_json", "claims": []}
    if not isinstance(data, dict) or set(data) != {"claims"}:
        return {"state": "invalid_envelope", "claims": []}
    claims = data["claims"]
    if not isinstance(claims, list) or len(claims) > 16:
        return {"state": "invalid_claim_population", "claims": []}
    for claim in claims:
        if not isinstance(claim, dict) or set(claim) != FIELDS:
            return {"state": "invalid_claim_fields", "claims": []}
        if any(
            not isinstance(value, str) or len(value) > 4096 for value in claim.values()
        ):
            return {"state": "invalid_claim_value", "claims": []}
        if not claim["subject"].strip() or not claim["assertion"].strip():
            return {"state": "missing_subject_or_assertion", "claims": []}
    return {"state": "parsed" if claims else "empty", "claims": claims}


def pinned(path: Path, expected: str) -> dict:
    if path.stat().st_size > 32 * 1024 * 1024:
        raise ValueError("proposition artifact exceeds input bound")
    raw = path.read_bytes()
    if hashlib.sha256(raw).hexdigest() != expected:
        raise ValueError("proposition artifact identity mismatch")
    return json.loads(raw)


def run(args: argparse.Namespace) -> None:
    report = pinned(args.report, args.report_sha256)
    oracle = pinned(args.units, args.units_sha256)
    if report["protocol"] != "irg256-local-answers/1":
        raise ValueError("unsupported local generation report")
    if oracle["protocol"] != "irg257-proposition-unit-oracle/1":
        raise ValueError("unsupported source-unit oracle")
    if report["inputSha256"] != oracle["modelInputSha256"]:
        raise ValueError("model and source-unit inputs differ")
    units = {unit["id"]: unit for unit in oracle["units"]}
    ids = [row["id"] for row in report["cases"]]
    if len(ids) != len(set(ids)) or set(ids) != set(units):
        raise ValueError(
            "first proposition response population differs from source units"
        )
    rows = []
    for response in report["cases"]:
        unit = units[response["id"]]
        if response["armId"] != "atomic-propositions":
            raise ValueError("unexpected generation purpose")
        parsed = parse_claims(response["answer"], response["outputReachedTokenCap"])
        rows.append(
            {
                "unitId": unit["id"],
                "caseId": unit["caseId"],
                "normalizedSha256": unit["normalizedSha256"],
                "sourceSpans": unit["sourceSpans"],
                "supportBlockIds": unit["supportBlockIds"],
                "expectedFactIds": unit["expectedFactIds"],
                **parsed,
                "rawResponse": response["answer"],
                "inputTokens": response["inputTokens"],
                "outputTokens": response["outputTokens"],
                "elapsedMs": response["elapsedMs"],
                "conditionFidelityAssessed": False,
            }
        )
    result = {
        "protocol": "irg257-first-proposition-candidates/1",
        "modelReportSha256": args.report_sha256,
        "unitOracleSha256": args.units_sha256,
        "nativeInputSha256": oracle["nativeInputSha256"],
        "units": rows,
        "claimCount": sum(len(row["claims"]) for row in rows),
        "modelRepairOrRetry": False,
        "sourceReplaced": False,
        "scope": "Parsed experimental candidates only; source linkage is not a truth or condition-preservation verdict",
    }
    with args.output.open("x", encoding="utf-8") as stream:
        json.dump(result, stream, ensure_ascii=False, indent=2, allow_nan=False)
        stream.write("\n")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("report", "units"):
        parser.add_argument(f"--{name}", required=True, type=Path)
        parser.add_argument(f"--{name}-sha256", required=True)
    parser.add_argument("--output", required=True, type=Path)
    run(parser.parse_args())


if __name__ == "__main__":
    main()
