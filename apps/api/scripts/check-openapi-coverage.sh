#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
http_root="$repo_root/apps/api/src/interfaces/http"

if [[ ! -d "$http_root" ]]; then
  echo "HTTP handler tree not found: $http_root" >&2
  exit 2
fi

python3 - "$repo_root" "$http_root" <<'PY'
import re
import sys
from pathlib import Path

repo_root = Path(sys.argv[1])
http_root = Path(sys.argv[2])

# MCP tool modules are JSON-RPC tool implementations, not Axum route handlers.
http_files = sorted(
    path
    for path in http_root.rglob("*.rs")
    if "mcp/tools" not in path.relative_to(http_root).as_posix()
)
http_entrypoint = http_root.with_suffix(".rs")
route_files = ([http_entrypoint] if http_entrypoint.exists() else []) + http_files

if not http_files:
    print(f"No Rust HTTP files found under {http_root}", file=sys.stderr)
    sys.exit(2)

route_method_pattern = re.compile(
    r"""
    (?:
        (?<![A-Za-z0-9_])(?:axum::routing::)?(?:get|post|put|patch|delete|head|options|trace)
        |
        \.(?:get|post|put|patch|delete|head|options|trace)
    )
    \s*\(\s*
    (?P<handler>(?:[A-Za-z_][A-Za-z0-9_]*::)*[A-Za-z_][A-Za-z0-9_]*)
    """,
    re.VERBOSE,
)
function_pattern = re.compile(
    r"^\s*pub(?:\s*\([^)]*\))?\s+(?:async\s+)?fn\s+"
    r"(?P<name>[A-Za-z_][A-Za-z0-9_]*)\s*\(",
)
skip_pattern = re.compile(r"//\s*openapi-skip:\s*\S")
route_start_pattern = re.compile(r"\.route\s*\(")


def strip_rust_comments_and_literals(text: str) -> str:
    output: list[str] = []
    index = 0
    state = "normal"
    while index < len(text):
        char = text[index]
        next_char = text[index + 1] if index + 1 < len(text) else ""

        if state == "normal":
            if char == "/" and next_char == "/":
                output.extend([" ", " "])
                index += 2
                state = "line_comment"
                continue
            if char == "/" and next_char == "*":
                output.extend([" ", " "])
                index += 2
                state = "block_comment"
                continue
            if char == '"':
                output.append(" ")
                index += 1
                state = "string"
                continue
            output.append(char)
            index += 1
            continue

        if state == "line_comment":
            output.append("\n" if char == "\n" else " ")
            index += 1
            if char == "\n":
                state = "normal"
            continue

        if state == "block_comment":
            if char == "*" and next_char == "/":
                output.extend([" ", " "])
                index += 2
                state = "normal"
                continue
            output.append("\n" if char == "\n" else " ")
            index += 1
            continue

        if state == "string":
            if char == "\\" and next_char:
                output.extend([" ", " "])
                index += 2
                continue
            output.append("\n" if char == "\n" else " ")
            index += 1
            if char == '"':
                state = "normal"
            continue

    return "".join(output)


def route_call_bodies(masked_text: str) -> list[str]:
    bodies: list[str] = []
    for match in route_start_pattern.finditer(masked_text):
        start = match.end()
        depth = 1
        index = start
        while index < len(masked_text):
            char = masked_text[index]
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    bodies.append(masked_text[start:index])
                    break
            index += 1
    return bodies


routed_handlers: set[str] = set()
for path in route_files:
    masked_text = strip_rust_comments_and_literals(path.read_text(encoding="utf-8"))
    for body in route_call_bodies(masked_text):
        for match in route_method_pattern.finditer(body):
            routed_handlers.add(match.group("handler").split("::")[-1])

if not routed_handlers:
    print(f"No routed handlers found under {http_root}", file=sys.stderr)
    sys.exit(2)


def relative(path: Path) -> str:
    return path.relative_to(repo_root).as_posix()


def preceding_attribute_block(lines: list[str], fn_line_index: int) -> str:
    block: list[str] = []
    index = fn_line_index - 1
    while index >= 0:
        line = lines[index]
        if not line.strip():
            break
        block.append(line)
        index -= 1
    block.reverse()
    return "\n".join(block)


missing: list[str] = []
for path in http_files:
    lines = path.read_text(encoding="utf-8").splitlines()
    for index, line in enumerate(lines):
        match = function_pattern.match(line)
        if not match:
            continue

        name = match.group("name")
        if name not in routed_handlers:
            continue

        attribute_block = preceding_attribute_block(lines, index)
        has_openapi_path = "#[utoipa::path" in attribute_block
        has_skip = bool(skip_pattern.search(attribute_block))
        if has_openapi_path or has_skip:
            continue

        missing.append(
            f"{relative(path)}:{index + 1}: routed handler {name} is missing "
            "#[utoipa::path(...)] or // openapi-skip: <reason>"
        )

if missing:
    print("OpenAPI annotation coverage failed:")
    for item in missing:
        print(item)
    sys.exit(1)

print(f"OpenAPI annotation coverage OK ({len(routed_handlers)} routed handler names checked).")
PY
