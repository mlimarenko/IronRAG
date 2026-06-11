#!/usr/bin/env bash
# Unit + offline-integration tests for install.sh (the setup wizard).
#
# Two layers:
#   1. Pure functions sourced from install.sh (IRONRAG_INSTALL_SOURCE_ONLY=1):
#      sizing calibration, CPU clamp, atomic/secret-safe .env merge.
#   2. The full main flow run offline (IRONRAG_INSTALL_SKIP_DOWNLOAD=1 +
#      IRONRAG_INSTALL_SKIP_DEPLOY=1): proves provider secrets survive a re-run
#      and that resource caps follow the documented update semantics.
#
# Run: tests/install_wizard.test.sh   (no Docker or network required)
set -uo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALL_SH="${ROOT_DIR}/install.sh"

PASS=0
FAIL=0
fail() { FAIL=$((FAIL + 1)); printf 'FAIL: %s\n' "$*" >&2; }
pass() { PASS=$((PASS + 1)); }
check() { # check <name> <actual> <expected>
  if [ "$2" = "$3" ]; then pass; else fail "$1: expected [$3], got [$2]"; fi
}

# ── Layer 1: source the pure functions. ─────────────────────────────────────
# shellcheck disable=SC1090
IRONRAG_INSTALL_SOURCE_ONLY=1 source "$INSTALL_SH"
set +eu  # install.sh enables `set -euo pipefail`; relax it for assertions.
setup_colors  # define C_* (no colour when piped)

echo "── recommend_profile thresholds ──"
check "4 GiB -> micro"   "$(recommend_profile 4096)"  "micro"
check "7 GiB -> micro"   "$(recommend_profile 7168)"  "micro"
check "8 GiB -> small"   "$(recommend_profile 8192)"  "small"
check "12 GiB -> small"  "$(recommend_profile 12288)" "small"
check "16 GiB -> medium" "$(recommend_profile 16384)" "medium"
check "30 GiB -> medium" "$(recommend_profile 30720)" "medium"
check "32 GiB -> large"  "$(recommend_profile 32768)" "large"
check "64 GiB -> large"  "$(recommend_profile 65536)" "large"

echo "── compute_plan calibration: 4 GiB == docker-compose.yml defaults ──"
compute_plan 4 4096 ""
check "micro profile"   "$REC_PROFILE"     "micro"
check "micro db mem"    "$REC_DB_MEM"      "1024"
check "micro backend"   "$REC_BACKEND_MEM" "1024"
check "micro worker"    "$REC_WORKER_MEM"  "768"
check "micro cache"     "$REC_CACHE_MEM"   "192"
check "micro frontend"  "$REC_FRONTEND_MEM" "192"
check "micro steady"    "$REC_STEADY_MIB"  "3200"
check "micro fits 4G"   "$REC_FITS"        "1"

echo "── compute_plan calibration: 16 GiB == .env.example worked example ──"
compute_plan 8 16384 ""
check "medium profile"  "$REC_PROFILE"     "medium"
check "medium db mem"   "$REC_DB_MEM"      "4096"
check "medium backend"  "$REC_BACKEND_MEM" "3584"
check "medium worker"   "$REC_WORKER_MEM"  "3072"
check "medium cache"    "$REC_CACHE_MEM"   "768"
check "medium frontend" "$REC_FRONTEND_MEM" "256"
check "medium db cpus"  "$REC_DB_CPUS"     "2.00"
check "medium be cpus"  "$REC_BACKEND_CPUS" "6.00"
check "medium fe cpus"  "$REC_FRONTEND_CPUS" "1.00"
check "medium fits 16G" "$REC_FITS"        "1"

echo "── compute_plan: large + small sanity ──"
compute_plan 16 32768 ""
check "large profile"   "$REC_PROFILE"     "large"
check "large db mem"    "$REC_DB_MEM"      "8192"
check "large fits 32G"  "$REC_FITS"        "1"
compute_plan 4 8192 ""
check "small profile"   "$REC_PROFILE"     "small"
check "small steady"    "$REC_STEADY_MIB"  "5952"
check "small fits 8G"   "$REC_FITS"        "1"

echo "── compute_plan: tiny host must NOT fit (warning path) ──"
compute_plan 2 2048 "micro"
check "2 GiB micro forced -> no fit" "$REC_FITS" "0"

