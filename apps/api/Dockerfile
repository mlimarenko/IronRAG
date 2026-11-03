# syntax=docker/dockerfile:1.7

# ── Stage 1: Rust builder ──────────────────────────────────────────────
FROM rust@sha256:a8a5f0a1e5fe7dfe1d352591e4a1c7dd2c08fd70475cae872cf3458ba0df0546 AS builder
WORKDIR /workspace
# No default: the build identity is baked into the binary and served by
# /v1/version, so an unset value must stop the build rather than ship a
# placeholder that reads like a real release. Enforced below, before cargo runs.
ARG APP_VERSION

# Wild 0.9.0 linker for fast native-speed dockerfile builds (~2x mold on incremental link).
ARG WILD_VERSION=0.9.0
ARG WILD_SHA256=deb6ee0e5caec798053ec4aafaba042e20a8edf91f08cb4d36268571cc628d3b
COPY docker/apt/debian-backports.sources /etc/apt/sources.list.d/debian-backports.sources
COPY docker/apt/90-backports /etc/apt/preferences.d/90-backports
RUN apt-get update \
  && apt-get upgrade -y \
  && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates clang curl \
  && if curl -fsSL --connect-timeout 30 --max-time 300 --retry 5 --retry-delay 5 --retry-all-errors "https://github.com/wild-linker/wild/releases/download/${WILD_VERSION}/wild-linker-${WILD_VERSION}-x86_64-unknown-linux-gnu.tar.gz" -o /tmp/wild.tgz; then \
       echo "${WILD_SHA256}  /tmp/wild.tgz" | sha256sum -c - \
       && tar -xzf /tmp/wild.tgz -C /tmp \
       && install -m 0755 "/tmp/wild-linker-${WILD_VERSION}-x86_64-unknown-linux-gnu/wild" /usr/local/bin/wild; \
     else \
       status="$?"; \
       if [ "${status}" = "22" ]; then exit "${status}"; fi; \
       echo "wild download failed; using system ld fallback" >&2 \
       && ln -s /usr/bin/ld /usr/local/bin/wild; \
     fi \
  && rm -rf /tmp/wild.tgz "/tmp/wild-linker-${WILD_VERSION}-x86_64-unknown-linux-gnu" \
  && rm -rf /var/lib/apt/lists/*

# Fast linker + release-profile overrides for incremental Docker builds.
# Public releases (gh/master) reset these via `--build-arg` to restore
# the default Cargo release profile (lto, single codegen-unit, etc.).
# RUSTUP_TOOLCHAIN pins rustup to the toolchain already baked into the base
# image and short-circuits the `rust-toolchain.toml` channel re-validation,
# which otherwise reaches static.rust-lang.org on every build. That fetch can
# hang for the full rustup timeout in sandboxes with a dead IPv6 egress route,
# so the build must not depend on it — the base image is the source of truth
# for the toolchain version (asserted below against the digest-pinned image).
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_TOOLCHAIN=1.98.1 \
    RUSTFLAGS="-C linker=clang -C link-arg=--ld-path=/usr/local/bin/wild" \
    CARGO_INCREMENTAL=0 \
    APP_VERSION=${APP_VERSION}

# Copy the whole source tree once. BuildKit cache mounts keep `target/` and
# the cargo registry warm between builds — better reuse than manual manifest
# layer priming.
COPY . .

ARG CACHEBUST=0
# CARGO_FEATURES is an opt-in build-arg for diagnostic builds (e.g. `heap-profile`
# wires dhat-rs as the global allocator). Empty by default → stock build. Pass
# `--build-arg CARGO_FEATURES=heap-profile` to produce a profiling image; see
# docs/perf/README.md.
ARG CARGO_FEATURES=
# Gate the build on the compiler the base image actually activates. The FROM
# digest fixes what is installed and RUSTUP_TOOLCHAIN selects it, but a drift
# there would otherwise ship an artefact built by some other rustc despite the
# expected 1.98.1 builder image.
RUN test "$(rustc --version | awk '{print $2}')" = "1.98.1"
# --mount=type=cache,target=/workspace/target persists compiled artefacts
# across `docker build` runs.  Combined with CARGO_INCREMENTAL=0 + wild,
# only changed crates are relinked.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=ironrag-cargo-reg,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=ironrag-cargo-git,sharing=locked \
    --mount=type=cache,target=/workspace/target,id=ironrag-target,sharing=locked \
    if [ -z "$(printf '%s' "${APP_VERSION}" | tr -d '[:space:]')" ]; then \
      echo "APP_VERSION is blank: this image would report an unidentifiable build at /v1/version." >&2; \
      echo "Pass the release tag or commit, for example:" >&2; \
      echo "  IRONRAG_VERSION=\$(git describe --always --abbrev=40 --dirty --match '')" >&2; \
      echo "  docker compose build backend," >&2; \
      echo "or docker build --build-arg APP_VERSION=<tag-or-commit>." >&2; \
      exit 1; \
    fi \
 && echo "Cache bust: ${CACHEBUST}" \
 && cargo build --locked --release -p ironrag-backend \
      $(if [ -n "${CARGO_FEATURES}" ]; then echo "--features ${CARGO_FEATURES}"; fi) \
      --bin ironrag-backend \
      --bin ironrag-cli \
      --bin ironrag-emit-openapi \
       --bin ironrag-maintenance \
       --bin ironrag-extract \
 && mkdir -p /out \
 && cp target/release/ironrag-backend /out/ \
 && cp target/release/ironrag-cli /out/ \
 && cp target/release/ironrag-emit-openapi /out/ \
  && cp target/release/ironrag-maintenance /out/ \
  && cp target/release/ironrag-extract /out/

# ── Stage 2: native parsing runtime; image recognition uses LLM Vision ──
FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad AS runtime
WORKDIR /app
COPY docker/apt/debian-backports.sources /etc/apt/sources.list.d/debian-backports.sources
COPY docker/apt/90-backports /etc/apt/preferences.d/90-backports
ARG APT_CACHEBUST=20261022
RUN echo "apt cache bust: ${APT_CACHEBUST}" \
    && apt-get update && apt-get upgrade -y \
    && apt-get install -y --no-install-recommends \
        adduser ca-certificates libssl3t64 libstdc++6 util-linux procps \
    && rm -rf /var/lib/apt/lists/* \
    && adduser --disabled-password --gecos "" appuser \
    && mkdir -p /var/lib/ironrag/content-storage \
    && chown -R appuser:appuser /app /var/lib/ironrag
COPY --from=builder /out/ /usr/local/bin/
COPY apps/api/migrations /app/migrations
COPY apps/api/docker/runtime-entrypoint.sh /usr/local/bin/runtime-entrypoint.sh
RUN chmod +x /usr/local/bin/runtime-entrypoint.sh \
    && test -x /bin/kill \
    && ldd /usr/local/bin/ironrag-extract \
    && ! ldd /usr/local/bin/ironrag-extract | grep -q 'not found'
ENV HOME=/home/appuser \
    IRONRAG_XBERG_EXTRACT_BIN=ironrag-extract
USER appuser

# Baked runtime tuning (image default; override per-deployment via env if needed):
#  - _RJEM_MALLOC_CONF: jemalloc background-decay thread returns freed pages to
#    the OS so RSS tracks the live working set. Set here rather than in Rust
#    source because the `malloc_conf` symbol needs `#[export_name]` (unsafe),
#    which the crate's forbid(unsafe_code) lint rejects. tikv-jemallocator reads
#    the `_rjem_`-prefixed name.
#  - TOKIO_WORKER_THREADS: cap the async runtime pool so thread stacks don't
#    scale with host core count on large machines.
#  - RUST_BACKTRACE: full panic backtraces in logs for operator diagnosis.
ENV _RJEM_MALLOC_CONF=background_thread:true,narenas:4,dirty_decay_ms:1000,muzzy_decay_ms:1000 \
    TOKIO_WORKER_THREADS=8 \
    RUST_BACKTRACE=full

EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=5 \
  CMD ["ironrag-cli", "probe", "--endpoint", "health"]
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["ironrag-backend"]
