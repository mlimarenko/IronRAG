import type { operations } from './generated'

export type { components, operations, paths } from './generated'

type JsonRequestBody<T> = T extends {
  requestBody: { content: { 'application/json': infer Body } }
}
  ? Body
  : never

type JsonResponse<T, Status extends number> = T extends {
  responses: Record<Status, { content: { 'application/json': infer Body } }>
}
  ? Body
  : never

type UiOperation<Name extends keyof operations> = operations[Name]

export type UiAuthOperations = Pick<operations, 'uiLogin' | 'getUiSession' | 'uiLogout'>
export type UiShellOperations = Pick<
  operations,
  'getUiContext' | 'updateUiContext' | 'listUiWorkspaces' | 'createUiWorkspace' | 'listUiLibraries' | 'createUiLibrary'
>
export type UiDocumentsOperations = Pick<
  operations,
  | 'getUiDocumentsSurface'
  | 'uploadUiDocuments'
  | 'getUiDocumentDetail'
  | 'deleteUiDocument'
  | 'downloadUiDocumentContent'
  | 'retryUiDocument'
  | 'reprocessUiDocument'
>
export type UiGraphOperations = Pick<
  operations,
  | 'getUiGraphSurface'
  | 'getUiGraphDiagnostics'
  | 'searchUiGraphNodes'
  | 'getUiGraphNodeDetail'
  | 'askUiGraphAssistant'
>
export type UiAdminOperations = Pick<
  operations,
  | 'getUiAdminOverview'
  | 'listUiAdminApiTokens'
  | 'createUiAdminApiToken'
  | 'revokeUiAdminApiToken'
  | 'listUiAdminMembers'
  | 'listUiAdminLibraryAccess'
  | 'getUiAdminSettings'
  | 'updateUiAdminProviderProfile'
  | 'validateUiAdminProviderProfile'
>
export type RuntimeProviderOperations = Pick<
  operations,
  'listRuntimeProviders' | 'validateRuntimeProvider' | 'getRuntimeProviderProfile' | 'updateRuntimeProviderProfile'
>
export type RuntimeDocumentsOperations = Pick<
  operations,
  | 'listRuntimeDocuments'
  | 'uploadRuntimeDocuments'
  | 'getRuntimeDocument'
  | 'deleteRuntimeDocument'
  | 'appendRuntimeDocument'
  | 'replaceRuntimeDocument'
  | 'retryRuntimeDocument'
  | 'reprocessRuntimeDocument'
>
export type RuntimeGraphOperations = Pick<
  operations,
  'getRuntimeGraphSurface' | 'getRuntimeGraphNodeDetail' | 'getRuntimeGraphDiagnostics'
>
export type RuntimeQueryOperations = Pick<
  operations,
  'runRuntimeAnswerQuery' | 'runRuntimeStructuredQuery' | 'getRuntimeQueryExecution'
>

export type UiLoginRequestContract = JsonRequestBody<UiOperation<'uiLogin'>>
export type UiSessionContract = JsonResponse<UiOperation<'getUiSession'>, 200>
export type UiShellContextContract = JsonResponse<UiOperation<'getUiContext'>, 200>
export type UiDocumentsSurfaceContract = JsonResponse<UiOperation<'getUiDocumentsSurface'>, 200>
export type UiDocumentDetailContract = JsonResponse<UiOperation<'getUiDocumentDetail'>, 200>
export type UiUploadDocumentsContract = JsonResponse<UiOperation<'uploadUiDocuments'>, 200>
export type UiGraphSurfaceContract = JsonResponse<UiOperation<'getUiGraphSurface'>, 200>
export type UiGraphDiagnosticsContract = JsonResponse<UiOperation<'getUiGraphDiagnostics'>, 200>
export type UiGraphNodeDetailContract = JsonResponse<UiOperation<'getUiGraphNodeDetail'>, 200>
export type UiGraphAssistantAnswerContract = JsonResponse<UiOperation<'askUiGraphAssistant'>, 200>
export type UiAdminOverviewContract = JsonResponse<UiOperation<'getUiAdminOverview'>, 200>
export type UiAdminApiTokensContract = JsonResponse<UiOperation<'listUiAdminApiTokens'>, 200>
export type UiAdminCreateApiTokenContract = JsonResponse<UiOperation<'createUiAdminApiToken'>, 200>
export type UiAdminSettingsContract = JsonResponse<UiOperation<'getUiAdminSettings'>, 200>
export type UiAdminProviderProfileContract = JsonResponse<UiOperation<'updateUiAdminProviderProfile'>, 200>
export type UiAdminProviderValidationContract = JsonResponse<UiOperation<'validateUiAdminProviderProfile'>, 200>
