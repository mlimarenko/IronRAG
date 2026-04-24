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
      --bin ironrag-gc-stale-chunks

FROM debian:trixie-slim AS runtime
WORKDIR /app

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates poppler-utils curl \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /workspace/target/release/ironrag-backend /usr/local/bin/ironrag-backend
COPY --from=builder /workspace/target/release/rebuild_runtime_graph /usr/local/bin/rebuild_runtime_graph
COPY --from=builder /workspace/target/release/ironrag-cli /usr/local/bin/ironrag-cli
COPY --from=builder /workspace/target/release/ironrag-embed-chunks-backfill /usr/local/bin/ironrag-embed-chunks-backfill
COPY --from=builder /workspace/target/release/ironrag-promote-null-heads /usr/local/bin/ironrag-promote-null-heads
COPY --from=builder /workspace/target/release/ironrag-gc-stale-chunks /usr/local/bin/ironrag-gc-stale-chunks
COPY apps/api/docker/runtime-entrypoint.sh /usr/local/bin/runtime-entrypoint.sh

RUN adduser --disabled-password --gecos "" appuser \
  && mkdir -p /var/lib/ironrag/content-storage \
  && chown -R appuser:appuser /app /var/lib/ironrag \
  && chmod +x /usr/local/bin/runtime-entrypoint.sh

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["ironrag-backend"]
