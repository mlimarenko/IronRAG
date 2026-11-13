FROM docker.io/pgvector/pgvector@sha256:2358fcba361ed2233a5ed81b5fe4ca779ccb304120ce531a3bf51c0ed7e2bc11 AS postgres

ARG PERL_VERSION=5.36.0-7+deb12u4

RUN set -eu; \
    dpkg-query -W -f='${Package}\t${Architecture}\t${Version}\n' > /tmp/packages.before; \
    LC_ALL=C sort -o /tmp/packages.before /tmp/packages.before; \
    sha256sum /usr/lib/postgresql/18/bin/postgres \
        /usr/lib/postgresql/18/lib/vector.so \
        /usr/share/postgresql/18/extension/vector.control \
        /usr/lib/x86_64-linux-gnu/libpq.so.5 > /tmp/runtime.before; \
    apt-get update; \
    apt-get install -y --no-install-recommends --only-upgrade \
        "perl=${PERL_VERSION}" "perl-base=${PERL_VERSION}" \
        "perl-modules-5.36=${PERL_VERSION}" "libperl5.36=${PERL_VERSION}"; \
    for package in perl perl-base perl-modules-5.36 libperl5.36; do \
        test "$(dpkg-query -W -f='${db:Status-Status}' "$package")" = installed; \
        installed="$(dpkg-query -W -f='${Version}' "$package")"; \
        dpkg --compare-versions "$installed" eq "$PERL_VERSION"; \
        dpkg --compare-versions "$installed" ge '5.36.0-7+deb12u4'; \
    done; \
    dpkg-query -W -f='${Package}\t${Architecture}\t${Version}\n' > /tmp/packages.after; \
    LC_ALL=C sort -o /tmp/packages.after /tmp/packages.after; \
    for snapshot in before after; do \
        awk '$1 != "perl" && $1 != "perl-base" && $1 != "perl-modules-5.36" && $1 != "libperl5.36"' \
            "/tmp/packages.${snapshot}" > "/tmp/protected.${snapshot}"; \
    done; \
    cmp /tmp/protected.before /tmp/protected.after; \
    sha256sum -c /tmp/runtime.before; \
    case "$(postgres --version)" in \
        'postgres (PostgreSQL) 18.6'|'postgres (PostgreSQL) 18.6 '*) ;; \
        *) exit 1 ;; \
    esac; \
    grep -Eq "^default_version[[:space:]]*=[[:space:]]*'0[.]8[.]7'[[:space:]]*$" \
        /usr/share/postgresql/18/extension/vector.control; \
    dpkg-query -W -f='${Package}=${Version}\n' perl perl-base perl-modules-5.36 libperl5.36; \
    rm -rf /var/lib/apt/lists/*; \
    rm -f /tmp/packages.before /tmp/packages.after /tmp/protected.before /tmp/protected.after /tmp/runtime.before

ARG CI_COMMIT_SHA
RUN case "$CI_COMMIT_SHA" in ''|*[!0-9a-f]*) exit 1 ;; esac \
    && case "${#CI_COMMIT_SHA}" in 40|64) ;; *) exit 1 ;; esac

LABEL org.opencontainers.image.source="https://github.com/mlimarenko/IronRAG" \
      org.opencontainers.image.revision=$CI_COMMIT_SHA
