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
encode_provider_map() {
  printf '%s' "$1" | base64 | tr -d '\n'
}

# ── Layer 1: source the pure functions. ─────────────────────────────────────
# shellcheck disable=SC1090
IRONRAG_INSTALL_SOURCE_ONLY=1 source "$INSTALL_SH"
set +eu  # install.sh enables `set -euo pipefail`; relax it for assertions.
setup_colors  # define C_* (no colour when piped)

echo "── distribution contract: every documented root env key is consumed by Compose ──"
missing_compose_keys=""
while IFS= read -r key; do
  case "$key" in
    *)
      if ! grep -Eq -- "(^|[^A-Z0-9_])${key}([^A-Z0-9_]|$)" "${ROOT_DIR}/docker-compose.yml"; then
        missing_compose_keys="${missing_compose_keys}${missing_compose_keys:+ }${key}"
      fi
      ;;
  esac
done < <(
  sed -nE 's/^[[:space:]]*#?[[:space:]]*([A-Z][A-Z0-9_]*)=.*/\1/p' \
    "${ROOT_DIR}/.env.example" | sort -u
)
check "root .env.example has no ignored Compose keys" "$missing_compose_keys" ""

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
check "micro provider max" "$REC_PROVIDER_MAX_OUTBOUND" "8"
check "micro provider reserve" "$REC_PROVIDER_QUERY_RESERVED" "2"

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
check "medium provider max" "$REC_PROVIDER_MAX_OUTBOUND" "16"
check "medium provider reserve" "$REC_PROVIDER_QUERY_RESERVED" "4"

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

echo "── latest tag parsing ──"
TMP_TAGS_JSON="$(mktemp)"
cat >"$TMP_TAGS_JSON" <<'JSON'
[
  {"name": "v0.5.9"},
  {"name": "v0.5.10"},
  {"name": "v0.6.0-rc.1"},
  {"name": "example"}
]
JSON
check "latest stable semver tag wins" "$(extract_latest_semver_tag_from_file "$TMP_TAGS_JSON")" "v0.5.10"
cat >"$TMP_TAGS_JSON" <<'JSON'
[
  {"name": "example"},
  {"name": "v0.6.0-rc.1"}
]
JSON
check "no stable semver tag returns empty" "$(extract_latest_semver_tag_from_file "$TMP_TAGS_JSON")" ""
rm -f "$TMP_TAGS_JSON"

echo "── env_file_set: atomic, verbatim, no clobber (the operator's #1 fear) ──"
TMP_ENV="$(mktemp)"
# A key whose value contains every sed-hostile character: & \ | $ /
SECRET_VAL='sk-Aa&Bb\Cc|Dd$Ee/Ff'  # pragma: allowlist secret  (synthetic test value)
{
  printf 'IRONRAG_TEST_SECRET=%s\n' "$SECRET_VAL"
  printf 'IRONRAG_PORT=19000\n'
  printf '# a comment line\n'
  printf 'IRONRAG_POSTGRES_PASSWORD=pgsecret123\n'  # pragma: allowlist secret
} >"$TMP_ENV"

env_file_set IRONRAG_PORT 8080 "$TMP_ENV"
env_file_set IRONRAG_DB_MEMORY_LIMIT 4096M "$TMP_ENV"
check "preserves special-char key" "$(env_get IRONRAG_TEST_SECRET "$TMP_ENV")" "$SECRET_VAL"
check "preserves pg password"      "$(env_get IRONRAG_POSTGRES_PASSWORD "$TMP_ENV")" "pgsecret123"
check "updates existing in place"  "$(env_get IRONRAG_PORT "$TMP_ENV")" "8080"
check "appends new key"            "$(env_get IRONRAG_DB_MEMORY_LIMIT "$TMP_ENV")" "4096M"
check "comment preserved"          "$(grep -c '^# a comment line$' "$TMP_ENV")" "1"
# Idempotent update: re-set the same key, exactly one occurrence remains.
env_file_set IRONRAG_PORT 8080 "$TMP_ENV"
check "no duplicate on re-set"     "$(grep -c '^IRONRAG_PORT=' "$TMP_ENV")" "1"
# Value with & and | written verbatim and read back unchanged.
env_file_set IRONRAG_TEST_SECRET 'new&val|with\specials' "$TMP_ENV"
check "verbatim special write"     "$(env_get IRONRAG_TEST_SECRET "$TMP_ENV")" 'new&val|with\specials'
rm -f "$TMP_ENV"

