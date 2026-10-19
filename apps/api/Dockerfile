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
 && cargo build --release -p ironrag-backend \
      $(if [ -n "${CARGO_FEATURES}" ]; then echo "--features ${CARGO_FEATURES}"; fi) \
      --bin ironrag-backend \
      --bin ironrag-cli \
      --bin ironrag-emit-openapi \
      --bin ironrag-maintenance \
 && mkdir -p /out \
 && cp target/release/ironrag-backend /out/ \
 && cp target/release/ironrag-cli /out/ \
 && cp target/release/ironrag-emit-openapi /out/ \
 && cp target/release/ironrag-maintenance /out/

# ── Stage 2: Docling / Python base ─────────────────────────────────────
# Isolated from the Rust builder so this stage is cached across
# application-code changes without re-downloading model weights.
FROM python:3.14.4-slim-trixie AS docling-base
WORKDIR /app
ARG DOCLING_VERSION=2.132.0
ARG URLLIB3_VERSION=2.8.0
ARG TESSDATA_REVISION=e12c65a915945e4c28e237a9b52bc4a8f39a0cec
ARG CHINESE_TESSDATA_SHA256=4fef2d1306c8e87616d4d3e4c6c67faf5d44be3342290cf8f2f0f6e3aa7e735b
ARG TESSDATA_LICENSE_SHA256=a6cba85bc92e0cff7a450b1d873c0eaa2e9fc96bf472df0247a26bec77bf3ff9
ARG TESSDATA_README_SHA256=08c87523fb799c5bae6380f6ffa1a823494ab78f63906d7255dddd7671f94a5f
ARG TORCH_VERSION=2.12.0+cpu
ARG TORCHVISION_VERSION=0.27.0+cpu
ENV VIRTUAL_ENV=/opt/ironrag-docling
ENV PATH="${VIRTUAL_ENV}/bin:${PATH}"
ENV PYTHONUNBUFFERED=1

