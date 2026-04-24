#!/usr/bin/env bash
# Fast diagnostic of lexical BM25 retrieval + backend telemetry for one
# query against a chosen library on a remote IronRAG host. Used to
# diagnose retrieval-quality misses (wrong docs in top-k, vector lane
# returning 0, BM25 stem collisions, …).
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

if [[ -z "${LIBRARY_ID}" ]]; then
  echo "error: library_id is required via 3rd arg or LIBRARY_ID env var" >&2
  exit 1
fi

ARANGO_PASS="$(ssh -n "${HOST}" "docker exec ironrag-backend-1 printenv IRONRAG_ARANGODB_PASSWORD")"

echo "================================================================"
echo "diag «${QUERY}» library=${LIBRARY_ID:0:13} top=${LIMIT}"
echo "================================================================"

ssh -n "${HOST}" "docker exec ironrag-arangodb-1 arangosh \
  --server.username root \
  --server.password ${ARANGO_PASS} \
  --server.database ironrag \
  --javascript.execute-string 'var tokens = db._query(\`RETURN TOKENS(\\\"${QUERY}\\\", \\\"text_ru\\\")\`).toArray()[0]; print(\"tokens:    \" + JSON.stringify(tokens));' 2>&1" | tail -3

echo
echo "--- lexical BM25 top-${LIMIT} (text_ru analyzer + PHRASE) ---"
ssh -n "${HOST}" "docker exec ironrag-arangodb-1 arangosh \
  --server.username root \
  --server.password ${ARANGO_PASS} \
  --server.database ironrag \
  --javascript.execute-string 'var cursor = db._query(\`FOR doc IN knowledge_search_view SEARCH doc.library_id == \\\"${LIBRARY_ID}\\\" AND doc.chunk_id != null AND doc.chunk_state == \\\"ready\\\" AND ( ANALYZER(doc.normalized_text IN TOKENS(\\\"${QUERY}\\\", \\\"text_ru\\\"), \\\"text_ru\\\") OR ANALYZER(PHRASE(doc.normalized_text, \\\"${QUERY}\\\", \\\"text_ru\\\"), \\\"text_ru\\\") ) LET s = BM25(doc) SORT s DESC LIMIT ${LIMIT} RETURN {chunk: SUBSTRING(doc.chunk_id, 0, 8), doc: SUBSTRING(doc.document_id, 0, 8), ix: doc.chunk_index, score: ROUND(s*100)/100, snippet: SUBSTRING(doc.normalized_text, 0, 140)}\`).toArray(); cursor.forEach(function(r){ print(\" \" + r.score + \"  doc=\" + r.doc + \" ix=\" + r.ix + \": \" + r.snippet.replace(/\\\\n/g, \" | \").substring(0,120)); });' 2>&1" | tail -${LIMIT}

echo
echo "--- distinct docs with ALL tokens-matching chunks ---"
ssh -n "${HOST}" "docker exec ironrag-arangodb-1 arangosh \
  --server.username root \
  --server.password ${ARANGO_PASS} \
  --server.database ironrag \
  --javascript.execute-string 'var cursor = db._query(\`FOR c IN knowledge_search_view SEARCH c.library_id == \\\"${LIBRARY_ID}\\\" AND c.chunk_id != null AND ANALYZER(c.normalized_text IN TOKENS(\\\"${QUERY}\\\", \\\"text_ru\\\"), \\\"text_ru\\\") COLLECT doc_id = c.document_id WITH COUNT INTO n SORT n DESC LIMIT 12 RETURN {doc: SUBSTRING(doc_id, 0, 8), n: n}\`).toArray(); cursor.forEach(function(r){ print(\"   doc=\" + r.doc + \" chunks=\" + r.n); });' 2>&1" | tail -13