echo "── provider key JSON map: exact kinds, stable order, fail-closed validation ──"
PROVIDER_JSON_A='{"zeta-gateway":"  value$#\"\\юникод  ","alpha.7":"alpha-secret","zeta_gateway":"underscore-secret"}' # pragma: allowlist secret
PROVIDER_JSON_B='{"zeta_gateway":"underscore-secret","zeta-gateway":"  value$#\"\\юникод  ","alpha.7":"alpha-secret"}' # pragma: allowlist secret
CANONICAL_A="$(canonicalize_provider_api_keys_b64 "$(encode_provider_map "$PROVIDER_JSON_A")")"
CANONICAL_B="$(canonicalize_provider_api_keys_b64 "$(encode_provider_map "$PROVIDER_JSON_B")")"
check "provider JSON accepts arbitrary exact kinds" "$?" "0"
check "provider JSON canonical ordering is stable" \
  "$( [ "$CANONICAL_A" = "$CANONICAL_B" ] && echo yes || echo no )" "yes"
check "hyphen/dot/underscore kinds stay distinct" \
  "$(printf '%s' "$CANONICAL_A" | base64 -d | python3 -c 'import json,sys; value=json.load(sys.stdin); print("yes" if len(value)==3 and set(value)=={"zeta-gateway","alpha.7","zeta_gateway"} else "no")')" "yes"
check "credential leading/trailing whitespace stays byte-exact" \
  "$(printf '%s' "$CANONICAL_A" | base64 -d | python3 -c 'import json,sys; value=json.load(sys.stdin); print("yes" if value["zeta-gateway"] == "  value$#\"\\юникод  " else "no")')" "yes"
canonicalize_provider_api_keys_b64 " $(encode_provider_map "$PROVIDER_JSON_A")" >/dev/null 2>&1
check "surrounding base64 whitespace rejected" "$?" "2"
canonicalize_provider_api_keys_b64 \
  "$(encode_provider_map '{"duplicate":"first-secret","duplicate":"second-secret"}')" >/dev/null 2>&1 # pragma: allowlist secret
check "duplicate provider kind rejected" "$?" "2"
canonicalize_provider_api_keys_b64 \
  "$(encode_provider_map '{"invalid kind":"secret-value"}')" >/dev/null 2>&1 # pragma: allowlist secret
check "whitespace provider kind rejected" "$?" "2"
TOO_MANY_PROVIDER_ENTRIES="$(python3 -c 'import json; print(json.dumps({f"provider-{index}": "value" for index in range(257)}, separators=(",", ":")))')"
canonicalize_provider_api_keys_b64 "$(encode_provider_map "$TOO_MANY_PROVIDER_ENTRIES")" >/dev/null 2>&1
check "provider entry count is bounded" "$?" "2"
check "removed convention detected structurally" \
  "$(is_removed_provider_api_key_env_name IRONRAG_ANYTHING_API_KEY; echo $?)" "0"

echo "── release image pin sync: official pins update, custom overrides survive ──"
TMP_ENV="$(mktemp)"
{
  printf 'IRONRAG_BACKEND_IMAGE=pipingspace/ironrag-backend:v0.5.1\n'
  printf 'IRONRAG_FRONTEND_IMAGE=docker.io/pipingspace/ironrag-frontend:v0.5.1\n'
} >"$TMP_ENV"
IRONRAG_IMAGE_PINS_UPDATED=0
IRONRAG_TARGET_IMAGE_TAG=""
sync_release_image_pins "$TMP_ENV" "v0.5.2"
check "official backend pin updated"  "$(env_get IRONRAG_BACKEND_IMAGE "$TMP_ENV")" "pipingspace/ironrag-backend:v0.5.2"
check "official frontend pin updated" "$(env_get IRONRAG_FRONTEND_IMAGE "$TMP_ENV")" "pipingspace/ironrag-frontend:v0.5.2"
check "pin update flag set"           "$IRONRAG_IMAGE_PINS_UPDATED" "1"
rm -f "$TMP_ENV"

