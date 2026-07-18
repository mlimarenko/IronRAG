#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# IronRAG installer / updater.
#
# Interactive setup wizard by default; fully scriptable for CI / Ansible.
#
#   curl -fsSL https://raw.githubusercontent.com/mlimarenko/IronRAG/master/install.sh | bash
#
# The wizard greets you, inspects the host (CPU + RAM), recommends a resource
# profile, and prompts for the important variables (port, admin, provider API
# keys, telemetry, storage) — each with a sensible default you can accept with
# Enter or skip. On a re-run it preserves your existing .env secrets and tuned
# caps, fills missing values, and advances official image pins to the selected
# release tag.
#
# Design constraints (do not regress):
#   * Single self-contained file — `curl | bash` ships only this script, so it
#     must run with no external TUI dependency (no gum/whiptail/dialog).
#   * `curl | bash` makes the script itself the shell's stdin, so prompts are
#     read from /dev/tty (fd 3), never from stdin. No TTY => non-interactive.
#   * The arg/env contract is stable: `install.sh [VERSION] [INSTALL_DIR]` plus
#     IRONRAG_PORT / IRONRAG_RESET_VOLUMES / … keep working unchanged.
#   * .env is rewritten atomically (temp + mv, never sed -i) and provider
#     secrets are asserted byte-identical after every write.
#   * Official IronRAG image pins are upgraded to the selected release tag on
#     re-run; custom image overrides are preserved.
#
# Flags:
#   -y, --yes, --non-interactive   Never prompt; use flags / env / existing .env / defaults.
#       --interactive              Force the wizard even if no TTY is detected.
#       --port <p>                 Published HTTP port (also IRONRAG_PORT).
#       --profile <name>           Resource profile micro|small|medium|large
#                                  (also IRONRAG_PROFILE). Default: auto from host RAM.
#       --admin-login <name>       Bootstrap admin login (also IRONRAG_ADMIN_LOGIN).
#       --plan-only                Detect + size + print the plan; write nothing,
#                                  deploy nothing, touch no network. Great for review.
#       --recompute-resources      On a re-run, recompute resource caps from the host
#                                  even if the existing .env already pins them.
#       --reset-volumes            Same as IRONRAG_RESET_VOLUMES=1 (wipe stale data
#                                  volumes when minting a fresh .env).
#   -h, --help                     Show this help.
#
# Answer precedence (so the same install is reproducible from CI / Ansible):
#   non-secret values  flag  >  env  >  interactive prompt / existing .env  >  default
#   secret values      env   >  existing .env  >  interactive prompt        >  skip
#
# Secrets (admin password, provider API keys) are intentionally NOT accepted as
# flags: argv is visible to other processes (`ps`, /proc/<pid>/cmdline) and leaks
# into shell history and CI logs. Pass them via environment variables or a
# pre-seeded .env instead — e.g. IRONRAG_ADMIN_PASSWORD or
# IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64.
#
# Test seams (used by tests/install_wizard.test.sh; harmless in production):
#   IRONRAG_INSTALL_SOURCE_ONLY=1  Define functions but do not run main (for sourcing).
#   IRONRAG_DETECT_CPUS / IRONRAG_DETECT_MEM_MIB  Override host detection.
#   IRONRAG_INSTALL_SKIP_DOWNLOAD=1  Reuse the docker-compose.yml + .env.example
#                                  already in INSTALL_DIR (offline / air-gapped re-run).
#   IRONRAG_INSTALL_SKIP_DEPLOY=1  Do everything except `docker compose pull/up`.
# ============================================================================

REPOSITORY="${IRONRAG_GITHUB_REPOSITORY:-mlimarenko/IronRAG}"
DEFAULT_PORT="${IRONRAG_DEFAULT_PORT:-19000}"
OFFICIAL_BACKEND_IMAGE="pipingspace/ironrag-backend"
OFFICIAL_FRONTEND_IMAGE="pipingspace/ironrag-frontend"

PROVIDER_API_KEYS_JSON_B64_ENV="IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64"
PROVIDER_KEYS=("${PROVIDER_API_KEYS_JSON_B64_ENV}")
# Machine secrets that are minted once and must survive every re-run.
SECRET_KEYS=(
  IRONRAG_POSTGRES_PASSWORD
  IRONRAG_BOOTSTRAP_TOKEN
  IRONRAG_CREDENTIAL_MASTER_KEY
  IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD
  IRONRAG_UI_BOOTSTRAP_ADMIN_API_TOKEN
)

# Full .env staging files contain secrets. Keep the live file untouched until
# every write and preservation assertion succeeds, and remove the stage on all
# ordinary error/signal exits. SIGKILL/power loss cannot run shell cleanup, so
# the same-directory stage is always mode 0600 and has an unmistakable suffix.
IRONRAG_ENV_STAGE_FILE=""
cleanup_env_stage() {
  if [ -n "${IRONRAG_ENV_STAGE_FILE:-}" ]; then
    rm -f -- "$IRONRAG_ENV_STAGE_FILE"
    IRONRAG_ENV_STAGE_FILE=""
  fi
}

install_env_cleanup_traps() {
  trap cleanup_env_stage EXIT
  trap 'cleanup_env_stage; exit 130' INT
  trap 'cleanup_env_stage; exit 143' TERM
}

# ─── Output helpers ─────────────────────────────────────────────────────────
# Colour only when fd 1 is a terminal and NO_COLOR is unset. ASCII-safe always.
setup_colors() {
  USE_COLOR=0
  if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-dumb}" != "dumb" ]; then
    USE_COLOR=1
  fi
  if [ "$USE_COLOR" = "1" ]; then
    C_RESET=$'\033[0m'; C_BOLD=$'\033[1m'; C_DIM=$'\033[2m'
    C_RED=$'\033[31m'; C_GREEN=$'\033[32m'; C_YELLOW=$'\033[33m'
    C_BLUE=$'\033[34m'; C_CYAN=$'\033[36m'
  else
    C_RESET=""; C_BOLD=""; C_DIM=""
    C_RED=""; C_GREEN=""; C_YELLOW=""; C_BLUE=""; C_CYAN=""
  fi
}

hr() { printf '%s\n' "${C_DIM}────────────────────────────────────────────────────────────${C_RESET}"; }
say() { printf '%s\n' "$*"; }
info() { printf '%s\n' "${C_CYAN}•${C_RESET} $*"; }
ok() { printf '%s\n' "${C_GREEN}✓${C_RESET} $*"; }
warn() { printf '%s\n' "${C_YELLOW}!${C_RESET} $*" >&2; }
err() { printf '%s\n' "${C_RED}error:${C_RESET} $*" >&2; }

banner() {
  hr
  printf '%s\n' "  ${C_BOLD}${C_BLUE}IronRAG${C_RESET} ${C_DIM}installer${C_RESET}"
  printf '%s\n' "  ${C_DIM}grounded answers over your own documents${C_RESET}"
  hr
}

# step <title> — interactive-only "Step i/N · title" header so the wizard reads
# as a progressing flow. Silent when non-interactive (scripted runs stay terse).
# Relies on STEP_NUM / STEP_TOTAL globals seeded in run_main.
step() {
  STEP_NUM=$(( STEP_NUM + 1 ))
  [ "${INTERACTIVE:-1}" = "1" ] || return 0
  printf '%s\n' "${C_BOLD}${C_BLUE}Step ${STEP_NUM}/${STEP_TOTAL}${C_RESET} ${C_DIM}·${C_RESET} ${C_BOLD}$*${C_RESET}"
}

# ─── Interactivity / TTY resolution ─────────────────────────────────────────
# curl|bash makes stdin the script, so [ -t 0 ] is false even on a real
# terminal. We instead open /dev/tty on fd 3: if that succeeds we can prompt
# (covers both `./install.sh` and the piped form); if not, run non-interactive.
TTY_FD_OPEN=0
open_tty() {
  TTY_FD_OPEN=0
  # /dev/tty may be a real device node yet still fail to open with ENXIO when
  # the process has no controlling terminal (e.g. some CI / nohup contexts), so
  # `[ -r /dev/tty ]` is not reliable — we must attempt the open. A bare
  # `exec 3</dev/tty` whose redirect fails would EXIT a non-interactive shell
  # and leak the OS error, so probe inside a subshell first (its failure stays
  # contained and `2>/dev/null` hides the message); only open for real once the
  # probe proved it works.
  if ( exec 3</dev/tty ) 2>/dev/null; then
    exec 3</dev/tty
    TTY_FD_OPEN=1
  fi
}

