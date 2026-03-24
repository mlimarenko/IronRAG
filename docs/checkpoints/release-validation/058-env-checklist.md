# 058 Environment Checklist

- [ ] Docker Compose services are healthy (`postgres`, `redis`, `arangodb`, `backend`, `frontend`)
- [ ] Target library UUID is known and readable
- [ ] Admin/operator account can login via `/v1/iam/session/login`
- [ ] Active runtime binding exists for `extract_graph`
- [ ] Active runtime binding exists for `embed_chunk`
- [ ] Vision path is configured (explicit `vision` binding or accepted fallback policy)
- [ ] Billing endpoints are reachable
- [ ] MCP capabilities endpoint is reachable