TMP_ENV="$(mktemp)"
{
  printf 'IRONRAG_BACKEND_IMAGE=ironrag-backend:local\n'
  printf 'IRONRAG_FRONTEND_IMAGE=registry.example.invalid/ironrag-frontend:v0.5.1\n'
  printf 'IRONRAG_REDIS_IMAGE=redis:8.8\n'
} >"$TMP_ENV"
IRONRAG_IMAGE_PINS_UPDATED=0
sync_release_image_pins "$TMP_ENV" "v0.5.2"
check "custom backend override kept"  "$(env_get IRONRAG_BACKEND_IMAGE "$TMP_ENV")" "ironrag-backend:local"
check "custom frontend override kept" "$(env_get IRONRAG_FRONTEND_IMAGE "$TMP_ENV")" "registry.example.invalid/ironrag-frontend:v0.5.1"
check "unrelated image kept"          "$(env_get IRONRAG_REDIS_IMAGE "$TMP_ENV")" "redis:8.8"
check "pin update flag clear"         "$IRONRAG_IMAGE_PINS_UPDATED" "0"
rm -f "$TMP_ENV"

# ── Layer 2: offline integration of the full main flow. ─────────────────────
echo "── integration: non-interactive re-run preserves provider secrets ──"
run_install() { # run_install <dir> [extra args...]
  local dir="$1"; shift
  IRONRAG_NONINTERACTIVE=1 \
  IRONRAG_INSTALL_SKIP_DOWNLOAD=1 \
  IRONRAG_INSTALL_SKIP_DEPLOY=1 \
  IRONRAG_DETECT_CPUS=8 \
  IRONRAG_DETECT_MEM_MIB=16384 \
    bash "$INSTALL_SH" local "$dir" "$@" </dev/null >/dev/null 2>&1
}

run_install_version() { # run_install_version <version> <dir> [extra args...]
  local version="$1" dir="$2"; shift 2
  IRONRAG_NONINTERACTIVE=1 \
  IRONRAG_INSTALL_SKIP_DOWNLOAD=1 \
  IRONRAG_INSTALL_SKIP_DEPLOY=1 \
  IRONRAG_DETECT_CPUS=8 \
  IRONRAG_DETECT_MEM_MIB=16384 \
    bash "$INSTALL_SH" "$version" "$dir" "$@" </dev/null >/dev/null 2>&1
}

echo "── integration: removed provider-specific env convention fails fast ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
printf 'IRONRAG_ARBITRARY_API_KEY=legacy-secret\n' >"${WORK}/.env" # pragma: allowlist secret
run_install "$WORK"
check "removed provider env exits 2" "$?" "2"
check "removed provider env remains byte-identical" \
  "$(env_get IRONRAG_ARBITRARY_API_KEY "${WORK}/.env")" "legacy-secret"
rm -rf "$WORK"

WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
LIVE_PROVIDER_MAP_B64="$(encode_provider_map '{"alpha7":"sk-live-secret-0001","zeta-gateway":"ds-live&secret|0002"}')" # pragma: allowlist secret
# Seed an .env that already holds live secrets and NO pinned caps.
{
  printf 'IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=%s\n' "$LIVE_PROVIDER_MAP_B64"
  printf 'IRONRAG_POSTGRES_PASSWORD=pg-live-pw-0003\n'     # pragma: allowlist secret
  printf 'IRONRAG_BOOTSTRAP_TOKEN=boot-0004\n'             # pragma: allowlist secret
  printf 'IRONRAG_CREDENTIAL_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n' # pragma: allowlist secret
  printf 'IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED=false\n'
} >"${WORK}/.env"

