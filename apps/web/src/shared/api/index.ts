// SDK surface — generated from apps/api/contracts/openapi.gen.yaml
// via @hey-api/openapi-ts. Re-export the tag classes so call sites can
// `import { Catalog } from '@/shared/api'` without reaching into the generated/
// sub-tree (an ESLint boundary rule will be added with the feature-folder
// migration in a later sprint).
export { Catalog } from './generated'
export { ApiError, getApiProblem, unwrap } from './runtime'

// Generated TanStack Query 5 query/mutation options + key factories for
// server-state hooks.
export * as queries from './generated/@tanstack/react-query.gen'

// UI API facades. Feature modules import these domain facades instead
// of depending on generated operation names directly.
export { authApi } from './auth'
export type {
  CatalogLibraryResponse,
  DashboardSurface,
  DocumentLifecycleDetail,
  WebBoundaryPolicy,
  WebIngestMode,
  WebIngestPattern,
} from './generated'
export {
  documentsApi,
  documentListQuery,
  documentFacetsInfiniteOptions,
  librarySnapshotApi,
} from './documents'
export { auditApi, auditFacetQuery, auditFacetsInfiniteOptions, auditListQuery } from './audit'
export type {
  AuditColumnFilters,
  AuditEventPage,
  AuditFacetParams,
  AuditFacetsResponse,
  AuditFacetValue,
  AuditGridColumnId,
  AuditListParams,
  AuditSearchHints,
  AuditSelectedValue,
  AuditSortOrder,
} from './audit'
export { adminLibraryFacetsInfiniteOptions, adminLibraryListQuery } from './libraries'
export type { AdminLibraryFacetParams, AdminLibraryListParams } from './libraries'
export type {
  AdminLibraryColumnFilters,
  AdminLibraryFacetColumn,
  AdminLibraryFacetsResponse,
  AdminLibrarySearchHints,
} from './libraries'
export type {
  DocumentColumnFilters,
  DocumentFacetColumn,
  DocumentFacetParams,
  DocumentFacetRawValue,
  DocumentFacetsResponse,
  DocumentFacetValue,
  DocumentIndexingFilter,
  DocumentListPageResponse,
  DocumentListSortKey,
  DocumentListSortOrder,
  DocumentListStatusFilter,
  WebIngestRunListItem,
  WebIngestRunPageItem,
} from './documents'
export { ASYNC_OPERATION_TERMINAL_STATES } from './ops'
export type { AsyncOperationDetail } from './ops'
export { queryApi } from './query'
export { knowledgeApi } from './knowledge'
export { adminApi } from './admin'