echo "── clamp_cpu: clamps to cores, floors at 0.25 ──"
check "600cc / 2 cores"  "$(clamp_cpu 600 2)"  "2.00"
check "600cc / 32 cores" "$(clamp_cpu 600 32)" "6.00"
check "50cc / 32 cores"  "$(clamp_cpu 50 32)"  "0.50"
check "10cc / 1 core"    "$(clamp_cpu 10 1)"   "0.25"

echo "── mib_to_gib_str ──"
check "4096 -> 4.0"   "$(mib_to_gib_str 4096)"  "4.0"
check "16384 -> 16.0" "$(mib_to_gib_str 16384)" "16.0"

echo "── env_file_set: atomic, verbatim, no clobber (the operator's #1 fear) ──"
TMP_ENV="$(mktemp)"
# A key whose value contains every sed-hostile character: & \ | $ /
SECRET_VAL='sk-Aa&Bb\Cc|Dd$Ee/Ff'  # pragma: allowlist secret  (synthetic test value)
{
  printf 'IRONRAG_OPENAI_API_KEY=%s\n' "$SECRET_VAL"
  printf 'IRONRAG_PORT=19000\n'
  printf '# a comment line\n'
  printf 'IRONRAG_POSTGRES_PASSWORD=pgsecret123\n'  # pragma: allowlist secret
} >"$TMP_ENV"

env_file_set IRONRAG_PORT 8080 "$TMP_ENV"
env_file_set IRONRAG_DB_MEMORY_LIMIT 4096M "$TMP_ENV"
check "preserves special-char key" "$(env_get IRONRAG_OPENAI_API_KEY "$TMP_ENV")" "$SECRET_VAL"
check "preserves pg password"      "$(env_get IRONRAG_POSTGRES_PASSWORD "$TMP_ENV")" "pgsecret123"
check "updates existing in place"  "$(env_get IRONRAG_PORT "$TMP_ENV")" "8080"
check "appends new key"            "$(env_get IRONRAG_DB_MEMORY_LIMIT "$TMP_ENV")" "4096M"
check "comment preserved"          "$(grep -c '^# a comment line$' "$TMP_ENV")" "1"
# Idempotent update: re-set the same key, exactly one occurrence remains.
env_file_set IRONRAG_PORT 8080 "$TMP_ENV"
check "no duplicate on re-set"     "$(grep -c '^IRONRAG_PORT=' "$TMP_ENV")" "1"
# Value with & and | written verbatim and read back unchanged.
env_file_set IRONRAG_OPENAI_API_KEY 'new&val|with\specials' "$TMP_ENV"
check "verbatim special write"     "$(env_get IRONRAG_OPENAI_API_KEY "$TMP_ENV")" 'new&val|with\specials'
rm -f "$TMP_ENV"

# ── Layer 2: offline integration of the full main flow. ─────────────────────
echo "── integration: non-interactive re-run preserves provider secrets ──"
run_install() { # run_install <dir> [extra args...]
  local dir="$1"; shift
  IRONRAG_NONINTERACTIVE=1 \
  IRONRAG_INSTALL_SKIP_DOWNLOAD=1 \
  IRONRAG_INSTALL_SKIP_DEPLOY=1 \
    bash "$INSTALL_SH" local "$dir" "$@" </dev/null >/dev/null 2>&1
}

WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
# Seed an .env that already holds live secrets and NO pinned caps.
{
  printf 'IRONRAG_OPENAI_API_KEY=sk-live-secret-0001\n'    # pragma: allowlist secret
  printf 'IRONRAG_DEEPSEEK_API_KEY=ds-live&secret|0002\n'  # pragma: allowlist secret
  printf 'IRONRAG_POSTGRES_PASSWORD=pg-live-pw-0003\n'     # pragma: allowlist secret
  printf 'IRONRAG_BOOTSTRAP_TOKEN=boot-0004\n'             # pragma: allowlist secret
} >"${WORK}/.env"

run_install "$WORK"
rc=$?
check "exit 0 on re-run" "$rc" "0"
check "openai key intact"   "$(env_get IRONRAG_OPENAI_API_KEY "${WORK}/.env")"   "sk-live-secret-0001"
check "deepseek key intact" "$(env_get IRONRAG_DEEPSEEK_API_KEY "${WORK}/.env")" "ds-live&secret|0002"
check "pg password intact"  "$(env_get IRONRAG_POSTGRES_PASSWORD "${WORK}/.env")" "pg-live-pw-0003"
check "boot token intact"   "$(env_get IRONRAG_BOOTSTRAP_TOKEN "${WORK}/.env")"  "boot-0004"
# Caps were absent -> filled in.
check "caps filled on update" "$([ -n "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" ] && echo yes)" "yes"
# No leftover backup file.
check "no .env.bak left" "$([ -e "${WORK}/.env.bak" ] && echo present || echo gone)" "gone"
rm -rf "$WORK"