run_install "$WORK"
rc=$?
check "exit 0 on re-run" "$rc" "0"
check "provider JSON intact" \
  "$(env_get IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 "${WORK}/.env")" \
  "$LIVE_PROVIDER_MAP_B64"
check "pg password intact"  "$(env_get IRONRAG_POSTGRES_PASSWORD "${WORK}/.env")" "pg-live-pw-0003"
check "boot token intact"   "$(env_get IRONRAG_BOOTSTRAP_TOKEN "${WORK}/.env")"  "boot-0004"
check "credential key intact" "$(env_get IRONRAG_CREDENTIAL_MASTER_KEY "${WORK}/.env")" "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
check "credential write gate preserved" "$(env_get IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED "${WORK}/.env")" "false"
# Caps were absent -> filled in.
check "caps filled on update" "$([ -n "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" ] && echo yes)" "yes"
check "provider cap filled on update" "$(env_get IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND "${WORK}/.env")" "16"
check "provider reserve filled on update" "$(env_get IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED "${WORK}/.env")" "4"
# No leftover backup file.
check "no .env.bak left" "$([ -e "${WORK}/.env.bak" ] && echo present || echo gone)" "gone"
check "no staged .env left" "$(find "$WORK" -maxdepth 1 -name '.env.next.*' -print -quit)" ""
rm -rf "$WORK"

echo "── integration: TERM during staged .env write keeps live secrets atomic ──"
WORK="$(mktemp -d)"
ORIGINAL_ENV="$(mktemp)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
SIGNAL_PROVIDER_MAP_B64="$(encode_provider_map '{"alpha7":"sk-signal-secret-0001"}')" # pragma: allowlist secret
{
  printf 'IRONRAG_PORT=19000\n'
  printf 'IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=%s\n' "$SIGNAL_PROVIDER_MAP_B64"
  printf 'IRONRAG_CREDENTIAL_MASTER_KEY=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n' # pragma: allowlist secret
} >"${WORK}/.env"
cp "${WORK}/.env" "$ORIGINAL_ENV"
IRONRAG_NONINTERACTIVE=1 \
IRONRAG_INSTALL_SKIP_DOWNLOAD=1 \
IRONRAG_INSTALL_SKIP_DEPLOY=1 \
IRONRAG_INSTALL_TEST_SIGNAL_AFTER_ENV_STAGE=TERM \
  bash "$INSTALL_SH" local "$WORK" --port 19111 </dev/null >/dev/null 2>&1
signal_rc=$?
check "TERM seam exits 143" "$signal_rc" "143"
check "TERM leaves live .env byte-identical" "$(cmp -s "$ORIGINAL_ENV" "${WORK}/.env" && echo yes || echo no)" "yes"
check "TERM removes secret-bearing stage" "$(find "$WORK" -maxdepth 1 -name '.env.next.*' -print -quit)" ""
rm -f "$ORIGINAL_ENV"
rm -rf "$WORK"

echo "── integration: release re-run upgrades official image tags ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
RELEASE_PROVIDER_MAP_B64="$(encode_provider_map '{"alpha7":"sk-release-pin-0001"}')" # pragma: allowlist secret
{
  printf 'IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=%s\n' "$RELEASE_PROVIDER_MAP_B64"
  printf 'IRONRAG_BACKEND_IMAGE=pipingspace/ironrag-backend:v0.5.1\n'
  printf 'IRONRAG_FRONTEND_IMAGE=pipingspace/ironrag-frontend:v0.5.1\n'
} >"${WORK}/.env"
run_install_version "v0.5.2" "$WORK"
check "release backend image tag upgraded"  "$(env_get IRONRAG_BACKEND_IMAGE "${WORK}/.env")" "pipingspace/ironrag-backend:v0.5.2"
check "release frontend image tag upgraded" "$(env_get IRONRAG_FRONTEND_IMAGE "${WORK}/.env")" "pipingspace/ironrag-frontend:v0.5.2"
check "release secret intact" \
  "$(env_get IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 "${WORK}/.env")" \
  "$RELEASE_PROVIDER_MAP_B64"
