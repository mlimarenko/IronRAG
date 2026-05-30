# syntax=docker/dockerfile:1.7

# ── Stage 1: Rust builder ──────────────────────────────────────────────
FROM rust:1.95.0-trixie AS builder
WORKDIR /workspace
ARG APP_VERSION=0.0.0-dev

# Wild 0.9.0 linker for fast native-speed dockerfile builds (~2x mold on incremental link).
ARG WILD_VERSION=0.9.0
ARG WILD_SHA256=deb6ee0e5caec798053ec4aafaba042e20a8edf91f08cb4d36268571cc628d3b
RUN apt-get update \
  && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates clang curl \
  && curl -fsSL "https://github.com/wild-linker/wild/releases/download/${WILD_VERSION}/wild-linker-${WILD_VERSION}-x86_64-unknown-linux-gnu.tar.gz" -o /tmp/wild.tgz \
  && echo "${WILD_SHA256}  /tmp/wild.tgz" | sha256sum -c - \
  && tar -xzf /tmp/wild.tgz -C /tmp \
  && install -m 0755 "/tmp/wild-linker-${WILD_VERSION}-x86_64-unknown-linux-gnu/wild" /usr/local/bin/wild \
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
# for the toolchain version (kept in lockstep with the FROM tag above).
ENV CARGO_HOME=/usr/local/cargo \
    RUSTUP_TOOLCHAIN=1.95.0 \
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
# --mount=type=cache,target=/workspace/target persists compiled artefacts
# across `docker build` runs.  Combined with CARGO_INCREMENTAL=0 + wild,
# only changed crates are relinked.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=ironrag-cargo-reg,sharing=locked \
    --mount=type=cache,target=/usr/local/cargo/git,id=ironrag-cargo-git,sharing=locked \
    --mount=type=cache,target=/workspace/target,id=ironrag-target,sharing=locked \
    echo "Cache bust: ${CACHEBUST}" \
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
ARG DOCLING_VERSION=2.93.0
ARG TORCH_VERSION=2.11.0+cpu
ARG TORCHVISION_VERSION=0.26.0+cpu
ENV VIRTUAL_ENV=/opt/ironrag-docling
ENV PATH="${VIRTUAL_ENV}/bin:${PATH}"
ENV PYTHONUNBUFFERED=1

RUN apt-get update \
  && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    libgl1 \
    libglib2.0-0 \
    libgomp1 \
  && rm -rf /var/lib/apt/lists/*

RUN python -m venv "${VIRTUAL_ENV}" \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir --upgrade pip \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir \
    --index-url https://download.pytorch.org/whl/cpu \
    "torch==${TORCH_VERSION}" \
    "torchvision==${TORCHVISION_VERSION}" \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir "docling==${DOCLING_VERSION}"

COPY apps/api/docker/docling-extract.py /usr/local/bin/ironrag-docling-extract

RUN chmod +x /usr/local/bin/ironrag-docling-extract

RUN adduser --disabled-password --gecos "" appuser \
  && mkdir -p /var/lib/ironrag/content-storage \
  && HOME=/home/appuser "${VIRTUAL_ENV}/bin/docling-tools" models download \
  && "${VIRTUAL_ENV}/bin/python" -c 'from pathlib import Path; from PIL import Image, ImageDraw; target = Path("/tmp/docling-ocr-warmup.png"); image = Image.new("RGB", (420, 120), "white"); draw = ImageDraw.Draw(image); draw.text((24, 42), "IronRAG Docling OCR warmup", fill="black"); image.save(target)' \
  && HOME=/home/appuser /usr/local/bin/ironrag-docling-extract /tmp/docling-ocr-warmup.png >/tmp/docling-ocr-warmup.json \
  && rm -f /tmp/docling-ocr-warmup.png /tmp/docling-ocr-warmup.json \
  && chown -R appuser:appuser /app /var/lib/ironrag \
  && chown -R appuser:appuser "${VIRTUAL_ENV}" \
  && chown -R appuser:appuser /home/appuser

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

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["ironrag-backend"]
