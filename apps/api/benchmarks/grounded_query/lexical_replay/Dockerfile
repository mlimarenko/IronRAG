FROM postgres@sha256:edb0c08dc33befd0262ab3b5cf1c4f3f8cfd8b09e88c05b45adee29d6761ddb6
LABEL experiment="IRG-285" scope="local-synthetic-only"
ADD --checksum=sha256:3aa15d28b87e336a3dd22fce1bb2e4d0e2958560b92adfdf5cb4d466288da95f https://github.com/paradedb/paradedb/releases/download/v0.26.2/postgresql-18-pg-search_0.26.2-1PARADEDB-trixie_amd64.deb /opt/irg-285-packages/pg-search.deb
RUN apt-get update \
    && apt-get download postgresql-18-pgvector=0.8.7-1.pgdg13+1 \
    && echo 'd9923b9ea81375604d88bfcfa167d58bfaf3de5689372c9e4504244f1671f596  postgresql-18-pgvector_0.8.7-1.pgdg13+1_amd64.deb' | sha256sum -c - \
    && mv postgresql-18-pgvector_0.8.7-1.pgdg13+1_amd64.deb /opt/irg-285-packages/ \
    && apt-get install -y --no-install-recommends /opt/irg-285-packages/*.deb \
    && dpkg-query -W postgresql-18 postgresql-18-pgvector postgresql-18-pg-search