rm -rf "$WORK"

echo "── integration: fresh release install pins image tags deterministically ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
run_install_version "v0.5.2" "$WORK"
check "fresh backend image tag pinned"  "$(env_get IRONRAG_BACKEND_IMAGE "${WORK}/.env")" "pipingspace/ironrag-backend:v0.5.2"
check "fresh frontend image tag pinned" "$(env_get IRONRAG_FRONTEND_IMAGE "${WORK}/.env")" "pipingspace/ironrag-frontend:v0.5.2"
rm -rf "$WORK"

echo "── integration: re-run keeps pinned caps unless --recompute-resources ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
PINNED_PROVIDER_MAP_B64="$(encode_provider_map '{"alpha7":"sk-pinned-0001"}')" # pragma: allowlist secret
{
  printf 'IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=%s\n' "$PINNED_PROVIDER_MAP_B64"
  printf 'IRONRAG_DB_MEMORY_LIMIT=9999M\n'
} >"${WORK}/.env"

run_install "$WORK"
check "pinned cap preserved" "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" "9999M"
run_install "$WORK" --profile micro --recompute-resources
recomputed="$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")"
if [ "$recomputed" != "9999M" ] && [ -n "$recomputed" ]; then pass; else fail "recompute should overwrite pinned cap, got [$recomputed]"; fi
check "recompute writes profile provider cap" "$(env_get IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND "${WORK}/.env")" "8"
check "recompute writes profile provider reserve" "$(env_get IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED "${WORK}/.env")" "2"
check "key still intact after recompute" \
  "$(env_get IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 "${WORK}/.env")" \
  "$PINNED_PROVIDER_MAP_B64"
rm -rf "$WORK"

echo "── integration: fresh .env mints secrets + writes caps ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
run_install "$WORK"
check "fresh .env created" "$([ -f "${WORK}/.env" ] && echo yes)" "yes"
check "minted pg password" "$([ -n "$(env_get IRONRAG_POSTGRES_PASSWORD "${WORK}/.env")" ] && echo yes)" "yes"
check "minted boot token"  "$([ -n "$(env_get IRONRAG_BOOTSTRAP_TOKEN "${WORK}/.env")" ] && echo yes)" "yes"
credential_key="$(env_get IRONRAG_CREDENTIAL_MASTER_KEY "${WORK}/.env")"
check "minted credential key" "$([ "${#credential_key}" -eq 44 ] && echo yes)" "yes"
check "fresh install enables credential writes" "$(env_get IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED "${WORK}/.env")" "true"
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
  INTERACTIVE_PROVIDER_MAP_B64="$(encode_provider_map '{"alpha7":"sk-keep-EXISTING"}')" # pragma: allowlist secret
  printf 'IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64=%s\n' "$INTERACTIVE_PROVIDER_MAP_B64" >"${IWORK}/.env"
  # Four prompts: profile, port, admin login, review confirmation. Provider
  # credentials are managed by the generic env namespace or the UI, so the
  # wizard never needs a hardcoded provider prompt catalog.
  printf '\n\n\n\n' >"${IWORK}/answers.txt"
  if script -qec \
       "IRONRAG_INSTALL_SKIP_DEPLOY=1 IRONRAG_INSTALL_SKIP_DOWNLOAD=1 bash ${INSTALL_SH} --interactive local ${IWORK}" \
       /dev/null <"${IWORK}/answers.txt" >"${IWORK}/session.log" 2>&1; then
    check "interactive: generic provider key kept" \
      "$(env_get IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 "${IWORK}/.env")" \
      "$INTERACTIVE_PROVIDER_MAP_B64"
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

# ── require_resolved: the non-interactive fail-fast contract (sourced). ──────
echo "── require_resolved: non-interactive contract ──"
# Synthetic inputs only — the "required" field lives in the test, not the
# installer's real variable table, so we exercise the mechanism without
# inventing a production required value.
(
  INTERACTIVE=0
  err_out="$(require_resolved "demo" "" 0 "--demo or IRONRAG_DEMO" 2>&1)"
  rc=$?
  check "missing + no default exits 3" "$rc" "3"
  case "$err_out" in
    *"required value 'demo' has no safe default"*) pass ;;
    *) fail "fail-fast message missing exact field name; got [$err_out]" ;;
  esac
  case "$err_out" in
    *"--demo or IRONRAG_DEMO"*) pass ;;
    *) fail "fail-fast message must list how to set it; got [$err_out]" ;;
  esac
  require_resolved "demo" "" 1 "x" 2>/dev/null; check "missing + safe default OK" "$?" "0"
  require_resolved "demo" "val" 0 "x" 2>/dev/null; check "present value OK" "$?" "0"
  INTERACTIVE=1
  require_resolved "demo" "" 0 "x" 2>/dev/null; check "interactive never fails (would prompt)" "$?" "0"
)