# read one line from the tty into the named var; never aborts under `set -e`
# (a closed/EOF tty just yields the empty string and the caller's default wins).
tty_read() {
  local __var="$1" __line=""
  if [ "$TTY_FD_OPEN" = "1" ]; then
    IFS= read -r -u 3 __line 2>/dev/null || __line=""
  fi
  printf -v "$__var" '%s' "$__line"
}

tty_read_secret() {
  local __var="$1" __line=""
  if [ "$TTY_FD_OPEN" = "1" ]; then
    IFS= read -r -s -u 3 __line 2>/dev/null || __line=""
    printf '\n' >/dev/tty 2>/dev/null || true
  fi
  printf -v "$__var" '%s' "$__line"
}

# ask <var> <prompt> <default>  — sets <var>; Enter accepts the default.
# All internals are __-prefixed: bash uses dynamic scope, so a plain local
# named `reply` here would SHADOW a caller that passes the var name "reply",
# silently writing our local instead of theirs (an unbound-variable crash
# under `set -u`). The __ prefix guarantees no collision with caller names.
ask() {
  local __var="$1" __prompt="$2" __default="$3" __reply=""
  if [ "$INTERACTIVE" != "1" ]; then
    printf -v "$__var" '%s' "$__default"
    return
  fi
  if [ -n "$__default" ]; then
    printf '%s %s[%s]%s ' "$__prompt" "$C_DIM" "$__default" "$C_RESET" >/dev/tty 2>/dev/null || true
  else
    printf '%s %s[skip]%s ' "$__prompt" "$C_DIM" "$C_RESET" >/dev/tty 2>/dev/null || true
  fi
  tty_read __reply
  printf -v "$__var" '%s' "${__reply:-$__default}"
}

# ask_secret <var> <prompt> <default>  — silent; Enter keeps the default.
ask_secret() {
  local __var="$1" __prompt="$2" __default="$3" __reply="" __hint="skip"
  if [ "$INTERACTIVE" != "1" ]; then
    printf -v "$__var" '%s' "$__default"
    return
  fi
  [ -n "$__default" ] && __hint="keep current"
  printf '%s %s[%s]%s ' "$__prompt" "$C_DIM" "$__hint" "$C_RESET" >/dev/tty 2>/dev/null || true
  tty_read_secret __reply
  printf -v "$__var" '%s' "${__reply:-$__default}"
}

# ask_yes_no <prompt> <default y|n> -> returns 0 for yes, 1 for no.
ask_yes_no() {
  local prompt="$1" default="$2" reply=""
  if [ "$INTERACTIVE" != "1" ]; then
    [ "$default" = "y" ]
    return
  fi
  local hint="y/N"; [ "$default" = "y" ] && hint="Y/n"
  printf '%s %s[%s]%s ' "$prompt" "$C_DIM" "$hint" "$C_RESET" >/dev/tty 2>/dev/null || true
  tty_read reply
  reply="${reply:-$default}"
  case "$reply" in
    [Yy]*) return 0 ;;
    *) return 1 ;;
  esac
}

# ─── Value resolution (flag > env > prompt) ─────────────────────────────────
# One resolver so every prompt is answerable from automation. A flag value (read
# from parse_args into a FLAG_* var) wins outright — even with a TTY — so a
# scripted `--port 8080` is honoured the same way interactive and non-interactive.
# With no flag/env value we fall through to ask(), which itself falls back to the
# supplied default when non-interactive. Secrets use resolve_secret (no flag tier,
# silent prompt) so they never travel through argv.

# resolve_value <out_var> <flag_value> <env_value> <prompt> <default>
resolve_value() {
  local __out="$1" __flag="$2" __env="$3" __prompt="$4" __default="$5" __tmp=""
  if [ -n "$__flag" ]; then
    printf -v "$__out" '%s' "$__flag"; return
  fi
  if [ -n "$__env" ]; then
    printf -v "$__out" '%s' "$__env"; return
  fi
  ask __tmp "$__prompt" "$__default"
  printf -v "$__out" '%s' "$__tmp"
}

# resolve_secret <out_var> <env_value> <prompt> <default>
# Secret path: env wins, else prompt (silent), else default. No flag tier.
resolve_secret() {
  local __out="$1" __env="$2" __prompt="$3" __default="$4" __tmp=""
  if [ -n "$__env" ]; then
    printf -v "$__out" '%s' "$__env"; return
  fi
  ask_secret __tmp "$__prompt" "$__default"
  printf -v "$__out" '%s' "$__tmp"
}

# require_resolved <display-name> <value> <has-safe-default 0|1> <how-to-set>
# Non-interactive contract: a required value with no safe default must fail fast
# with the exact flag/env var to set, never hang waiting for input that can't come.
require_resolved() {
  local name="$1" value="$2" has_default="$3" how="$4"
  if [ "${INTERACTIVE:-1}" != "1" ] && [ -z "$value" ] && [ "$has_default" != "1" ]; then
    err "non-interactive mode: required value '${name}' has no safe default."
    say "  Set it via: ${how}" >&2
    exit 3
  fi
}

# ─── External commands / download ───────────────────────────────────────────
require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    err "required command not found: $1"
    exit 1
  fi
}

download() {
  local url="$1" destination="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$destination"; return
  fi
  if command -v wget >/dev/null 2>&1; then
    wget -qO "$destination" "$url"; return
  fi
  err "curl or wget is required"
  exit 1
}

resolve_release_tag() {
  local release_api_url="https://api.github.com/repos/${REPOSITORY}/releases/latest"
  local tags_api_url="https://api.github.com/repos/${REPOSITORY}/tags?per_page=100"
  local release_file tags_file release_tag tag_tag selected_tag
  release_file="$(mktemp)"
  tags_file="$(mktemp)"
  trap 'rm -f "$release_file" "$tags_file"' RETURN

  release_tag=""
  if download "$release_api_url" "$release_file"; then
    release_tag="$(extract_latest_release_tag_from_file "$release_file")"
  else
    warn "failed to query latest GitHub release from ${release_api_url}"
  fi

  tag_tag=""
  if download "$tags_api_url" "$tags_file"; then
    tag_tag="$(extract_latest_semver_tag_from_file "$tags_file")"
  else
    warn "failed to query GitHub tags from ${tags_api_url}"
  fi

  selected_tag="$(printf '%s\n%s\n' "$release_tag" "$tag_tag" | awk 'NF' | sort -V | tail -n 1)"
  if [ -z "$selected_tag" ]; then
    err "failed to resolve latest release tag from ${release_api_url} or ${tags_api_url}"
    exit 1
  fi
  if [ -n "$release_tag" ] && [ -n "$tag_tag" ] && [ "$selected_tag" != "$release_tag" ]; then
    warn "latest GitHub release is ${release_tag}, but latest stable tag is ${tag_tag}; using ${selected_tag}."
  fi
  printf '%s\n' "$selected_tag"
}