COPY docker/apt/debian-backports.sources /etc/apt/sources.list.d/debian-backports.sources
COPY docker/apt/90-backports /etc/apt/preferences.d/90-backports
# The apt layer is this stage's cache boundary: BuildKit registry cache keeps
# `apt-get upgrade` from re-running, so Debian security updates published after
# the layer was first built never reach the image. Bump APT_CACHEBUST to force
# the apt layer (and everything after it) to rebuild with current package
# versions — the same manual pattern as CACHEBUST in the builder stage.
ARG APT_CACHEBUST=20261001
# Use the accuracy model for Chinese; the distribution's fast model loses
# characters in sparse picture text. Ship its upstream license declaration and
# provenance alongside the only installed Chinese model.
RUN echo "apt cache bust: ${APT_CACHEBUST}" \
  && apt-get update \
  && apt-get upgrade -y \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libgl1 \
    libglib2.0-0 \
    libgomp1 \
    tesseract-ocr \
    tesseract-ocr-eng \
    tesseract-ocr-rus \
  && rm -rf /var/lib/apt/lists/* \
  && python -m venv "${VIRTUAL_ENV}" \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir --upgrade pip \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir \
    --index-url https://download.pytorch.org/whl/cpu \
    --extra-index-url https://pypi.org/simple \
    "torch==${TORCH_VERSION}" \
    "torchvision==${TORCHVISION_VERSION}" \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir \
    "docling==${DOCLING_VERSION}" \
    "urllib3==${URLLIB3_VERSION}" \
  && "${VIRTUAL_ENV}/bin/python" -m pip uninstall --yes pip \
  && /usr/local/bin/python -m pip uninstall --yes pip \
  && "${VIRTUAL_ENV}/bin/python" -c 'import importlib.util; assert importlib.util.find_spec("pip") is None' \
  && /usr/local/bin/python -c 'import importlib.util; assert importlib.util.find_spec("pip") is None' \
  && curl --fail --location --max-time 120 \
      "https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/${TESSDATA_REVISION}/chi_sim.traineddata" \
      --output /usr/share/tesseract-ocr/5/tessdata/chi_sim.traineddata \
  && mkdir -p /usr/share/doc/tessdata_best \
  && curl --fail --location --max-time 120 \
      "https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/${TESSDATA_REVISION}/LICENSE" \
      --output /usr/share/doc/tessdata_best/LICENSE \
  && curl --fail --location --max-time 120 \
      "https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/${TESSDATA_REVISION}/README.md" \
      --output /usr/share/doc/tessdata_best/README.md \
  && printf '%s  %s\n' \
      "${CHINESE_TESSDATA_SHA256}" /usr/share/tesseract-ocr/5/tessdata/chi_sim.traineddata \
      "${TESSDATA_LICENSE_SHA256}" /usr/share/doc/tessdata_best/LICENSE \
      "${TESSDATA_README_SHA256}" /usr/share/doc/tessdata_best/README.md \
      | sha256sum --check \
  && printf '%s\n' \
      'Model: chi_sim.traineddata (unmodified)' \
      'Upstream: https://github.com/tesseract-ocr/tessdata_best' \
      "Revision: ${TESSDATA_REVISION}" \
      "Model SHA256: ${CHINESE_TESSDATA_SHA256}" \
      'License: Apache-2.0; see LICENSE and the upstream README.md declaration' \
      > /usr/share/doc/tessdata_best/MODEL

# Package installation is finished. Both interpreters are installer-free before
# model download and OCR warmup prove that the runtime does not depend on pip.

COPY apps/api/docker/docling-extract.py /usr/local/bin/ironrag-docling-extract

RUN chmod +x /usr/local/bin/ironrag-docling-extract \
  && adduser --disabled-password --gecos "" appuser \
  && mkdir -p /var/lib/ironrag/content-storage \
  && HOME=/home/appuser "${VIRTUAL_ENV}/bin/docling-tools" models download layout tableformer \
  && printf '%s\n' \
    'from pathlib import Path' \
    'from PIL import Image, ImageDraw' \
    'target = Path("/tmp/docling-ocr-warmup.png")' \
    'image = Image.new("RGB", (420, 120), "white")' \
    'draw = ImageDraw.Draw(image)' \
    'draw.text((24, 42), "IronRAG Docling OCR warmup", fill="black")' \
    'image.save(target)' \
    | "${VIRTUAL_ENV}/bin/python" \
  && HOME=/home/appuser /usr/local/bin/ironrag-docling-extract /tmp/docling-ocr-warmup.png >/tmp/docling-ocr-warmup.json \
  && rm -f /tmp/docling-ocr-warmup.png /tmp/docling-ocr-warmup.json \
  && chown -R appuser:appuser /app /var/lib/ironrag \
  && chown -R appuser:appuser "${VIRTUAL_ENV}" \
  && chown -R appuser:appuser /home/appuser

# Cached Docling model cards carry their license declarations. TableFormer uses
# CDLA-Permissive-2.0; include its agreement text alongside the native Apache text.
COPY apps/api/docker/model-licenses/CDLA-Permissive-2.0.txt \
     /usr/share/common-licenses/CDLA-Permissive-2.0

# ── Stage 3: Runtime assembly ──────────────────────────────────────────
FROM docling-base AS runtime

COPY --from=builder /out/ironrag-backend /usr/local/bin/ironrag-backend
COPY --from=builder /out/ironrag-cli /usr/local/bin/ironrag-cli
COPY --from=builder /out/ironrag-emit-openapi /usr/local/bin/ironrag-emit-openapi
COPY --from=builder /out/ironrag-maintenance /usr/local/bin/ironrag-maintenance
COPY apps/api/migrations /app/migrations
COPY apps/api/docker/runtime-entrypoint.sh /usr/local/bin/runtime-entrypoint.sh
COPY apps/api/docker/docling-extract.py /usr/local/bin/ironrag-docling-extract

RUN chmod +x /usr/local/bin/runtime-entrypoint.sh /usr/local/bin/ironrag-docling-extract

ENV HOME=/home/appuser
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
  CMD curl -fsS http://127.0.0.1:8080/v1/health >/dev/null || exit 1
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["ironrag-backend"]