# ── resolve_value precedence: flag > env > prompt-default (sourced). ─────────
echo "── resolve_value: flag > env > default ──"
# INTERACTIVE is read by the sourced resolve_value/ask helpers (dynamic scope),
# so shellcheck cannot see the use from here.
# shellcheck disable=SC2034
INTERACTIVE=0
resolve_value RV "flagval" "envval" "p" "defval"; check "flag wins"     "$RV" "flagval"
resolve_value RV ""        "envval" "p" "defval"; check "env when no flag" "$RV" "envval"
resolve_value RV ""        ""       "p" "defval"; check "default when none" "$RV" "defval"
# Secret path has no flag tier: env wins, else default (no prompt with no TTY).
resolve_secret RS "envsec" "p" "defsec"; check "secret env wins" "$RS" "envsec"
resolve_secret RS ""       "p" "defsec"; check "secret default"  "$RS" "defsec"

# ── --help exits 0 and documents every flag + env var. ──────────────────────
echo "── --help: exits 0 and lists every option ──"
HELP_OUT="$(bash "$INSTALL_SH" --help 2>&1)"; check "--help exit 0" "$?" "0"
for tok in \
  "--non-interactive" "--interactive" "--port" "--profile" "--admin-login" \
  "--plan-only" "--recompute-resources" "--reset-volumes" "--help" \
  "IRONRAG_PORT" "IRONRAG_PROFILE" "IRONRAG_ADMIN_LOGIN" "IRONRAG_ADMIN_PASSWORD" \
  "IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64" \
  "precedence"
do
  case "$HELP_OUT" in
    *"$tok"*) pass ;;
    *) fail "--help omits ${tok}" ;;
  esac
done

# ── A value-taking flag with no operand must error (exit 2), not eat the next flag. ──
echo "── flag-needs-value guard ──"
bash "$INSTALL_SH" --port </dev/null >/dev/null 2>&1; check "--port without value -> 2" "$?" "2"
bash "$INSTALL_SH" --port --yes </dev/null >/dev/null 2>&1; check "--port then flag -> 2" "$?" "2"

# ── Non-interactive, env-var-driven full flow completes and writes answers. ──
echo "── integration: non-interactive env-var answers complete the flow ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
# Fresh .env; every answer is supplied via env (no flags), with no TTY. The
# synthetic values exercise the verbatim atomic .env write and log redaction.
ENV_ADMIN_PW='pw&with|specials'         # pragma: allowlist secret  (synthetic)
ENV_PROVIDER_PLAINTEXT='plaintext-$#-leak-marker' # pragma: allowlist secret  (synthetic)
ENV_PROVIDER_KEYS_JSON_B64="$(encode_provider_map "{\"unseen-gateway7\":\"${ENV_PROVIDER_PLAINTEXT}\"}")" # pragma: allowlist secret  (synthetic)
IRONRAG_NONINTERACTIVE=1 \
IRONRAG_INSTALL_SKIP_DOWNLOAD=1 \
IRONRAG_INSTALL_SKIP_DEPLOY=1 \
IRONRAG_PORT=18123 \
IRONRAG_PROFILE=small \
IRONRAG_ADMIN_LOGIN=root \
IRONRAG_ADMIN_PASSWORD="$ENV_ADMIN_PW" \
IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64="$ENV_PROVIDER_KEYS_JSON_B64" \
  bash "$INSTALL_SH" local "$WORK" </dev/null >"${WORK}/install.log" 2>&1