extract_latest_release_tag_from_file() {
  sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

extract_latest_semver_tag_from_file() {
  sed -n 's/.*"name":[[:space:]]*"\([^"]*\)".*/\1/p' "$1" \
    | grep -E '^v?[0-9]+\.[0-9]+\.[0-9]+$' \
    | sort -V \
    | tail -n 1 \
    || true
}

# Hex secret, length in bytes (output is 2*n hex chars).
rand_hex_bytes() {
  local nbytes="${1:-24}"
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex "$nbytes"; return
  fi
  LC_ALL=C tr -dc 'a-f0-9' </dev/urandom | head -c "$((nbytes * 2))"
}

# Canonical standard-base64 encoding of exactly 32 random bytes. The value is
# written directly to .env and never printed.
rand_credential_master_key() {
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -base64 32 | tr -d '\n'; return
  fi
  head -c 32 /dev/urandom | base64 | tr -d '\n'
}

# ─── .env read / atomic write ───────────────────────────────────────────────
is_removed_provider_api_key_env_name() {
  [[ "$1" =~ ^IRONRAG_[A-Z][A-Z0-9_]*_API_KEY$ ]]
}

# Reject removed provider-specific env names without guessing an alias. The
# exact provider kind is data inside the JSON map and cannot be reconstructed
# safely from a normalized shell variable name.
validate_removed_provider_api_key_env_names() {
  local env_file="$1" line key

  if [ -f "$env_file" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      case "$line" in
        IRONRAG_*_API_KEY=*)
          key="${line%%=*}"
          if is_removed_provider_api_key_env_name "$key"; then
            err "${key} uses the removed provider-specific credential convention."
            err "move the exact catalog provider kind and credential into ${PROVIDER_API_KEYS_JSON_B64_ENV}; aliases are not guessed."
            return 2
          fi
          ;;
      esac
    done <"$env_file"
  fi

  while IFS= read -r key; do
    case "$key" in
      IRONRAG_*_API_KEY)
        if is_removed_provider_api_key_env_name "$key"; then
          err "${key} uses the removed provider-specific credential convention."
          err "move the exact catalog provider kind and credential into ${PROVIDER_API_KEYS_JSON_B64_ENV}; aliases are not guessed."
          return 2
        fi
        ;;
      esac
  done < <(compgen -e)
}

# Validates and canonicalizes the provider map without ever placing its value
# in argv or diagnostics. Python is required only when an env-managed provider
# map is actually configured; users who configure credentials in the UI do not
# need it. Exact provider-kind keys are preserved, sorted, and duplicate keys
# fail closed.
canonicalize_provider_api_keys_b64() {
  local encoded_map="$1"
  [ -n "$encoded_map" ] || { printf ''; return 0; }
  if ! command -v python3 >/dev/null 2>&1; then
    err "python3 is required to validate ${PROVIDER_API_KEYS_JSON_B64_ENV}."
    return 2
  fi
  if ! printf '%s' "$encoded_map" | python3 -c '
import base64
import binascii
import json
import sys
import unicodedata

MAX_JSON_BYTES = 1_048_576
MAX_BASE64_BYTES = ((MAX_JSON_BYTES + 2) // 3) * 4
MAX_ENTRIES = 256
MAX_PROVIDER_KIND_BYTES = 128
MAX_CREDENTIAL_BYTES = 65_536

def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate key")
        result[key] = value
    return result

try:
    encoded = sys.stdin.read()
    if len(encoded.encode("utf-8")) > MAX_BASE64_BYTES:
        raise ValueError("encoded map is too large")
    decoded = base64.b64decode(encoded, validate=True)
    if len(decoded) > MAX_JSON_BYTES:
        raise ValueError("decoded map is too large")
    if base64.b64encode(decoded).decode("ascii") != encoded:
        raise ValueError("non-canonical base64")
    value = json.loads(decoded.decode("utf-8"), object_pairs_hook=unique_object)
    if not isinstance(value, dict):
        raise ValueError("not an object")
    if len(value) > MAX_ENTRIES:
        raise ValueError("too many entries")
    for provider_kind, api_key in value.items():
        if not isinstance(provider_kind, str) or not isinstance(api_key, str):
            raise ValueError("invalid entry type")
        if not provider_kind or len(provider_kind.encode("utf-8")) > MAX_PROVIDER_KIND_BYTES:
            raise ValueError("invalid provider kind length")
        if any(char.isspace() or unicodedata.category(char) == "Cc" for char in provider_kind):
            raise ValueError("invalid provider kind character")
        if len(api_key.encode("utf-8")) > MAX_CREDENTIAL_BYTES:
            raise ValueError("credential is too large")
    canonical_json = json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    sys.stdout.write(base64.b64encode(canonical_json).decode("ascii"))
except (OSError, UnicodeError, ValueError, json.JSONDecodeError, TypeError, binascii.Error):
    raise SystemExit(2)
' 2>/dev/null; then
    err "${PROVIDER_API_KEYS_JSON_B64_ENV} must be canonical standard-base64 of a JSON object mapping exact provider kinds to string credentials."
    return 2
  fi
}

# Value of KEY= from the last matching line (empty if missing).
env_get() {
  local key="$1" file="$2"
  [ -f "$file" ] || { printf ''; return; }
  sed -n "s/^${key}=//p" "$file" 2>/dev/null | tail -n1 | tr -d '\r'
}

env_value_nonempty() {
  local v
  v="$(env_get "$1" "$2")"
  [ -n "${v//[[:space:]]/}" ]
}

image_tag_for_version() {
  local version="$1"
  case "$version" in
    v[0-9]*|[0-9]*|latest)
      printf '%s\n' "$version"
      return 0
      ;;
    *)
      return 1
      ;;
  esac
}

is_mutable_official_image_pin() {
  local current="$1" image="$2"
  case "$current" in
    ""|"$image"|"$image":*|"docker.io/$image"|"docker.io/$image":*)
      case "$current" in
        *@sha256:*) return 1 ;;
        *) return 0 ;;
      esac
      ;;
  esac
  return 1
}

sync_release_image_pin() {
  local key="$1" image="$2" tag="$3" file="$4"
  local current desired
  current="$(env_get "$key" "$file")"
  desired="${image}:${tag}"
  if is_mutable_official_image_pin "$current" "$image"; then
    if [ "$current" != "$desired" ]; then
      env_file_set "$key" "$desired" "$file"
      return 0
    fi
  fi
  return 1
}

sync_release_image_pins() {
  local file="$1" version="$2" tag changed=0
  if ! tag="$(image_tag_for_version "$version")"; then
    return 1
  fi
  sync_release_image_pin "IRONRAG_BACKEND_IMAGE" "$OFFICIAL_BACKEND_IMAGE" "$tag" "$file" && changed=1
  sync_release_image_pin "IRONRAG_FRONTEND_IMAGE" "$OFFICIAL_FRONTEND_IMAGE" "$tag" "$file" && changed=1
  IRONRAG_TARGET_IMAGE_TAG="$tag"
  IRONRAG_IMAGE_PINS_UPDATED="$changed"
  return 0
}

# Atomic, sed-free upsert: rebuild the file to a temp and mv it into place so a
# crash can never truncate a live .env, and so values containing & \ | (API
# keys, passwords) are written verbatim. Comments and blank lines are preserved.
env_file_set() {
  local key="$1" val="$2" file="$3"
  local tmp found=0 line
  tmp="$(mktemp "${file}.XXXXXX")"
  if [ -f "$file" ]; then
    while IFS= read -r line || [ -n "$line" ]; do
      if [[ "$line" == "${key}="* ]]; then
        printf '%s=%s\n' "$key" "$val" >>"$tmp"
        found=1
      else
        printf '%s\n' "$line" >>"$tmp"
      fi
    done <"$file"
  fi
  if [ "$found" -eq 0 ]; then
    printf '%s=%s\n' "$key" "$val" >>"$tmp"
  fi
  mv "$tmp" "$file"
}

# ─── Host detection ─────────────────────────────────────────────────────────
detect_cpus() {
  if [ -n "${IRONRAG_DETECT_CPUS:-}" ]; then printf '%s\n' "$IRONRAG_DETECT_CPUS"; return; fi
  local n=""
  if command -v nproc >/dev/null 2>&1; then n="$(nproc 2>/dev/null || true)"; fi
  if [ -z "$n" ] && command -v sysctl >/dev/null 2>&1; then n="$(sysctl -n hw.ncpu 2>/dev/null || true)"; fi
  printf '%s\n' "${n:-1}"
}

