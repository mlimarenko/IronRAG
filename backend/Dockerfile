FROM debian:trixie-slim
WORKDIR /app
RUN apt-get update \
  && apt-get install -y --no-install-recommends ca-certificates \
  && rm -rf /var/lib/apt/lists/*
COPY target/release/rustrag-backend /usr/local/bin/rustrag-backend
EXPOSE 8080
CMD ["rustrag-backend"]
