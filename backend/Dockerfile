FROM rust:1.94-trixie AS builder
WORKDIR /build

RUN apt-get update \
  && apt-get install -y --no-install-recommends pkg-config libssl-dev ca-certificates \
  && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock* ./
COPY src ./src
COPY migrations ./migrations
COPY contracts ./contracts
COPY rust-toolchain.toml ./rust-toolchain.toml
COPY clippy.toml ./clippy.toml

RUN cargo build --release --bins

FROM debian:trixie-slim AS runtime
WORKDIR /app

RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates poppler-utils curl \
  && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/rustrag-backend /usr/local/bin/rustrag-backend
COPY --from=builder /build/target/release/rebuild_runtime_graph /usr/local/bin/rebuild_runtime_graph
COPY docker/runtime-entrypoint.sh /usr/local/bin/runtime-entrypoint.sh

RUN adduser --disabled-password --gecos "" appuser \
  && chown -R appuser:appuser /app \
  && chmod +x /usr/local/bin/runtime-entrypoint.sh

EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/runtime-entrypoint.sh"]
CMD ["rustrag-backend"]