detect_mem_mib() {
  if [ -n "${IRONRAG_DETECT_MEM_MIB:-}" ]; then printf '%s\n' "$IRONRAG_DETECT_MEM_MIB"; return; fi
  local mib=""
  if [ -r /proc/meminfo ]; then
    local kb
    kb="$(sed -n 's/^MemTotal:[[:space:]]*\([0-9]*\).*/\1/p' /proc/meminfo 2>/dev/null | head -n1)"
    [ -n "$kb" ] && mib=$(( kb / 1024 ))
  fi
  if [ -z "$mib" ] && command -v sysctl >/dev/null 2>&1; then
    local bytes
    bytes="$(sysctl -n hw.memsize 2>/dev/null || true)"
    [ -n "$bytes" ] && mib=$(( bytes / 1024 / 1024 ))
  fi
  printf '%s\n' "${mib:-2048}"
}

# Pretty GiB (one decimal) from MiB, integer-only math.
mib_to_gib_str() {
  local mib="$1"
  printf '%d.%d' $(( mib / 1024 )) $(( (mib % 1024) * 10 / 1024 ))
}

# ─── Resource sizing ────────────────────────────────────────────────────────
# Discrete profiles calibrated to two known-good points: the docker-compose.yml
# defaults are the 4 GiB-VM baseline (`micro`), and the .env.example worked
# example is a 16 GiB host (`medium`). Thresholds sit at 8/16/32 GiB so each
# profile's steady-state memory sum fits the low end of its range with > 0.5 GiB
# free. Per-role CPU is the profile value clamped to the detected core count.
# docling / heavy-pipeline parallelism stay `auto`: the backend derives them
# from these caps at runtime (auto_docling_max_concurrency_for_limits), so we
# never duplicate that formula in shell.
recommend_profile() {
  local mem_mib="$1"
  if   [ "$mem_mib" -lt 8192 ];  then printf 'micro\n'
  elif [ "$mem_mib" -lt 16384 ]; then printf 'small\n'
  elif [ "$mem_mib" -lt 32768 ]; then printf 'medium\n'
  else printf 'large\n'
  fi
}

# Populate REC_* globals for a profile + detected core count.
#   REC_PROFILE
#   REC_{DB,BACKEND,WORKER,CACHE,FRONTEND}_MEM   (MiB, e.g. 4096M written later)
#   REC_{DB,BACKEND,WORKER,CACHE,FRONTEND}_CPUS  (formatted X.XX)
#   REC_DATABASE_MAX_CONNECTIONS REC_EMBED_PARALLELISM REC_GRAPH_PARALLELISM
#   REC_PROVIDER_MAX_OUTBOUND REC_PROVIDER_QUERY_RESERVED
#   REC_REDIS_MAXMEMORY (MiB) REC_POSTGRES_SHM (MiB)
#   REC_STEADY_MIB REC_HEADROOM_MIB REC_FITS (0/1)
compute_plan() {
  local cpus="$1" mem_mib="$2" profile="$3"
  [ -n "$profile" ] || profile="$(recommend_profile "$mem_mib")"
  REC_PROFILE="$profile"

  # mem (MiB) and cpu (centi-cpus) per role, per profile.
  local db_mem be_mem wk_mem ca_mem fe_mem
  local db_cc be_cc wk_cc ca_cc fe_cc
  local dbconn embed graph provider_max provider_reserve redis shm
  case "$profile" in
    micro)
      db_mem=1024; be_mem=1024; wk_mem=768;  ca_mem=192;  fe_mem=192
      db_cc=100;   be_cc=200;   wk_cc=200;   ca_cc=50;    fe_cc=50
      dbconn=20; embed=8;  graph=16; provider_max=8;  provider_reserve=2; redis=128;  shm=256 ;;
    small)
      db_mem=2048; be_mem=1792; wk_mem=1536; ca_mem=384;  fe_mem=192
      db_cc=200;   be_cc=400;   wk_cc=400;   ca_cc=100;   fe_cc=50
      dbconn=25; embed=12; graph=16; provider_max=12; provider_reserve=3; redis=320;  shm=512 ;;
    medium)
      db_mem=4096; be_mem=3584; wk_mem=3072; ca_mem=768;  fe_mem=256
      db_cc=200;   be_cc=600;   wk_cc=600;   ca_cc=100;   fe_cc=100
      dbconn=30; embed=16; graph=16; provider_max=16; provider_reserve=4; redis=640;  shm=1024 ;;
    large)
      db_mem=8192; be_mem=6144; wk_mem=6144; ca_mem=1536; fe_mem=512
      db_cc=400;   be_cc=800;   wk_cc=800;   ca_cc=200;   fe_cc=100
      dbconn=32; embed=24; graph=24; provider_max=24; provider_reserve=6; redis=1280; shm=2048 ;;
    *) err "unknown profile: $profile"; exit 1 ;;
  esac

  REC_DB_MEM=$db_mem; REC_BACKEND_MEM=$be_mem; REC_WORKER_MEM=$wk_mem
  REC_CACHE_MEM=$ca_mem; REC_FRONTEND_MEM=$fe_mem
  REC_DB_CPUS="$(clamp_cpu "$db_cc" "$cpus")"
  REC_BACKEND_CPUS="$(clamp_cpu "$be_cc" "$cpus")"
  REC_WORKER_CPUS="$(clamp_cpu "$wk_cc" "$cpus")"
  REC_CACHE_CPUS="$(clamp_cpu "$ca_cc" "$cpus")"
  REC_FRONTEND_CPUS="$(clamp_cpu "$fe_cc" "$cpus")"
  REC_DATABASE_MAX_CONNECTIONS=$dbconn
  REC_EMBED_PARALLELISM=$embed
  REC_GRAPH_PARALLELISM=$graph
  REC_PROVIDER_MAX_OUTBOUND=$provider_max
  REC_PROVIDER_QUERY_RESERVED=$provider_reserve
  REC_REDIS_MAXMEMORY=$redis
  REC_POSTGRES_SHM=$shm

  REC_STEADY_MIB=$(( db_mem + be_mem + wk_mem + ca_mem + fe_mem ))
  # Reserve max(512 MiB, 12% of RAM) for the kernel + host — matches the repo's
  # own 4 GiB-VM baseline (check-mem-budget.sh ceiling = 4096 - 512).
  local pct=$(( mem_mib * 12 / 100 ))
  REC_HEADROOM_MIB=$(( pct > 512 ? pct : 512 ))
  if [ "$REC_STEADY_MIB" -le $(( mem_mib - REC_HEADROOM_MIB )) ]; then
    REC_FITS=1
  else
    REC_FITS=0
  fi
}

# Clamp centi-cpu request to the detected core count; format as X.XX.
clamp_cpu() {
  local cc="$1" cpus="$2"
  # NB: compute max_cc on its own line — a single `local a=$2 b=$((a*…))` reads
  # the not-yet-assigned local and silently uses an outer/unset value.
  local max_cc=$(( cpus * 100 ))
  [ "$cc" -gt "$max_cc" ] && cc=$max_cc
  [ "$cc" -lt 25 ] && cc=25
  printf '%d.%02d' $(( cc / 100 )) $(( cc % 100 ))
}

print_plan_table() {
  printf '  %s%-10s %-9s %-6s%s\n' "$C_DIM" "role" "memory" "cpu" "$C_RESET"
  printf '  %s\n' "${C_DIM}─────────────────────────────────${C_RESET}"
  printf '  %-10s %-9s %-6s\n' "postgres" "${REC_DB_MEM}M"       "$REC_DB_CPUS"
  printf '  %-10s %-9s %-6s\n' "backend"  "${REC_BACKEND_MEM}M"  "$REC_BACKEND_CPUS"
  printf '  %-10s %-9s %-6s\n' "worker"   "${REC_WORKER_MEM}M"   "$REC_WORKER_CPUS"
  printf '  %-10s %-9s %-6s\n' "redis"    "${REC_CACHE_MEM}M"    "$REC_CACHE_CPUS"
  printf '  %-10s %-9s %-6s\n' "frontend" "${REC_FRONTEND_MEM}M" "$REC_FRONTEND_CPUS"
  printf '  %s\n' "${C_DIM}─────────────────────────────────${C_RESET}"
  printf '  steady ≈ %s GiB  (parallelism: embed %s, graph/doc %s, provider %s/%s, db budget %s)\n' \
    "$(mib_to_gib_str "$REC_STEADY_MIB")" "$REC_EMBED_PARALLELISM" \
    "$REC_GRAPH_PARALLELISM" "$REC_PROVIDER_MAX_OUTBOUND" \
    "$REC_PROVIDER_QUERY_RESERVED" "$REC_DATABASE_MAX_CONNECTIONS"
}

