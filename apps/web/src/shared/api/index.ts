// SDK surface — generated from apps/api/contracts/openapi.gen.yaml
// via @hey-api/openapi-ts. Re-export the tag classes so call sites can
// `import { Catalog } from '@/shared/api'` without reaching into the generated/
// sub-tree (an ESLint boundary rule will be added with the feature-folder
// migration in a later sprint).
export {
  Catalog,
  Ops,
} from "./generated";
export { ApiError, unwrap } from "./runtime";

// Generated TanStack Query 5 query/mutation options + key factories for
// server-state hooks.
export * as queries from "./generated/@tanstack/react-query.gen";

// UI API facades. Feature modules import these domain facades instead
// of depending on generated operation names directly.
export { authApi } from "./auth";
export type {
  CatalogLibraryResponse,
  DashboardSurface,
  DocumentLifecycleDetail,
  WebBoundaryPolicy,
  WebIngestMode,
  WebIngestPattern,
  WebIngestUrlFilter,
} from "./generated";
export {
  documentsApi,
  librarySnapshotApi,
  DOCUMENT_LIST_STATUS_FILTERS,
} from "./documents";
export type {
  DocumentListPageResponse,
  DocumentListSortKey,
  DocumentListSortOrder,
  DocumentListStatusFilter,
  WebIngestRunListItem,
  WebIngestRunPageItem,
} from "./documents";
export { ASYNC_OPERATION_TERMINAL_STATES } from "./ops";
export type { AsyncOperationDetail, AsyncOperationStatus } from "./ops";
export { queryApi } from "./query";
export { knowledgeApi } from "./knowledge";
export {
  ADMIN_MODEL_CATALOG_QUERY_KEY,
  adminApi,
  adminModelCatalogOptions,
  adminModelCatalogQueryKey,
} from "./admin";
export type {
  ListModelsParams,
} from "./admin";
