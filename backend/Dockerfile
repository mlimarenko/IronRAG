# syntax=docker/dockerfile:1
# cargo-chef + BuildKit cache: dependency layers stay warm, so source-only rebuilds stay fast.
FROM rust:1.94-trixie AS chef
WORKDIR /build
RUN apt-get update \
  && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates mold \
  && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --locked

FROM chef AS planner
WORKDIR /build
COPY backend/Cargo.toml backend/Cargo.lock backend/rust-toolchain.toml backend/build.rs ./
COPY backend/src ./src
COPY backend/migrations ./migrations
COPY backend/contracts ./contracts
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
WORKDIR /build
COPY --from=planner /build/recipe.json recipe.json
COPY backend/Cargo.toml backend/Cargo.lock backend/rust-toolchain.toml backend/build.rs ./
COPY backend/migrations ./migrations
ENV RUSTFLAGS="-C link-arg=-fuse-ld=mold"
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rustrag-cargo-reg \
    --mount=type=cache,target=/usr/local/cargo/git,id=rustrag-cargo-git \
    --mount=type=cache,target=/build/target,id=rustrag-target \
    cargo chef cook --release --recipe-path recipe.json
# cargo-chef rewrites Cargo.toml during cook; restore the real manifest and build script before the final build.
COPY backend/Cargo.toml backend/Cargo.lock backend/rust-toolchain.toml backend/build.rs ./
COPY backend/src ./src
COPY backend/migrations ./migrations
COPY backend/contracts ./contracts
# BuildKit target cache does not become part of the image layer, so copy release binaries into /artifacts for the runtime stage.
RUN --mount=type=cache,target=/usr/local/cargo/registry,id=rustrag-cargo-reg \
    --mount=type=cache,target=/usr/local/cargo/git,id=rustrag-cargo-git \
    --mount=type=cache,target=/build/target,id=rustrag-target \
    cargo clean -p rustrag-backend \
    && cargo build --release --bins \
    && mkdir -p /artifacts \
    && cp /build/target/release/rustrag-backend /build/target/release/rebuild_runtime_graph /artifacts/

FROM debian:trixie-slim AS runtime
WORKDIR /app

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates poppler-utils curl \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /artifacts/rustrag-backend /usr/local/bin/rustrag-backend
COPY --from=builder /artifacts/rebuild_runtime_graph /usr/local/bin/rebuild_runtime_graph
COPY backend/docker/runtime-entrypoint.sh /usr/local/bin/runtime-entrypoint.sh

RUN adduser --disabled-password --gecos "" appuser \
  && chown -R appuser:appuser /app \
  && chmod +x /usr/local/bin/runtime-entrypoint.sh

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["rustrag-backend"]