# Write the computed resource plan into the env file.
write_resource_plan() {
  local file="$1"
  env_file_set IRONRAG_DB_CPUS            "$REC_DB_CPUS"            "$file"
  env_file_set IRONRAG_DB_MEMORY_LIMIT    "${REC_DB_MEM}M"         "$file"
  env_file_set IRONRAG_BACKEND_CPUS       "$REC_BACKEND_CPUS"      "$file"
  env_file_set IRONRAG_BACKEND_MEMORY_LIMIT "${REC_BACKEND_MEM}M"  "$file"
  env_file_set IRONRAG_WORKER_CPUS        "$REC_WORKER_CPUS"       "$file"
  env_file_set IRONRAG_WORKER_MEMORY_LIMIT "${REC_WORKER_MEM}M"    "$file"
  env_file_set IRONRAG_CACHE_CPUS         "$REC_CACHE_CPUS"        "$file"
  env_file_set IRONRAG_CACHE_MEMORY_LIMIT "${REC_CACHE_MEM}M"      "$file"
  env_file_set IRONRAG_FRONTEND_CPUS      "$REC_FRONTEND_CPUS"     "$file"
  env_file_set IRONRAG_FRONTEND_MEMORY_LIMIT "${REC_FRONTEND_MEM}M" "$file"
  env_file_set IRONRAG_REDIS_MAXMEMORY    "${REC_REDIS_MAXMEMORY}mb" "$file"
  env_file_set IRONRAG_POSTGRES_SHM_SIZE  "${REC_POSTGRES_SHM}mb"  "$file"
  env_file_set IRONRAG_DATABASE_MAX_CONNECTIONS "$REC_DATABASE_MAX_CONNECTIONS" "$file"
  env_file_set IRONRAG_INGESTION_EMBEDDING_PARALLELISM "$REC_EMBED_PARALLELISM" "$file"
  env_file_set IRONRAG_INGESTION_GRAPH_EXTRACT_PARALLELISM_PER_DOC "$REC_GRAPH_PARALLELISM" "$file"
  env_file_set IRONRAG_PROVIDER_CONCURRENCY_MAX_OUTBOUND "$REC_PROVIDER_MAX_OUTBOUND" "$file"
  env_file_set IRONRAG_PROVIDER_CONCURRENCY_QUERY_RESERVED "$REC_PROVIDER_QUERY_RESERVED" "$file"
}

sync_frontend_origin_to_port() {
  local file="$1" port="$2"
  env_file_set "IRONRAG_FRONTEND_ORIGIN" "http://127.0.0.1:${port},http://localhost:${port}" "$file"
}

# ─── Startup watcher (unchanged behaviour) ──────────────────────────────────
# Watch the one-shot startup container before bringing up app services. Some
# Compose versions block forever on `service_completed_successfully` dependents
# when startup restarts, so the installer owns the wait loop and prints the real
# failure (including the migration-checksum-drift recovery steps) instead of
# leaving `ironrag-startup-1 Waiting`.
wait_for_startup_authority() {
  local install_dir="$1"
  local wait_seconds="${IRONRAG_STARTUP_WAIT_SECS:-300}"
  local deadline=$(( $(date +%s) + wait_seconds ))
  local startup_id

  while [ "$(date +%s)" -lt "$deadline" ]; do
    startup_id="$(cd "$install_dir" && docker compose ps -a -q startup 2>/dev/null || true)"
    if [ -z "$startup_id" ]; then
      sleep 2
      continue
    fi

    local startup_logs
    startup_logs="$(docker logs "$startup_id" 2>&1 | tail -n 200)"

    local drift_line
    drift_line="$(printf '%s\n' "$startup_logs" | grep -m 1 -E 'migration [0-9]+ was previously applied but has been modified' || true)"
    if [ -n "$drift_line" ]; then
      local version padded_version
      version="$(printf '%s\n' "$drift_line" | grep -oE '[0-9]+' | head -n 1)"
      padded_version="$(printf '%04d' "$version")"
      cat >&2 <<DRIFT_ERR
ERROR: ironrag-startup-1 keeps restarting because the bundled
       schema for migration ${version} doesn't match the one applied
       to this database. sqlx refuses to start until the recorded
       checksum matches the file in the running image.
       This happens when an existing deployment pulls a release that
       touched a previously-applied migration.

       Resolve in three steps:

       1. Extract and apply the canonical idempotent migration file:

            docker compose -f ${install_dir}/docker-compose.yml run --rm --no-deps \\
              --entrypoint sh backend \\
              -c 'cat /app/migrations/${padded_version}_*.sql' \\
              >/tmp/ironrag-migration-${version}.sql

            docker compose -f ${install_dir}/docker-compose.yml exec -T \\
              postgres psql -U postgres -d ironrag \\
              </tmp/ironrag-migration-${version}.sql

       2. Compute the new checksum for migration ${version} from the
          running backend image:

            docker compose -f ${install_dir}/docker-compose.yml run --rm --no-deps \\
              --entrypoint sha384sum backend \\
              /app/migrations/${padded_version}_*.sql

       3. Update the row in _sqlx_migrations:

            docker compose -f ${install_dir}/docker-compose.yml exec \\
              postgres psql -U postgres -d ironrag -c \\
              "UPDATE _sqlx_migrations SET checksum = decode('<NEW_HEX>','hex') WHERE version = ${version};"

       Then restart the stack:

            docker compose -f ${install_dir}/docker-compose.yml restart \\
              startup backend worker

       Stack is stopped now to avoid a silent restart loop.
DRIFT_ERR
      (cd "$install_dir" && docker compose stop startup backend worker frontend >/dev/null 2>&1 || true)
      return 1
    fi

    local state status exit_code restart_count
    state="$(docker inspect "$startup_id" -f '{{.State.Status}}|{{.State.ExitCode}}|{{.RestartCount}}' 2>/dev/null || echo 'unknown|1|0')"
    status="${state%%|*}"
    state="${state#*|}"
    exit_code="${state%%|*}"
    restart_count="${state##*|}"

    if [ "$status" = "exited" ] && [ "$exit_code" = "0" ]; then
      return 0
    fi

    if { [ "$status" = "exited" ] && [ "$exit_code" != "0" ]; } \
      || { [ "$status" = "restarting" ] && [ "${restart_count:-0}" -gt 0 ]; }; then
      cat >&2 <<STARTUP_ERR
ERROR: ironrag-startup-1 failed before the API could start.

Last startup logs:
${startup_logs}

Stack is stopped now to avoid a silent restart loop.
STARTUP_ERR
      (cd "$install_dir" && docker compose stop startup backend worker frontend >/dev/null 2>&1 || true)
      return 1
    fi

    sleep 3
  done

  startup_id="$(cd "$install_dir" && docker compose ps -a -q startup 2>/dev/null || true)"
  if [ -n "$startup_id" ]; then
    docker logs "$startup_id" --tail 200 >&2 || true
  fi
  err "ironrag-startup-1 did not finish within ${wait_seconds}s."
  (cd "$install_dir" && docker compose stop startup backend worker frontend >/dev/null 2>&1 || true)
  return 1
}

