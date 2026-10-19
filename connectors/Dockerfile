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

# Build the virtualenv on musl, in the same pinned base all targets run.
FROM python:3.12-alpine@sha256:1b668429b3511ab407d8e00648891631b0b1a4d7e15e3ca70f38ab5b91ad4ab4 AS runtime-base

ARG APP_VERSION
RUN if [ -z "$(printf '%s' "${APP_VERSION}" | tr -d '[:space:]')" ]; then \
        echo "APP_VERSION is blank: this image would report an unidentifiable build at /health." >&2; \
        echo "Pass the release tag, for example --build-arg APP_VERSION=v0.6.0." >&2; \
        exit 1; \
    fi \
    && apk add --no-cache ca-certificates curl

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

RUN addgroup -g 10001 -S connector \
    && adduser -u 10001 -S -D -G connector -H -h /var/lib/ironrag-connector connector \
    && mkdir -p /var/lib/ironrag-connector/repo-cache \
    && chown 10001:10001 /var/lib/ironrag-connector /var/lib/ironrag-connector/repo-cache
USER 10001:10001

VOLUME ["/var/lib/ironrag-connector"]
EXPOSE 8088

HEALTHCHECK --interval=30s --timeout=5s --start-period=15s --retries=3 \
    CMD curl -fsS http://127.0.0.1:${PORT}/health || exit 1

# Only the git source needs the git CLI and HTTPS/SSH remote support.
FROM runtime-base AS vcs-base
USER root
RUN apk add --no-cache git openssh-client
USER 10001:10001

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
