# 058 US2 Evidence (Graph Quality and Meaningfulness)

## Source run
- Run ID: `20260324T093938Z-43695`
- Report JSON: `/tmp/rustrag-release-validation/20260324T093938Z-43695/artifacts/release-validation-report.json`

## Graph metrics
- Entities: 110
- Relations: 158
- Matched semantic terms: acme, beta, berlin, rustrag, budget 2026
- Semantic pass: true

## Search validation
- Direct search (`/knowledge/.../search/documents`): status 200, all 5 semantic terms matched
- MCP search (`search_documents`): passed, all 5 semantic terms matched

## Verdict contribution
- Graph semantic quality: pass
- No workspace/library isolation violations detected.