# ─── Final summary ──────────────────────────────────────────────────────────
print_configuration_summary() {
  local env_file="$1"
  hr
  printf '%s\n' "${C_BOLD}Configuration${C_RESET}"
  if [ "${IRONRAG_NEW_ENV_SECRETS:-0}" = "1" ]; then
    ok "New .env created with random Postgres password, bootstrap token, and credential encryption key (not printed)."
  elif [ "${IRONRAG_IMAGE_PINS_UPDATED:-0}" = "1" ]; then
    ok "Existing .env preserved; official IronRAG images pinned to ${IRONRAG_TARGET_IMAGE_TAG}."
    info "Secrets, resource caps, and custom image overrides were left unchanged."
  else
    ok "Existing .env preserved; secrets and your tuned values unchanged."
  fi
  if env_value_nonempty "IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD" "$env_file"; then
    info "Admin: bootstrapped from .env (IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN / _PASSWORD)."
  else
    info "Admin: create the first account in the UI on first visit."
  fi
  local found=""
  env_value_nonempty "$PROVIDER_API_KEYS_JSON_B64_ENV" "$env_file" && found="yes"
  if [ -n "$found" ]; then
    info "LLM providers: at least one API key set in .env."
  else
    info "LLM providers: none set — add a key in .env or via the UI later."
  fi
  hr
}

usage() {
  # Embedded (not self-read from "$0"): under `curl … | bash` the script has no
  # file path, so reading "$0" would print nothing.
  cat <<'USAGE'
IronRAG installer / updater — interactive setup wizard by default, fully
scriptable for CI / Ansible.

Usage: install.sh [VERSION] [INSTALL_DIR] [flags]
  VERSION       release tag to install, or "latest" (default: latest)
  INSTALL_DIR   target directory (default: ironrag)

Flags:
  -y, --yes, --non-interactive   Never prompt; use flags / env / existing .env / defaults.
      --interactive              Force the wizard even if no TTY is detected.
      --port <p>                 Published HTTP port (default: 19000 or IRONRAG_PORT).
      --profile <name>           Resource profile: micro | small | medium | large
                                 (default: auto-detected from host RAM).
      --admin-login <name>       Bootstrap admin login (default: create it in the UI).
      --plan-only                Detect + size + print the plan; write/deploy nothing.
      --recompute-resources      On a re-run, recompute resource caps from the host.
      --reset-volumes            Wipe stale data volumes when minting a fresh .env.
  -h, --help                     Show this help.

Environment variables (answer every prompt without a TTY):
  IRONRAG_PORT                   Published HTTP port.
  IRONRAG_PROFILE                Resource profile (micro|small|medium|large).
  IRONRAG_ADMIN_LOGIN            Bootstrap admin login.
  IRONRAG_ADMIN_PASSWORD         Bootstrap admin password (secret; env only).
  IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64
                                 Canonical standard-base64 of a JSON object
                                 mapping exact provider kinds to API keys.
  IRONRAG_NONINTERACTIVE=1       Same as --non-interactive.
  IRONRAG_RESET_VOLUMES=1        Same as --reset-volumes.
  IRONRAG_RECOMPUTE_RESOURCES=1  Same as --recompute-resources.

Answer precedence:
  non-secret  flag > env > interactive prompt / existing .env > default
  secret      env  > existing .env > interactive prompt > skip

Secrets (admin password, provider API keys) are accepted via environment
variables or a pre-seeded .env only — never as flags, because argv is visible to
other processes (ps, /proc/<pid>/cmdline) and leaks into shell history and CI logs.

The wizard inspects the host (CPU + RAM), recommends a resource profile, and
prompts for port and admin bootstrap. Provider keys come from the validated map
above or the UI. A re-run preserves existing .env secrets and tuned caps while
advancing official IronRAG image pins to the selected release tag.

Non-interactive example (no TTY, env-driven):
  IRONRAG_PORT=8080 IRONRAG_PROFILE=small \
  IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64="$(printf '%s' '{"provider-alpha":"secret-value"}' | base64 | tr -d '\n')" \
    ./install.sh --non-interactive
USAGE
}

# ─── Argument parsing ───────────────────────────────────────────────────────
parse_args() {
  VERSION_INPUT=""
  INSTALL_DIR_INPUT=""
  FORCE_NONINTERACTIVE=0
  FORCE_INTERACTIVE=0
  PLAN_ONLY=0
  RECOMPUTE_RESOURCES="${IRONRAG_RECOMPUTE_RESOURCES:-0}"
  # Flag tier for the non-secret answers (empty => fall through to env/prompt).
  FLAG_PORT=""
  FLAG_PROFILE=""
  FLAG_ADMIN_LOGIN=""
  local positional=()
  # need_value <flag-name> <next-arg-or-empty>: require a non-flag operand.
  need_value() {
    if [ "$#" -lt 2 ] || [ -z "$2" ] || [ "${2:0:1}" = "-" ]; then
      err "flag $1 requires a value"; usage; exit 2
    fi
  }
  while [ "$#" -gt 0 ]; do
    case "$1" in
      -y|--yes|--non-interactive) FORCE_NONINTERACTIVE=1 ;;
      --interactive) FORCE_INTERACTIVE=1 ;;
      --plan-only|--dry-run) PLAN_ONLY=1 ;;
      --recompute-resources) RECOMPUTE_RESOURCES=1 ;;
      --reset-volumes) IRONRAG_RESET_VOLUMES=1 ;;
      --port) need_value "$1" "${2:-}"; FLAG_PORT="$2"; shift ;;
      --port=*) FLAG_PORT="${1#*=}" ;;
      --profile) need_value "$1" "${2:-}"; FLAG_PROFILE="$2"; shift ;;
      --profile=*) FLAG_PROFILE="${1#*=}" ;;
      --admin-login) need_value "$1" "${2:-}"; FLAG_ADMIN_LOGIN="$2"; shift ;;
      --admin-login=*) FLAG_ADMIN_LOGIN="${1#*=}" ;;
      -h|--help) usage; exit 0 ;;
      --) shift; while [ "$#" -gt 0 ]; do positional+=("$1"); shift; done; break ;;
      -*) err "unknown flag: $1"; usage; exit 2 ;;
      *) positional+=("$1") ;;
    esac
    shift
  done
  VERSION_INPUT="${positional[0]:-${VERSION_INPUT:-latest}}"
  INSTALL_DIR_INPUT="${positional[1]:-${INSTALL_DIR_INPUT:-ironrag}}"
}

resolve_interactivity() {
  open_tty
  INTERACTIVE=1
  if [ "$FORCE_NONINTERACTIVE" = "1" ] || [ "${IRONRAG_NONINTERACTIVE:-0}" = "1" ]; then
    INTERACTIVE=0
  elif [ "$FORCE_INTERACTIVE" = "1" ]; then
    INTERACTIVE=1
  elif [ "$TTY_FD_OPEN" != "1" ]; then
    INTERACTIVE=0
  fi
}

