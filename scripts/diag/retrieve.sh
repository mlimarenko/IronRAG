#!/usr/bin/env bash
# Fast diagnostic of PostgreSQL FTS lexical retrieval for one
# query against a chosen library on a remote IronRAG host. Used to
# diagnose retrieval-quality misses (wrong docs in top-k, vector lane
# returning 0, lexical rank collisions, ...).
#
# Usage:
#   HOST=<remote-host> LIBRARY_ID=<uuid> scripts/diag/retrieve.sh "query text"
#   HOST=<remote-host> scripts/diag/retrieve.sh "query text" 20 <library_uuid>
#
# Requires: HOST env var set to an SSH-reachable host running the
# IronRAG docker-compose stack.

set -euo pipefail

QUERY="${1:?usage: $0 <query> [limit] [library_id]}"
LIMIT="${2:-10}"
LIBRARY_ID="${3:-${LIBRARY_ID:-}}"
HOST="${HOST:?HOST env var is required (SSH-reachable IronRAG host)}"

if [[ ! "${LIMIT}" =~ ^[0-9]+$ ]] || [[ "${LIMIT}" -lt 1 ]]; then
  echo "error: limit must be a positive integer" >&2
  exit 1
fi

if [[ -z "${LIBRARY_ID}" ]]; then
  echo "error: library_id is required via 3rd arg or LIBRARY_ID env var" >&2
  exit 1
fi

if [[ ! "${LIBRARY_ID}" =~ ^[0-9a-fA-F-]{36}$ ]]; then
  echo "error: library_id must be a UUID" >&2
  exit 1
fi

QUERY_B64="$(printf '%s' "${QUERY}" | base64 | tr -d '\n')"

run_psql() {
  ssh "${HOST}" "docker exec -i ironrag-postgres-1 sh -c 'export PGPASSWORD=\"\$POSTGRES_PASSWORD\"; exec psql -v ON_ERROR_STOP=1 -U \"\$POSTGRES_USER\" -d \"\$POSTGRES_DB\" --set=query_b64=\"${QUERY_B64}\" --set=library_id=\"${LIBRARY_ID}\" --set=limit=\"${LIMIT}\"'"
}

echo "================================================================"
echo "diag «${QUERY}» library=${LIBRARY_ID:0:13} top=${LIMIT}"
echo "================================================================"

run_psql <<'SQL'
\pset tuples_only on
\pset format unaligned
with params as (
  select convert_from(decode(:'query_b64', 'base64'), 'UTF8') as query_text
)
select 'fts query: ' || websearch_to_tsquery('simple', ironrag_unaccent(query_text))::text
from params;
SQL

echo
echo "--- lexical FTS top-${LIMIT} (knowledge_chunk.search_tsv + ts_rank_cd) ---"
run_psql <<'SQL'
\pset tuples_only on
\pset format unaligned
with params as (
  select
    convert_from(decode(:'query_b64', 'base64'), 'UTF8') as query_text,
    :'library_id'::uuid as library_id,
    :limit::integer as result_limit
),
query as (
  select
    library_id,
    result_limit,
    websearch_to_tsquery('simple', ironrag_unaccent(query_text)) as tsq
  from params
)
select format(
  ' %s  doc=%s ix=%s: %s',
  round(ts_rank_cd(c.search_tsv, q.tsq)::numeric, 4),
  left(c.document_id::text, 8),
  c.chunk_index,
  left(replace(coalesce(c.normalized_text, c.content_text), E'\n', ' | '), 120)
)
from query q
join knowledge_chunk c
  on c.library_id = q.library_id
where c.chunk_state = 'ready'
  and q.tsq @@ c.search_tsv
order by ts_rank_cd(c.search_tsv, q.tsq) desc, c.chunk_index asc
limit (select result_limit from params);
SQL

echo
echo "--- distinct docs with ALL tokens-matching chunks ---"
run_psql <<'SQL'
\pset tuples_only on
\pset format unaligned
with params as (
  select
    convert_from(decode(:'query_b64', 'base64'), 'UTF8') as query_text,
    :'library_id'::uuid as library_id
),
query as (
  select
    library_id,
    plainto_tsquery('simple', ironrag_unaccent(query_text)) as tsq
  from params
)
select format('   doc=%s chunks=%s', left(c.document_id::text, 8), count(*))
from query q
join knowledge_chunk c
  on c.library_id = q.library_id
where c.chunk_state = 'ready'
  and q.tsq @@ c.search_tsv
group by c.document_id
order by count(*) desc, c.document_id
limit 12;
SQL
