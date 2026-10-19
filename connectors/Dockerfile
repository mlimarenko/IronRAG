# One image definition for every connector.
#
# Each source is a build target that differs only in which source the entry
# point runs; `gitrepos` additionally needs the git CLI, which is the one real
# per-source system dependency. Build from this directory:
#
#     docker build --target bookstack --build-arg APP_VERSION=v0.6.0 \
#         -t pipingspace/ironrag.bookstack:v0.6.0 connectors
#
# The framework used to be checked out from a companion repository and staged
# into the build context; it is in this tree now, so there is nothing to pin
# and nothing to stage.

FROM python:3.12-slim-trixie AS runtime-base

ARG APP_VERSION
RUN if [ -z "$(printf '%s' "${APP_VERSION}" | tr -d '[:space:]')" ]; then \
        echo "APP_VERSION is blank: this image would report an unidentifiable build at /health." >&2; \
        echo "Pass the release tag, for example --build-arg APP_VERSION=v0.6.0." >&2; \
        exit 1; \
    fi \
    && apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        libpcre2-8-0=10.46-1~deb13u3 \
    && rm -rf /var/lib/apt/lists/*

COPY --from=ghcr.io/astral-sh/uv:0.9.22 /uv /usr/local/bin/uv

WORKDIR /app
COPY pyproject.toml uv.lock README.md ./
COPY src/ ./src/
RUN uv sync --frozen --no-dev --no-editable

ENV PATH=/app/.venv/bin:$PATH \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    HOST=0.0.0.0 \
    PORT=8088 \
    LOG_LEVEL=info \
    ROUTING_CONFIG_PATH=/app/routing.yaml \
    STATE_DB_PATH=/var/lib/ironrag-connector/state.sqlite \
    IRONRAG_CONNECTOR_VERSION=${APP_VERSION} \
    # The account's home is the state volume: it is the only writable place, it
    # is where ssh looks for a deploy key (ssh reads passwd, not $HOME), and it
    # is where git wants to write its global config.
    HOME=/var/lib/ironrag-connector

# Three of the four connectors ran as root. One base means one place to fix it.
RUN groupadd --gid 10001 connector \
    && useradd --uid 10001 --gid 10001 --no-create-home \
        --home-dir /var/lib/ironrag-connector connector \
    && install -d -o 10001 -g 10001 /var/lib/ironrag-connector \
    && install -d -o 10001 -g 10001 /var/lib/ironrag-connector/repo-cache
USER 10001:10001

VOLUME ["/var/lib/ironrag-connector"]
EXPOSE 8088

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:${PORT}/health || exit 1

# The git source shells out to the git CLI and reaches private remotes over
# HTTPS and SSH. It is also the only target that needs that CLI, so it is the
# one target that does not build on `runtime-base`: the Debian base carries an
# unfixed util-linux/perl/ncurses/libexpat baseline the image cannot trim, and a
# scan of the built image reported no fixed version for any of its findings.
# This stage builds the same application on Alpine (musl) with the git and ssh
# tooling. The virtualenv is created HERE, inside the Alpine image: a venv built
# against glibc cannot be copied into a musl image. ENV vocabulary, non-root
# UID, state directory, port, healthcheck and version wiring stay identical to
# `runtime-base`; the other three targets keep using it.
FROM python:3.12-alpine@sha256:1b668429b3511ab407d8e00648891631b0b1a4d7e15e3ca70f38ab5b91ad4ab4 AS vcs-base

ARG APP_VERSION
RUN if [ -z "$(printf '%s' "${APP_VERSION}" | tr -d '[:space:]')" ]; then \
        echo "APP_VERSION is blank: this image would report an unidentifiable build at /health." >&2; \
        echo "Pass the release tag, for example --build-arg APP_VERSION=v0.6.0." >&2; \
        exit 1; \
    fi \
    && apk add --no-cache ca-certificates curl git openssh-client

COPY --from=ghcr.io/astral-sh/uv:0.9.22 /uv /usr/local/bin/uv

WORKDIR /app
COPY pyproject.toml uv.lock README.md ./
COPY src/ ./src/
RUN uv sync --frozen --no-dev --no-editable

ENV PATH=/app/.venv/bin:$PATH \
    PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    HOST=0.0.0.0 \
    PORT=8088 \
    LOG_LEVEL=info \
    ROUTING_CONFIG_PATH=/app/routing.yaml \
    STATE_DB_PATH=/var/lib/ironrag-connector/state.sqlite \
    IRONRAG_CONNECTOR_VERSION=${APP_VERSION} \
    HOME=/var/lib/ironrag-connector

RUN addgroup -g 10001 -S connector \
    && adduser -u 10001 -S -D -G connector -H -h /var/lib/ironrag-connector connector \
    && mkdir -p /var/lib/ironrag-connector/repo-cache \
    && chown 10001:10001 /var/lib/ironrag-connector /var/lib/ironrag-connector/repo-cache
USER 10001:10001

VOLUME ["/var/lib/ironrag-connector"]
EXPOSE 8088

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:${PORT}/health || exit 1

# Where clones land. The code's default is a relative path, which resolved to
# the working directory — writable when the container ran as root, not writable
# now. The old image set this too; dropping it would have made the image fail on
# its first sweep rather than at start-up.
ENV GITREPOS_CLONE_ROOT=/var/lib/ironrag-connector/repo-cache \
    GITREPOS_CONFIG_PATH=/app/repos.yaml

FROM runtime-base AS bookstack
CMD ["ironrag-connector", "bookstack"]

FROM runtime-base AS confluence
CMD ["ironrag-connector", "confluence"]

FROM runtime-base AS youtrack
CMD ["ironrag-connector", "youtrack"]

FROM vcs-base AS gitrepos
CMD ["ironrag-connector", "gitrepos"]