# ─── Main ───────────────────────────────────────────────────────────────────
run_main() {
  install_env_cleanup_traps
  setup_colors
  parse_args "$@"
  resolve_interactivity

  local cpus mem_mib
  cpus="$(detect_cpus)"
  mem_mib="$(detect_mem_mib)"

  # Total wizard steps for the "step i/N" progress headers (interactive only):
  # 1 host+profile, 2 port, 3 admin, 4 summary.
  STEP_TOTAL=4
  STEP_NUM=0

  banner
  if [ "$INTERACTIVE" = "1" ]; then
    say "  Welcome. This wizard inspects your host and sets up IronRAG."
    say "  Press ${C_BOLD}Enter${C_RESET} to accept the ${C_DIM}[default]${C_RESET}; values are saved to .env."
  else
    info "Non-interactive mode (no prompts; using flags / env / existing .env / defaults)."
  fi
  say ""

  # ── Step 1: host detection + profile selection ──
  step "Host & resource profile"
  info "Host: ${C_BOLD}${cpus}${C_RESET} vCPU, ${C_BOLD}$(mib_to_gib_str "$mem_mib")${C_RESET} GiB RAM"

  local recommended profile pick=""
  recommended="$(recommend_profile "$mem_mib")"
  # flag > env > interactive prompt > recommended default.
  resolve_value pick "$FLAG_PROFILE" "${IRONRAG_PROFILE:-}" \
    "Resource profile (micro/small/medium/large)" "$recommended"
  case "$pick" in
    micro|small|medium|large) profile="$pick" ;;
    "") profile="$recommended" ;;
    *) warn "unrecognised profile '$pick'; using recommended '$recommended'"; profile="$recommended" ;;
  esac
  compute_plan "$cpus" "$mem_mib" "$profile"
  say ""
  info "Profile: ${C_BOLD}${REC_PROFILE}${C_RESET}${C_DIM}$( [ "$REC_PROFILE" = "$recommended" ] && printf ' (recommended)' )${C_RESET}"
  print_plan_table
  if [ "$REC_FITS" != "1" ]; then
    warn "Profile steady set (~$(mib_to_gib_str "$REC_STEADY_MIB") GiB) leaves less than the"
    warn "reserved headroom on a $(mib_to_gib_str "$mem_mib") GiB host. On a swapless host this"
    warn "risks the kernel OOM killer — consider a smaller profile or more RAM."
    if [ "$INTERACTIVE" = "1" ]; then
      if ! ask_yes_no "Continue with this profile anyway?" "n"; then
        err "aborted by user"; exit 1
      fi
    fi
  fi
  say ""

  # ── Step 2: port ──
  step "Network port"
  # flag > env > interactive prompt > DEFAULT_PORT. (IRONRAG_PORT as default
  # keeps the prior env-as-default behaviour for the non-interactive path.)
  local port=""
  resolve_value port "$FLAG_PORT" "${IRONRAG_PORT:-}" \
    "Published HTTP port" "${IRONRAG_PORT:-$DEFAULT_PORT}"
  # Port always has a safe default (DEFAULT_PORT), so this never trips in normal
  # use — it asserts the non-interactive contract at the call site: were a future
  # value to lose its default, the run fails fast here instead of hanging.
  require_resolved "port" "$port" 1 "--port <p> or IRONRAG_PORT"
  IRONRAG_PORT="$port"

  # ── Step 3: admin bootstrap. Login via flag/env/prompt; password is a secret
  #    so it has no flag tier (env / prompt only). Non-interactive with no
  #    flag/env leaves both empty => no admin write (unchanged behaviour). ──
  local admin_login="" admin_pass=""
  declare -A NEW_PROVIDER=()
  if [ "$INTERACTIVE" = "1" ]; then
    say ""
    step "Admin bootstrap"
    info "${C_DIM}(optional — Enter to skip and create it in the UI)${C_RESET}"
  fi
  resolve_value admin_login "$FLAG_ADMIN_LOGIN" "${IRONRAG_ADMIN_LOGIN:-}" \
    "  Admin login" ""
  if [ -n "$admin_login" ]; then
    resolve_secret admin_pass "${IRONRAG_ADMIN_PASSWORD:-}" "  Admin password" ""
  fi
  if [ "$INTERACTIVE" = "1" ]; then
    info "${C_DIM}Provider credentials: pre-seed ${PROVIDER_API_KEYS_JSON_B64_ENV} or configure them in the UI.${C_RESET}"
  fi

  # ── Resolve version + download artifacts (skipped in plan-only) ──
  local VERSION RAW_BASE_URL INSTALL_DIR
  INSTALL_DIR="$INSTALL_DIR_INPUT"
  VERSION="$VERSION_INPUT"

  if [ "$PLAN_ONLY" = "1" ]; then
    say ""
    hr
    info "${C_BOLD}--plan-only${C_RESET}: nothing was written or deployed."
    info "Install dir: ${INSTALL_DIR}   Version: ${VERSION}   Port: ${IRONRAG_PORT}"
    hr
    return 0
  fi

  # Docker is only strictly required to deploy; the env-generation path can run
  # without it (and without network) for offline re-runs and tests.
  if [ "${IRONRAG_INSTALL_SKIP_DEPLOY:-0}" != "1" ]; then
    require_command docker
    docker compose version >/dev/null
  fi

  mkdir -p "$INSTALL_DIR"
  if [ "${IRONRAG_INSTALL_SKIP_DOWNLOAD:-0}" = "1" ]; then
    # Offline / air-gapped re-run: reuse the artifacts already in INSTALL_DIR.
    [ "$VERSION" = "latest" ] && VERSION="local"
    if [ ! -f "${INSTALL_DIR}/docker-compose.yml" ] || [ ! -f "${INSTALL_DIR}/.env.example" ]; then
      err "IRONRAG_INSTALL_SKIP_DOWNLOAD=1 but ${INSTALL_DIR}/docker-compose.yml or .env.example is missing."
      exit 1
    fi
    info "Reusing existing ${INSTALL_DIR}/docker-compose.yml + .env.example (download skipped)."
  else
    if [ "$VERSION" = "latest" ]; then
      VERSION="$(resolve_release_tag)"
    fi
    RAW_BASE_URL="https://raw.githubusercontent.com/${REPOSITORY}/${VERSION}"
    say ""
    info "Installing IronRAG ${C_BOLD}${VERSION}${C_RESET} into ${C_BOLD}${INSTALL_DIR}${C_RESET}"
    download "${RAW_BASE_URL}/docker-compose.yml" "${INSTALL_DIR}/docker-compose.yml"
    download "${RAW_BASE_URL}/.env.example" "${INSTALL_DIR}/.env.example"
  fi

  local env_file="${INSTALL_DIR}/.env"
  IRONRAG_NEW_ENV_SECRETS=0

  if [ ! -f "$env_file" ]; then
    # Refuse to mint a fresh random Postgres password when a stale data volume
    # from a previous install survives: Postgres bakes the initial password into
    # PGDATA, so a fresh .env would auth-loop forever otherwise. Only relevant
    # when we actually deploy — pure env-generation (skip-deploy) can't auth-loop.
    if [ "${IRONRAG_INSTALL_SKIP_DEPLOY:-0}" != "1" ]; then
      local stale_volumes=""
      if command -v docker >/dev/null 2>&1; then
        local vol
        for vol in ironrag_postgres_data ironrag_content_storage_data; do
          if docker volume inspect "$vol" >/dev/null 2>&1; then
            stale_volumes="${stale_volumes}${stale_volumes:+ }${vol}"
          fi
        done
      fi
      if [ -n "$stale_volumes" ]; then
        if [ "${IRONRAG_RESET_VOLUMES:-0}" = "1" ]; then
          warn "Wiping stale Docker volumes (IRONRAG_RESET_VOLUMES=1): $stale_volumes"
          docker volume rm $stale_volumes >/dev/null
        else
          err ".env is missing but stale Docker volumes survive from a previous install:"
          say "  $stale_volumes" >&2
          say "Minting fresh secrets would not match the passwords baked into those" >&2
          say "volumes (Postgres PGDATA). Pick one:" >&2
          say "  1. Restore the previous .env if you still have it." >&2
          say "  2. Re-run with --reset-volumes (or IRONRAG_RESET_VOLUMES=1) to wipe and start fresh." >&2
          exit 1
        fi
      fi
    fi
    cp "${INSTALL_DIR}/.env.example" "$env_file"
    chmod 600 "$env_file"
    IRONRAG_NEW_ENV_SECRETS=1
    env_file_set "IRONRAG_POSTGRES_PASSWORD" "$(rand_hex_bytes 24)" "$env_file"
    env_file_set "IRONRAG_BOOTSTRAP_TOKEN" "$(rand_hex_bytes 24)" "$env_file"
    env_file_set "IRONRAG_CREDENTIAL_MASTER_KEY" "$(rand_credential_master_key)" "$env_file"
    # A fresh install has no older pods, so encrypted writes are safe from the
    # first start. Existing upgrades preserve their explicit staged value.
    env_file_set "IRONRAG_CREDENTIAL_ENCRYPTION_WRITE_ENABLED" "true" "$env_file"
    ok "Created ${env_file} with fresh machine secrets."
  else
    ok "Existing ${env_file} found — preserving it."
  fi

  # Reject the removed provider-specific convention before any staged write.
  validate_removed_provider_api_key_env_names "$env_file" || exit $?

  # Snapshot every secret/provider value BEFORE writing, to assert preservation.
  declare -A SECRET_BEFORE=()
  local k
  for k in "${PROVIDER_KEYS[@]}" "${SECRET_KEYS[@]}"; do
    SECRET_BEFORE["$k"]="$(env_get "$k" "$env_file")"
  done

  # Provider credentials have one generic, typed JSON-map path. An exported
  # value wins over the existing .env entry; an unset/blank value preserves the file.
  # Interactive provider setup belongs to the UI and does not require a
  # hardcoded provider catalog in this installer.
  local key existing env_val
  for key in "${PROVIDER_KEYS[@]}"; do
    existing="$(env_get "$key" "$env_file")"
    env_val="${!key:-}"
    if [ -n "$env_val" ]; then
      env_val="$(canonicalize_provider_api_keys_b64 "$env_val")" || exit $?
      [ "$env_val" != "$existing" ] && NEW_PROVIDER["$key"]="$env_val"
    elif [ -n "$existing" ]; then
      canonicalize_provider_api_keys_b64 "$existing" >/dev/null || exit $?
    fi
  done

  # ── Step 4: review screen — show the resolved choices before touching .env so
  #    an interactive operator can abort. Scripted (non-interactive) runs skip the
  #    pause and just proceed, keeping the unattended path silent and fast. ──
  if [ "$INTERACTIVE" = "1" ]; then
    say ""
    step "Review"
    hr
    printf '  %-18s %s\n' "Resource profile" "${REC_PROFILE}"
    printf '  %-18s %s\n' "HTTP port" "${IRONRAG_PORT}"
    if [ -n "$admin_login" ]; then
      printf '  %-18s %s\n' "Admin login" "${admin_login}"
      printf '  %-18s %s\n' "Admin password" "$( [ -n "$admin_pass" ] && echo 'set' || echo 'unchanged' )"
    else
      printf '  %-18s %s\n' "Admin" "create in the UI on first visit"
    fi
    local provider_configured=0 provider_updated=0 provider_key
    for provider_key in "${PROVIDER_KEYS[@]}"; do
      if [ -n "${NEW_PROVIDER[$provider_key]:-}" ]; then
        provider_updated=$((provider_updated + 1))
      elif env_value_nonempty "$provider_key" "$env_file"; then
        provider_configured=$((provider_configured + 1))
      fi
    done
    printf '  %-18s %s\n' "Provider keys" "${provider_updated} updated, ${provider_configured} kept"
    hr
    if ! ask_yes_no "Apply this configuration?" "y"; then
      err "aborted by user"; exit 1
    fi
    say ""
  fi

  # ── Apply writes ──
  # Work on a restrictive same-directory copy and atomically rename it only
  # after the complete batch validates. The EXIT/INT/TERM traps remove this
  # secret-bearing stage on every pre-commit failure.
  local live_env_file="$env_file"
  IRONRAG_ENV_STAGE_FILE="$(mktemp "${live_env_file}.next.XXXXXX")"
  cp "$live_env_file" "$IRONRAG_ENV_STAGE_FILE"
  chmod 600 "$IRONRAG_ENV_STAGE_FILE"
  env_file="$IRONRAG_ENV_STAGE_FILE"

  # Port + derived frontend origin.
  env_file_set "IRONRAG_PORT" "$IRONRAG_PORT" "$env_file"
  sync_frontend_origin_to_port "$env_file" "$IRONRAG_PORT"
  sync_release_image_pins "$env_file" "$VERSION" || true

  # Deterministic signal seam for the offline installer regression test. It is
  # inert unless explicitly set by tests and exercises the actual TERM trap
  # after the staged file has already diverged from the live one.
  if [ "${IRONRAG_INSTALL_TEST_SIGNAL_AFTER_ENV_STAGE:-}" = "TERM" ]; then
    kill -TERM "$$"
  fi

  # Admin bootstrap (only when freshly provided this run).
  if [ -n "$admin_login" ]; then
    env_file_set "IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN" "$admin_login" "$env_file"
    [ -n "$admin_pass" ] && env_file_set "IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD" "$admin_pass" "$env_file"
  fi

  # Provider keys the operator changed this run. Guard the empty-array
  # expansion: `"${!arr[@]}"` on an empty associative array aborts under
  # `set -u` on bash < 4.4 (RHEL/CentOS 7 ships 4.2), and on the unattended
  # path NEW_PROVIDER is always empty. `${#arr[@]}` is safe on every version.
  if [ "${#NEW_PROVIDER[@]}" -gt 0 ]; then
    for k in "${PROVIDER_KEYS[@]}"; do
      if [ -n "${NEW_PROVIDER[$k]:-}" ]; then
        env_file_set "$k" "${NEW_PROVIDER[$k]}" "$env_file"
      fi
    done
  fi

  # Resource plan: write on a new .env, or when explicitly recomputing, or when
  # the caps are not yet pinned. Otherwise leave the operator's tuned values be.
  local caps_pinned=0
  env_value_nonempty "IRONRAG_DB_MEMORY_LIMIT" "$env_file" && caps_pinned=1
  if [ "$IRONRAG_NEW_ENV_SECRETS" = "1" ] || [ "$RECOMPUTE_RESOURCES" = "1" ] || [ "$caps_pinned" = "0" ]; then
    write_resource_plan "$env_file"
    ok "Resource profile '${REC_PROFILE}' written to .env."
  else
    info "Resource caps already pinned in .env — kept as-is (use --recompute-resources to refresh)."
  fi

  # ── Assert provider/machine secrets survived (the operator's #1 concern). ──
  for k in "${PROVIDER_KEYS[@]}" "${SECRET_KEYS[@]}"; do
    local before="${SECRET_BEFORE[$k]}" after
    after="$(env_get "$k" "$env_file")"
    # A key the operator deliberately changed this run is allowed to differ.
    if [ -n "${NEW_PROVIDER[$k]:-}" ]; then continue; fi
    # Carve-out must track the WRITE condition (admin_pass), not admin_login:
    # a returning operator who re-types the login but keeps the password leaves
    # the password unchanged, so its integrity check must still run.
    if [ -n "$admin_pass" ] && [ "$k" = "IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD" ]; then continue; fi
    if [ "$before" != "$after" ]; then
      err "secret ${k} changed unexpectedly during staged .env write."
      exit 1
    fi
  done
  mv -f -- "$env_file" "$live_env_file"
  IRONRAG_ENV_STAGE_FILE=""
  env_file="$live_env_file"

  # ── Deploy ──
  if [ "${IRONRAG_INSTALL_SKIP_DEPLOY:-0}" = "1" ]; then
    say ""
    ok "IRONRAG_INSTALL_SKIP_DEPLOY=1 — wrote ${env_file}, skipped docker compose."
    print_configuration_summary "$env_file"
    return 0
  fi

  say ""
  info "Pulling images and starting the data plane…"
  (
    cd "$INSTALL_DIR"
    docker compose pull
    docker compose up -d postgres redis startup
  )

  if ! wait_for_startup_authority "$INSTALL_DIR"; then
    exit 1
  fi

  info "Starting application services…"
  (
    cd "$INSTALL_DIR"
    docker compose up -d backend worker frontend
    # Nginx resolves the backend upstream at startup. If Compose recreated the
    # backend container but left the frontend untouched, /v1 proxying can keep a
    # stale Docker DNS target until nginx restarts.
    docker compose restart frontend >/dev/null
  )

  say ""
  hr
  ok "IronRAG ${VERSION} is starting."
  say "  Directory: ${INSTALL_DIR}"
  say "  App: ${C_BOLD}http://127.0.0.1:${IRONRAG_PORT}${C_RESET}"
  say "  MCP: http://127.0.0.1:${IRONRAG_PORT}/v1/mcp"
  hr
  print_configuration_summary "$env_file"
}

# Allow the test harness to source this file for unit-testing the pure
# functions without running the installer.
if [ "${IRONRAG_INSTALL_SOURCE_ONLY:-0}" = "1" ]; then
  return 0 2>/dev/null || true
fi

run_main "$@"
