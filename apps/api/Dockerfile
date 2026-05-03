# syntax=docker/dockerfile:1

FROM rust:1.95.0-trixie AS builder
WORKDIR /workspace
ARG APP_VERSION

RUN apt-get update \
  && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates mold \
  && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY apps/api/Cargo.toml apps/api/build.rs ./apps/api/
COPY apps/api/migrations ./apps/api/migrations
COPY apps/api/contracts ./apps/api/contracts
COPY apps/api/src ./apps/api/src
COPY crates/contracts/Cargo.toml ./crates/contracts/
COPY crates/contracts/src ./crates/contracts/src

ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"
ENV APP_VERSION=${APP_VERSION}

RUN --mount=type=cache,target=/usr/local/cargo/registry,id=ironrag-cargo-reg \
    --mount=type=cache,target=/usr/local/cargo/git,id=ironrag-cargo-git \
    cargo build --locked --release -p ironrag-backend \
      --bin ironrag-backend \
      --bin rebuild_runtime_graph \
      --bin ironrag-cli \
      --bin ironrag-embed-chunks-backfill \
      --bin ironrag-promote-null-heads \
      --bin ironrag-gc-stale-chunks \
      --bin ironrag-backfill-chunk-temporal-bounds

FROM python:3.12-slim-trixie AS runtime
WORKDIR /app
ARG DOCLING_VERSION=2.91.0
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
    tesseract-ocr \
    tesseract-ocr-eng \
    tesseract-ocr-rus \
  && rm -rf /var/lib/apt/lists/*

RUN python -m venv "${VIRTUAL_ENV}" \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir --upgrade pip \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir \
    --index-url https://download.pytorch.org/whl/cpu \
    "torch==${TORCH_VERSION}" \
    "torchvision==${TORCHVISION_VERSION}" \
  && "${VIRTUAL_ENV}/bin/pip" install --no-cache-dir "docling==${DOCLING_VERSION}"

COPY --from=builder /workspace/target/release/ironrag-backend /usr/local/bin/ironrag-backend
COPY --from=builder /workspace/target/release/rebuild_runtime_graph /usr/local/bin/rebuild_runtime_graph
COPY --from=builder /workspace/target/release/ironrag-cli /usr/local/bin/ironrag-cli
COPY --from=builder /workspace/target/release/ironrag-embed-chunks-backfill /usr/local/bin/ironrag-embed-chunks-backfill
COPY --from=builder /workspace/target/release/ironrag-promote-null-heads /usr/local/bin/ironrag-promote-null-heads
COPY --from=builder /workspace/target/release/ironrag-gc-stale-chunks /usr/local/bin/ironrag-gc-stale-chunks
COPY --from=builder /workspace/target/release/ironrag-backfill-chunk-temporal-bounds /usr/local/bin/ironrag-backfill-chunk-temporal-bounds
COPY apps/api/docker/runtime-entrypoint.sh /usr/local/bin/runtime-entrypoint.sh
COPY apps/api/docker/docling-extract.py /usr/local/bin/ironrag-docling-extract

RUN adduser --disabled-password --gecos "" appuser \
  && chmod +x /usr/local/bin/runtime-entrypoint.sh /usr/local/bin/ironrag-docling-extract \
  && mkdir -p /var/lib/ironrag/content-storage \
  && HOME=/home/appuser "${VIRTUAL_ENV}/bin/docling-tools" models download \
  && "${VIRTUAL_ENV}/bin/python" -c 'from pathlib import Path; from PIL import Image, ImageDraw; target = Path("/tmp/docling-ocr-warmup.png"); image = Image.new("RGB", (420, 120), "white"); draw = ImageDraw.Draw(image); draw.text((24, 42), "IronRAG Docling OCR warmup", fill="black"); image.save(target)' \
  && HOME=/home/appuser /usr/local/bin/ironrag-docling-extract /tmp/docling-ocr-warmup.png >/tmp/docling-ocr-warmup.json \
  && rm -f /tmp/docling-ocr-warmup.png /tmp/docling-ocr-warmup.json \
  && chown -R appuser:appuser /app /var/lib/ironrag \
  && chown -R appuser:appuser "${VIRTUAL_ENV}" \
  && chown -R appuser:appuser /home/appuser

ENV HOME=/home/appuser

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["ironrag-backend"]