echo "── integration: re-run keeps pinned caps unless --recompute-resources ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
{
  printf 'IRONRAG_OPENAI_API_KEY=sk-pinned-0001\n'  # pragma: allowlist secret
  printf 'IRONRAG_DB_MEMORY_LIMIT=9999M\n'
} >"${WORK}/.env"

run_install "$WORK"
check "pinned cap preserved" "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" "9999M"
run_install "$WORK" --recompute-resources
recomputed="$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")"
if [ "$recomputed" != "9999M" ] && [ -n "$recomputed" ]; then pass; else fail "recompute should overwrite pinned cap, got [$recomputed]"; fi
check "key still intact after recompute" "$(env_get IRONRAG_OPENAI_API_KEY "${WORK}/.env")" "sk-pinned-0001"
rm -rf "$WORK"

echo "── integration: fresh .env mints secrets + writes caps ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
run_install "$WORK"
check "fresh .env created" "$([ -f "${WORK}/.env" ] && echo yes)" "yes"
check "minted pg password" "$([ -n "$(env_get IRONRAG_POSTGRES_PASSWORD "${WORK}/.env")" ] && echo yes)" "yes"
check "minted boot token"  "$([ -n "$(env_get IRONRAG_BOOTSTRAP_TOKEN "${WORK}/.env")" ] && echo yes)" "yes"
check "caps written fresh"  "$([ -n "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" ] && echo yes)" "yes"
rm -rf "$WORK"

echo "── integration: interactive prompts via pseudo-tty (keep vs change) ──"
# The wizard reads prompts from /dev/tty, so this is the only layer that
# exercises ask()/ask_secret() for real and guards the dynamic-scope shadowing
# bug (a caller var colliding with a helper local) that only bites interactively.
# Best-effort: skip cleanly where util-linux `script` (a pty) is unavailable.
if command -v script >/dev/null 2>&1; then
  IWORK="$(mktemp -d)"
  cp "${ROOT_DIR}/docker-compose.yml" "${IWORK}/"
  cp "${ROOT_DIR}/.env.example"      "${IWORK}/"
  printf 'IRONRAG_OPENAI_API_KEY=sk-keep-EXISTING\n' >"${IWORK}/.env"  # pragma: allowlist secret
  # 9 prompts in order: profile, port, admin-login, then the 6 provider keys
  # (OpenAI, DeepSeek, Qwen, GPTunnel, OpenRouter, RouterAI). Keep OpenAI
  # (blank), set DeepSeek, skip the rest.
  printf '\n\n\n\nds-NEW-typed\n\n\n\n\n' >"${IWORK}/answers.txt"
  if script -qec \
       "IRONRAG_INSTALL_SKIP_DEPLOY=1 IRONRAG_INSTALL_SKIP_DOWNLOAD=1 bash ${INSTALL_SH} --interactive local ${IWORK}" \
       /dev/null <"${IWORK}/answers.txt" >"${IWORK}/session.log" 2>&1; then
    check "interactive: OpenAI kept on Enter"    "$(env_get IRONRAG_OPENAI_API_KEY "${IWORK}/.env")"   "sk-keep-EXISTING"
    check "interactive: DeepSeek set from input" "$(env_get IRONRAG_DEEPSEEK_API_KEY "${IWORK}/.env")" "ds-NEW-typed"
    check "interactive: Qwen empty on skip"      "$(env_get IRONRAG_QWEN_API_KEY "${IWORK}/.env")"     ""
    check "interactive: caps written"            "$([ -n "$(env_get IRONRAG_DB_MEMORY_LIMIT "${IWORK}/.env")" ] && echo yes)" "yes"
    # Any bash runtime error is prefixed with the script name regardless of
    # locale ("install.sh: line N:" / "install.sh: строка N:").
    if grep -aq 'install.sh:' "${IWORK}/session.log"; then
      fail "interactive: shell error in session ($(grep -am1 'install.sh:' "${IWORK}/session.log"))"
    else pass; fi
  else
    echo "  SKIP: pty harness (script) failed to run on this host"
  fi
  rm -rf "$IWORK"
else
  echo "  SKIP: util-linux 'script' not available — interactive path not exercised"
fi

# ── Report ──
echo ""
echo "──────────────────────────────────────────"
echo "install.sh wizard tests: ${PASS} passed, ${FAIL} failed"
[ "$FAIL" -eq 0 ]
