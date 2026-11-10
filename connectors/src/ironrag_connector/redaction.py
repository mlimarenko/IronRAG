"""Secret and credential redaction shared by every connector log record.

A connector error message can quote an upstream response body, a webhook URL
with embedded credentials, or an operator-supplied path. The processor in
:mod:`ironrag_connector.observability` routes every string value through
:func:`scrub` before the JSON renderer sees it, so one rule set governs the
whole connector instead of each adapter growing its own.
"""

from __future__ import annotations

import hashlib
import re

#: Hard bound on one scrubbed value; a diagnostic dump is not an archive.
MAX_SCRUBBED_CHARS = 4096

TRUNCATION_MARKER = "[TRUNCATED]"

# ``scheme://user:password@host``: the userinfo segment is a credential even
# when it does not look like one.
_URL_CREDENTIALS = re.compile(r"(?i)\b([a-z][a-z0-9+.\-]*://)[^/@\s]+@")
_BEARER = re.compile(
    r"(?i:\bBearer\s+)(?=[A-Za-z0-9._~+/=\-]*[A-Z0-9._~+/=\-])[A-Za-z0-9._~+/=\-]{8,}"
)
_BASIC = re.compile(r"(?i:\bBasic\s+)(?=[A-Za-z0-9+/=]*[A-Z0-9+/=])[A-Za-z0-9+/=]{8,}")
_PROVIDER_KEY = re.compile(r"\bsk-[A-Za-z0-9_\-]{8,}")
_SECRET_ASSIGNMENT = re.compile(
    r"(?i)\b(password|passwd|secret|client_secret|token|access_token|refresh_token"
    r"|api_key|apikey|access_key)\b\s*[=:]\s*(\"[^\"]*\"|'[^']*'|[^\s,;]+)"
)
_EMAIL = re.compile(r"(?i)\b[A-Z0-9._%+\-]+@[A-Z0-9.\-]+\.[A-Z]{2,}\b")

# Volatile parts of a message that must not split one fault class into many.
_UUID = re.compile(r"(?i)\b[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b")
_URL = re.compile(r"(?i)\b[a-z][a-z0-9+.\-]*://[^\s\"']+")
_PATH = re.compile(r"\"[^\"]*[\\/][^\"]*\"|'[^']*[\\/][^']*'")
_HEX_ID = re.compile(r"\b[0-9a-fA-F]{16,}\b")
_NUMBER = re.compile(r"\b\d+\b")
_WHITESPACE = re.compile(r"\s+")


def scrub(text: str) -> str:
    """Remove every recognised secret class and bound the result."""
    scrubbed = _URL_CREDENTIALS.sub(r"\1[REDACTED:credentials]@", text)
    scrubbed = _BEARER.sub("Bearer [REDACTED:bearer]", scrubbed)
    scrubbed = _BASIC.sub("Basic [REDACTED:basic]", scrubbed)
    scrubbed = _PROVIDER_KEY.sub("[REDACTED:provider_key]", scrubbed)
    scrubbed = _SECRET_ASSIGNMENT.sub(
        lambda match: f"[REDACTED:{match.group(1).lower()}]",
        scrubbed,
    )
    scrubbed = _EMAIL.sub("[REDACTED:email]", scrubbed)
    if len(scrubbed) > MAX_SCRUBBED_CHARS:
        scrubbed = scrubbed[:MAX_SCRUBBED_CHARS] + TRUNCATION_MARKER
    return scrubbed


def fingerprint(kind: str, message: str) -> str:
    """Stable 16-hex id for one fault class.

    ``kind`` separates unrelated faults that share wording; the normalised
    message folds ids, addresses, counts and paths so repeats of one fault
    produce the same id.
    """
    normalised = _WHITESPACE.sub(" ", message.strip().lower())
    for pattern, replacement in (
        (_URL, "<url>"),
        (_EMAIL, "<email>"),
        (_UUID, "<uuid>"),
        (_PATH, "<path>"),
        (_HEX_ID, "<hex>"),
        (_NUMBER, "<n>"),
    ):
        normalised = pattern.sub(replacement, normalised)
    digest = hashlib.sha256(f"{kind}\x1f{normalised}".encode()).hexdigest()
    return digest[:16]
