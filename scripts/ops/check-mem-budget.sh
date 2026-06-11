#!/usr/bin/env bash
# Regression guard for the docker-compose memory budget.
#
# The shipped defaults must fit a small 4 GiB VM. On a swapless host the
# steady-state sum of `memory` LIMITS is the only real containment lever
# (Compose does not enforce `reservations`, and cgroup v2 lets the sum of
# caps exceed physical RAM — a combined RSS above physical RAM trips the
# kernel global OOM killer). This guard fails the build if a future edit to
# docker-compose.yml pushes the steady-state limit sum past the ceiling, so
# the default stack can never silently outgrow a 4 GiB VM. The one-shot
# `startup` migrator is excluded: it exits before steady state and never
# co-resides under load.
#
# Each anchor's memory limit is env-overridable as
# `memory: ${IRONRAG_*_MEMORY_LIMIT:-<default>}` (CPU likewise via
# IRONRAG_*_CPUS); this guard parses the baseline DEFAULT (the 4 GiB-VM
# sizing). Raising a cap via env for a bigger host (e.g. a 16 GiB stage)
# intentionally exceeds this ceiling and is not checked here.
#
# Usage: scripts/ops/check-mem-budget.sh [compose-file] [ceiling-MiB]
#   defaults: docker-compose.yml, 3584 MiB (4 GiB - 0.5 GiB headroom)
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
COMPOSE_FILE="${1:-${ROOT_DIR}/docker-compose.yml}"
CEILING_MIB="${2:-3584}"

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


def compose_default(raw: str) -> str:
    """Resolve a docker-compose interpolation to its baseline default.

    `memory: ${IRONRAG_DB_MEMORY_LIMIT:-1024M}` carries the 4 GiB-VM
    baseline in the `:-<default>` clause; the guard sizes against that, not
    the operator's env override. A plain literal (`memory: 1024M`) passes
    through unchanged. An interpolation without a default is unparseable —
    a future edit must keep a literal default so the baseline is knowable.
    """
    m = re.fullmatch(r"\s*\$\{[A-Za-z_][A-Za-z0-9_]*:-(.*?)\}\s*", raw)
    if m:
        return m.group(1)
    if "${" in raw:
        raise ValueError(
            f"memory limit {raw!r} interpolates without a `:-<default>` "
            "clause; the baseline budget cannot be verified"
        )
    return raw


def mem_to_mib(raw: str) -> int:
    """Parse a docker-compose memory literal (e.g. 5120M, 2G) to MiB.

    Docker treats the b/k/m/g suffixes as binary (powers of 1024), so M == MiB
    and G == 1024 MiB. A bare integer is interpreted as bytes.
    """
    raw = compose_default(raw)
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
# Services gated behind a `profiles:` key (e.g. s4core under the `s4` profile)
# are opt-in and never start in the default stack, so they are excluded from
# the baseline budget exactly like the one-shot startup migrator.
service_anchor: dict[str, str] = {}
profiled: set[str] = set()
in_services = False
current_service = None
svc_def = re.compile(r"^  (\w[\w-]*):\s*$")
merge_ref = re.compile(r"<<:\s*\*(ironrag-resources-[\w-]+)")
profiles_key = re.compile(r"^    profiles:")
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
        if profiles_key.match(line):
            profiled.add(current_service)
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
    if svc in EXCLUDED:
        reason = "one-shot, excluded"
    elif svc in profiled:
        reason = "profile, excluded"
    else:
        reason = None
        total += mib
    rows.append((svc, anchor, mib, reason))

width = max(len(r[0]) for r in rows)
print(f"docker-compose memory budget ({os.path.basename(compose_file)}):")
for svc, anchor, mib, reason in rows:
    note = f"  ({reason})" if reason else ""
    print(f"  {svc:<{width}}  {mib:>5} MiB  [{anchor}]{note}")
print(f"  {'─' * (width + 2)}")
print(f"  steady-state Σ = {total} MiB = {total / 1024:.2f} GiB"
      f"  (ceiling {ceiling_mib} MiB = {ceiling_mib / 1024:.2f} GiB)")

if total > ceiling_mib:
    over = total - ceiling_mib
    print(f"\nFAIL: steady-state memory limit sum exceeds the budget ceiling by "
          f"{over} MiB ({over / 1024:.2f} GiB).\n"
          f"On a swapless 4 GiB VM this risks the kernel global OOM killer. "
          f"Lower a service cap. The baseline defaults must fit a 4 GiB VM; a "
          f"bigger host is opt-in via the IRONRAG_*_MEMORY_LIMIT env overrides, "
          f"not by raising the defaults here.",
          file=sys.stderr)
    sys.exit(1)

print("\nOK: within budget.")
PY