check "env-driven run exit 0" "$?" "0"
check "installer never prints encoded provider map" \
  "$(grep -Fq -- "$ENV_PROVIDER_KEYS_JSON_B64" "${WORK}/install.log" && echo leaked || echo clean)" "clean"
check "installer never prints provider plaintext" \
  "$(grep -Fq -- "$ENV_PROVIDER_PLAINTEXT" "${WORK}/install.log" && echo leaked || echo clean)" "clean"
check "env port written"      "$(env_get IRONRAG_PORT "${WORK}/.env")" "18123"
# small profile -> db mem 2048M (compute_plan calibration above).
check "env profile applied"   "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" "2048M"
check "env provider cap applied" "$(env_get IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND "${WORK}/.env")" "12"
check "env provider reserve applied" "$(env_get IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED "${WORK}/.env")" "3"
check "env admin login"       "$(env_get IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN "${WORK}/.env")" "root"
check "env admin password"    "$(env_get IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD "${WORK}/.env")" "$ENV_ADMIN_PW"
check "env arbitrary provider key" \
  "$(env_get IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 "${WORK}/.env")" "$ENV_PROVIDER_KEYS_JSON_B64"
check "frontend origin synced to port" \
  "$(env_get IRONRAG_FRONTEND_ORIGIN "${WORK}/.env")" \
  "http://127.0.0.1:18123,http://localhost:18123"
rm -rf "$WORK"

# ── Flag tier beats env tier for non-secret values. ─────────────────────────
echo "── integration: flag > env for --port / --profile ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
IRONRAG_NONINTERACTIVE=1 \
IRONRAG_INSTALL_SKIP_DOWNLOAD=1 \
IRONRAG_INSTALL_SKIP_DEPLOY=1 \
IRONRAG_PORT=18123 IRONRAG_PROFILE=micro \
  bash "$INSTALL_SH" --port 18999 --profile=large local "$WORK" </dev/null >/dev/null 2>&1
check "flag port beats env"    "$(env_get IRONRAG_PORT "${WORK}/.env")" "18999"
# large profile -> db mem 8192M, beating env's micro (1024M).
check "flag profile beats env" "$(env_get IRONRAG_DB_MEMORY_LIMIT "${WORK}/.env")" "8192M"
rm -rf "$WORK"

# ── No TTY on stdin auto-selects non-interactive (no flag, no env override). ──
echo "── integration: non-TTY stdin auto non-interactive ──"
WORK="$(mktemp -d)"
cp "${ROOT_DIR}/docker-compose.yml" "${WORK}/docker-compose.yml"
cp "${ROOT_DIR}/.env.example"      "${WORK}/.env.example"
# Deliberately do NOT pass --non-interactive / IRONRAG_NONINTERACTIVE: stdin is
# /dev/null (no TTY), so open_tty fails and the script must not hang on a prompt.
IRONRAG_INSTALL_SKIP_DOWNLOAD=1 IRONRAG_INSTALL_SKIP_DEPLOY=1 \
  bash "$INSTALL_SH" local "$WORK" </dev/null >"${WORK}/out.log" 2>&1
check "non-TTY run exit 0" "$?" "0"
case "$(cat "${WORK}/out.log")" in
  *"Non-interactive mode"*) pass ;;
  *) fail "non-TTY run did not announce non-interactive mode" ;;
esac
check "non-TTY wrote default port" "$(env_get IRONRAG_PORT "${WORK}/.env")" "19000"
rm -rf "$WORK"

# ── Report ──
echo ""
echo "──────────────────────────────────────────"
echo "install.sh wizard tests: ${PASS} passed, ${FAIL} failed"
[ "$FAIL" -eq 0 ]
