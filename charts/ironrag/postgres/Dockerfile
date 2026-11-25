FROM gitlab.piping.space:4567/piping-space/infrastructure/argocd/postgres@sha256:fcf1c9c875fb5f8c3b907041a4746ddea06454f921f31d0570c5715c65c46fd7 AS gosu

FROM docker.io/pgvector/pgvector@sha256:89879f41d564f67b0c1462a491d16fcbc6de39ac10db17e7a4bba2138a5e0ba5 AS postgres

ARG PERL_VERSION=5.36.0-7+deb12u4
ARG APT_VERSION=2.6.2
ARG OPENSSL_VERSION=3.0.22-1~deb12u2
ARG LIBPCRE2_VERSION=10.42-1+deb12u2

COPY --from=gosu /usr/local/bin/gosu /usr/local/bin/gosu

RUN set -eu; \
    echo '871a71838ac38dff37fefec117ac12919538602a12503588870f36c58b7f50b5  /usr/local/bin/gosu' \
        | sha256sum -c -; \
    gosu_build="$(gosu --version)"; \
    case "$gosu_build" in \
        '1.19 (go1.27.4 on linux/amd64; gc)'*) ;; \
        *) echo "unexpected gosu build: $gosu_build" >&2; exit 1 ;; \
    esac; \
    dpkg-query -W -f='${Package}\t${Architecture}\t${Version}\n' > /tmp/packages.before; \
    LC_ALL=C sort -o /tmp/packages.before /tmp/packages.before; \
    sha256sum /usr/lib/postgresql/19/bin/postgres \
        /usr/lib/postgresql/19/lib/vector.so \
        /usr/share/postgresql/19/extension/vector.control \
        /usr/lib/x86_64-linux-gnu/libpq.so.5 > /tmp/runtime.before; \
    apt-get update; \
    apt-get install -y --no-install-recommends --only-upgrade \
        "perl=${PERL_VERSION}" "perl-base=${PERL_VERSION}" \
        "perl-modules-5.36=${PERL_VERSION}" "libperl5.36=${PERL_VERSION}" \
        "apt=${APT_VERSION}" "libapt-pkg6.0=${APT_VERSION}" \
        "openssl=${OPENSSL_VERSION}" "libssl3=${OPENSSL_VERSION}" \
        "libpcre2-8-0=${LIBPCRE2_VERSION}"; \
    for pinned in \
        "perl=${PERL_VERSION}" "perl-base=${PERL_VERSION}" \
        "perl-modules-5.36=${PERL_VERSION}" "libperl5.36=${PERL_VERSION}" \
        "apt=${APT_VERSION}" "libapt-pkg6.0=${APT_VERSION}" \
        "openssl=${OPENSSL_VERSION}" "libssl3=${OPENSSL_VERSION}" \
        "libpcre2-8-0=${LIBPCRE2_VERSION}"; do \
        package="${pinned%%=*}"; \
        expected="${pinned#*=}"; \
        test "$(dpkg-query -W -f='${db:Status-Status}' "$package")" = installed; \
        installed="$(dpkg-query -W -f='${Version}' "$package")"; \
        dpkg --compare-versions "$installed" eq "$expected"; \
        dpkg --compare-versions "$installed" ge "$expected"; \
    done; \
    apt-get purge -y \
        gnupg gpg gpg-wks-client gpg-wks-server libsqlite3-0; \
    for removed in gnupg gpg gpg-wks-client gpg-wks-server libsqlite3-0; do \
        test "$(dpkg-query -W -f='${db:Status-Status}' "$removed" 2>/dev/null || true)" != installed; \
    done; \
    dpkg-query -W -f='${Package}\t${Architecture}\t${Version}\n' > /tmp/packages.after; \
    LC_ALL=C sort -o /tmp/packages.after /tmp/packages.after; \
    for snapshot in before after; do \
        awk '$1 != "perl" && $1 != "perl-base" && $1 != "perl-modules-5.36" && \
             $1 != "libperl5.36" && $1 != "apt" && $1 != "libapt-pkg6.0" && \
             $1 != "openssl" && $1 != "libssl3" && $1 != "libpcre2-8-0" && \
             $1 != "gnupg" && $1 != "gpg" && $1 != "gpg-wks-client" && \
             $1 != "gpg-wks-server" && $1 != "libsqlite3-0"' \
            "/tmp/packages.${snapshot}" > "/tmp/protected.${snapshot}"; \
    done; \
    cmp /tmp/protected.before /tmp/protected.after; \
    sha256sum -c /tmp/runtime.before; \
    case "$(postgres --version)" in \
        'postgres (PostgreSQL) 19.0'|'postgres (PostgreSQL) 19.0 '*) ;; \
        *) exit 1 ;; \
    esac; \
    grep -Eq "^default_version[[:space:]]*=[[:space:]]*'0[.]8[.]7'[[:space:]]*$" \
        /usr/share/postgresql/19/extension/vector.control; \
    dpkg-query -W -f='${Package}=${Version}\n' \
        perl perl-base perl-modules-5.36 libperl5.36 \
        apt libapt-pkg6.0 openssl libssl3 libpcre2-8-0; \
    rm -rf /var/lib/apt/lists/*; \
    rm -f /tmp/packages.before /tmp/packages.after /tmp/protected.before /tmp/protected.after /tmp/runtime.before

ARG CI_COMMIT_SHA
RUN case "$CI_COMMIT_SHA" in ''|*[!0-9a-f]*) exit 1 ;; esac \
    && case "${#CI_COMMIT_SHA}" in 40|64) ;; *) exit 1 ;; esac

LABEL org.opencontainers.image.source="https://github.com/mlimarenko/IronRAG" \
      org.opencontainers.image.revision=$CI_COMMIT_SHA

USER 999:999
