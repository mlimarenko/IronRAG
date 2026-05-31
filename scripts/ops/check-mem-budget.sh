#!/usr/bin/env bash
# Regression guard for the docker-compose memory budget.
#
# On the canonical swapless 16 GiB host the steady-state sum of `memory`
# LIMITS is the only real containment lever (Compose does not enforce
# `reservations`, and cgroup v2 lets the sum of caps exceed physical RAM —
# a combined RSS above ~16 GiB trips the kernel global OOM killer). This
# guard fails the build if a future edit to docker-compose.yml pushes the
# steady-state limit sum past the ceiling, so the host can never be silently
# re-oversubscribed. The one-shot `startup` migrator is excluded: it exits
# before steady state and never co-resides under load.
#
# The large-host overlay (docker-compose.large.yml) intentionally exceeds
# this ceiling and is NOT checked here — it targets 24-32 GiB hosts.
#
# Usage: scripts/ops/check-mem-budget.sh [compose-file] [ceiling-MiB]
#   defaults: docker-compose.yml, 14848 MiB (16 GiB - 1.5 GiB headroom)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE_FILE="${1:-${ROOT_DIR}/docker-compose.yml}"
CEILING_MIB="${2:-14848}"

if [[ ! -f "${COMPOSE_FILE}" ]]; then
  echo "check-mem-budget: compose file not found: ${COMPOSE_FILE}" >&2
  exit 2
fi

PYBIN="$(command -v python3 || command -v python || true)"
if [[ -z "${PYBIN}" ]]; then
  echo "check-mem-budget: python3 not found; cannot verify memory budget" >&2
  exit 2
fi

COMPOSE_FILE="${COMPOSE_FILE}" CEILING_MIB="${CEILING_MIB}" "${PYBIN}" - <<'PY'
import os
import re
import sys

compose_file = os.environ["COMPOSE_FILE"]
ceiling_mib = int(os.environ["CEILING_MIB"])

with open(compose_file, encoding="utf-8") as fh:
    lines = fh.readlines()

# One-shot services that exit before steady state and never co-reside.
EXCLUDED = {"startup"}


def mem_to_mib(raw: str) -> int:
    """Parse a docker-compose memory literal (e.g. 5120M, 2G) to MiB.

    Docker treats the b/k/m/g suffixes as binary (powers of 1024), so M == MiB
    and G == 1024 MiB. A bare integer is interpreted as bytes.
    """
    m = re.fullmatch(r"\s*(\d+)\s*([bkmgBKMG]?)\s*", raw)
    if not m:
        raise ValueError(f"unparseable memory literal: {raw!r}")
    value = int(m.group(1))
    suffix = m.group(2).lower()
    factors_mib = {"": 1 / (1024 * 1024), "b": 1 / (1024 * 1024),
                   "k": 1 / 1024, "m": 1, "g": 1024}
    return round(value * factors_mib[suffix])


# ── Pass 1: anchor name → limits.memory (MiB). ──────────────────────────────
# An anchor block looks like:
#   x-ironrag-resources-vector: &ironrag-resources-vector
#     limits:
#       memory: 5120M
#     reservations:
#       memory: 1G
anchor_mem: dict[str, int] = {}
anchor_def = re.compile(r"^x-ironrag-resources-[\w-]+:\s*&(ironrag-resources-[\w-]+)\s*$")
current_anchor = None
in_limits = False
for line in lines:
    m = anchor_def.match(line)
    if m:
        current_anchor = m.group(1)
        in_limits = False
        continue
    if current_anchor is None:
        continue
    # Leaving the anchor block (a new top-level key starts in column 0).
    if line and not line[0].isspace() and line.strip():
        current_anchor = None
        in_limits = False
        continue
    stripped = line.strip()
    if stripped == "limits:":
        in_limits = True
        continue
    if stripped == "reservations:":
        in_limits = False
        continue
    if in_limits:
        mm = re.match(r"memory:\s*(.+?)\s*$", stripped)
        if mm:
            anchor_mem[current_anchor] = mem_to_mib(mm.group(1))
            in_limits = False

# ── Pass 2: service name → anchor (via `<<: *anchor` under deploy.resources). ─
service_anchor: dict[str, str] = {}
in_services = False
current_service = None
svc_def = re.compile(r"^  (\w[\w-]*):\s*$")
merge_ref = re.compile(r"<<:\s*\*(ironrag-resources-[\w-]+)")
for line in lines:
    if re.match(r"^services:\s*$", line):
        in_services = True
        continue
    if not in_services:
        continue
    # A new top-level key (column 0, non-blank) ends the services section.
    if line and not line[0].isspace() and line.strip():
        in_services = False
        continue
    m = svc_def.match(line)
    if m:
        current_service = m.group(1)
        continue
    if current_service is not None:
        ref = merge_ref.search(line)
        if ref:
            service_anchor[current_service] = ref.group(1)

if not anchor_mem:
    print("check-mem-budget: no resource anchors found — parser out of sync "
          "with docker-compose.yml", file=sys.stderr)
    sys.exit(2)
if not service_anchor:
    print("check-mem-budget: no services reference a resource anchor — parser "
          "out of sync with docker-compose.yml", file=sys.stderr)
    sys.exit(2)

# ── Compute the steady-state sum. ───────────────────────────────────────────
total = 0
rows = []
for svc in sorted(service_anchor):
    anchor = service_anchor[svc]
    mib = anchor_mem.get(anchor)
    if mib is None:
        print(f"check-mem-budget: service {svc!r} references unknown anchor "
              f"{anchor!r}", file=sys.stderr)
        sys.exit(2)
    excluded = svc in EXCLUDED
    if not excluded:
        total += mib
    rows.append((svc, anchor, mib, excluded))

width = max(len(r[0]) for r in rows)
print(f"docker-compose memory budget ({os.path.basename(compose_file)}):")
for svc, anchor, mib, excluded in rows:
    note = "  (one-shot, excluded)" if excluded else ""
    print(f"  {svc:<{width}}  {mib:>5} MiB  [{anchor}]{note}")
print(f"  {'─' * (width + 2)}")
print(f"  steady-state Σ = {total} MiB = {total / 1024:.2f} GiB"
      f"  (ceiling {ceiling_mib} MiB = {ceiling_mib / 1024:.2f} GiB)")

if total > ceiling_mib:
    over = total - ceiling_mib
    print(f"\nFAIL: steady-state memory limit sum exceeds the budget ceiling by "
          f"{over} MiB ({over / 1024:.2f} GiB).\n"
          f"On a swapless 16 GiB host this risks the kernel global OOM killer. "
          f"Lower a service cap, or if this is intentional for a bigger host, "
          f"move the change into docker-compose.large.yml and raise the host.",
          file=sys.stderr)
    sys.exit(1)

print("\nOK: within budget.")
PY
