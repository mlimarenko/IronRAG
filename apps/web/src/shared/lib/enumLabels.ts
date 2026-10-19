import type { TFunction } from 'i18next'

/**
 * Human labels for the closed enum vocabularies the API returns.
 *
 * These are contract-defined value sets (document status, queue state, webhook
 * delivery state, …), so a fixed key map is correct here — it is presentation
 * of a protocol enum, not natural-language routing of content.
 *
 * Anything not in the map degrades to a humanized form of the raw value
 * ("graph_extract" -> "Graph extract") so an added backend value never leaks
 * as raw snake_case, and never renders blank.
 */
function humanize(value: string): string {
  const spaced = value.replace(/[_-]+/g, ' ').trim()
  return spaced ? spaced.charAt(0).toUpperCase() + spaced.slice(1) : value
}

function translate(t: TFunction, key: string, raw: string): string {
  const label = t(key)
  // i18next echoes the key back when it is missing — treat that as a miss.
  return label === key ? humanize(raw) : label
}

const DOCUMENT_STATUS_KEYS: Record<string, string> = {
  queued: 'documents.statusQueued',
  processing: 'documents.statusProcessing',
  ready: 'documents.statusReady',
  failed: 'documents.statusFailed',
  canceled: 'documents.statusCanceled',
  empty: 'documents.statusEmpty',
}

export function documentStatusLabel(status: string, t: TFunction): string {
  const key = DOCUMENT_STATUS_KEYS[status]
  return key ? translate(t, key, status) : humanize(status)
}

const QUEUE_STATE_KEYS: Record<string, string> = {
  queued: 'admin.queueStateQueued',
  leased: 'admin.queueStateRunning',
  running: 'admin.queueStateRunning',
  paused: 'admin.queueStatePaused',
  pausing: 'admin.queueStatePausing',
  completed: 'enums.queueCompleted',
  succeeded: 'admin.queueStateSucceeded',
  failed: 'documents.statusFailed',
  canceled: 'documents.statusCanceled',
  abandoned: 'admin.queueStateAbandoned',
}

export function queueStateLabel(state: string, t: TFunction): string {
  const key = QUEUE_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const TOKEN_STATUS_KEYS: Record<string, string> = {
  active: 'common.active',
  disabled: 'admin.disabled',
  revoked: 'admin.revoked',
  expired: 'admin.expired',
}

export function tokenStatusLabel(status: string, t: TFunction): string {
  const key = TOKEN_STATUS_KEYS[status]
  return key ? translate(t, key, status) : humanize(status)
}

const DELIVERY_STATE_KEYS: Record<string, string> = {
  pending: 'enums.deliveryPending',
  delivering: 'enums.deliveryDelivering',
  delivered: 'enums.deliveryDelivered',
  failed: 'documents.statusFailed',
  abandoned: 'enums.deliveryAbandoned',
}

export function deliveryStateLabel(state: string, t: TFunction): string {
  const key = DELIVERY_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const STAGE_STATUS_KEYS: Record<string, string> = {
  started: 'enums.stageStarted',
  completed: 'enums.stageCompleted',
  failed: 'documents.statusFailed',
  skipped: 'enums.stageSkipped',
}

export function stageStatusLabel(status: string, t: TFunction): string {
  const key = STAGE_STATUS_KEYS[status]
  return key ? translate(t, key, status) : humanize(status)
}

const UPLOAD_STATE_KEYS: Record<string, string> = {
  pending: 'enums.uploadPending',
  uploading: 'enums.uploadUploading',
  done: 'enums.uploadDone',
  error: 'enums.uploadError',
}

export function uploadStateLabel(state: string, t: TFunction): string {
  const key = UPLOAD_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const GRAPH_STATUS_KEYS: Record<string, string> = {
  empty: 'enums.graphEmpty',
  building: 'enums.graphBuilding',
  rebuilding: 'enums.graphRebuilding',
  ready: 'documents.statusReady',
  partial: 'enums.graphPartial',
  failed: 'documents.statusFailed',
  stale: 'enums.graphStale',
}

export function graphStatusLabel(status: string, t: TFunction): string {
  const key = GRAPH_STATUS_KEYS[status]
  return key ? translate(t, key, status) : humanize(status)
}

/**
 * Entity sub-types come from extracted content, not from a contract enum, so
 * they can be anything. Only the shape is normalized for display.
 */
export function humanizeToken(value: string): string {
  return humanize(value)
}

const ENTITY_STATE_KEYS: Record<string, string> = {
  active: 'graph.entityStates.active',
  inactive: 'graph.entityStates.inactive',
  suppressed: 'graph.entityStates.suppressed',
}

export function entityStateLabel(state: string, t: TFunction): string {
  const key = ENTITY_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const WEB_INGEST_RUN_STATE_KEYS: Record<string, string> = {
  accepted: 'documents.webRunStates.accepted',
  discovering: 'documents.webRunStates.discovering',
  processing: 'documents.statusProcessing',
  completed: 'documents.webRunStates.completed',
  completed_partial: 'documents.webRunStates.completedPartial',
  failed: 'documents.statusFailed',
  canceled: 'documents.statusCanceled',
}

export function webIngestRunStateLabel(state: string, t: TFunction): string {
  const key = WEB_INGEST_RUN_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const WEB_INGEST_MODE_KEYS: Record<string, string> = {
  single_page: 'documents.singlePage',
  recursive_crawl: 'documents.recursiveCrawl',
}

export function webIngestModeLabel(mode: string, t: TFunction): string {
  const key = WEB_INGEST_MODE_KEYS[mode]
  return key ? translate(t, key, mode) : humanize(mode)
}

const WEB_CANDIDATE_STATE_KEYS: Record<string, string> = {
  discovered: 'documents.pageStateLabels.discovered',
  eligible: 'documents.pageStateLabels.eligible',
  excluded: 'documents.pageStateLabels.excluded',
  blocked: 'documents.pageStateLabels.blocked',
  duplicate: 'documents.pageStateLabels.duplicates',
  duplicates: 'documents.pageStateLabels.duplicates',
  queued: 'documents.pageStateLabels.queued',
  processing: 'documents.pageStateLabels.processing',
  materialized: 'documents.pageStateLabels.materialized',
  processed: 'documents.pageStateLabels.processed',
  failed: 'documents.pageStateLabels.failed',
  canceled: 'documents.pageStateLabels.canceled',
  unknown: 'documents.pageStateLabels.unknown',
}

export function webCandidateStateLabel(state: string, t: TFunction): string {
  const key = WEB_CANDIDATE_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const WEB_CLASSIFICATION_REASON_KEYS: Record<string, string> = {
  seed_accepted: 'documents.webClassificationReasons.seedAccepted',
  duplicate_canonical_url: 'documents.webClassificationReasons.duplicateCanonicalUrl',
  duplicate_content: 'documents.webClassificationReasons.duplicateContent',
  outside_boundary_policy: 'documents.webClassificationReasons.outsideBoundaryPolicy',
  exceeded_max_depth: 'documents.webClassificationReasons.exceededMaxDepth',
  exceeded_max_pages: 'documents.webClassificationReasons.exceededMaxPages',
  url_filter: 'documents.webClassificationReasons.urlFilter',
  unsupported_scheme: 'documents.webClassificationReasons.unsupportedScheme',
  invalid_url: 'documents.webClassificationReasons.invalidUrl',
  inaccessible: 'documents.webClassificationReasons.inaccessible',
  unsupported_content: 'documents.webClassificationReasons.unsupportedContent',
  cancel_requested: 'documents.webClassificationReasons.cancelRequested',
}

export function webClassificationReasonLabel(reason: string, t: TFunction): string {
  const key = WEB_CLASSIFICATION_REASON_KEYS[reason]
  return key ? translate(t, key, reason) : humanize(reason)
}

const STRUCTURED_BLOCK_KIND_KEYS: Record<string, string> = {
  heading: 'documents.blockKinds.heading',
  paragraph: 'documents.blockKinds.paragraph',
  list_item: 'documents.blockKinds.listItem',
  table: 'documents.blockKinds.table',
  table_row: 'documents.blockKinds.tableRow',
  code_block: 'documents.blockKinds.codeBlock',
  endpoint_block: 'documents.blockKinds.endpointBlock',
  quote_block: 'documents.blockKinds.quoteBlock',
  metadata_block: 'documents.blockKinds.metadataBlock',
  source_profile: 'documents.blockKinds.sourceProfile',
  source_unit: 'documents.blockKinds.sourceUnit',
}

export function structuredBlockKindLabel(kind: string, t: TFunction): string {
  const key = STRUCTURED_BLOCK_KIND_KEYS[kind]
  return key ? translate(t, key, kind) : humanize(kind)
}

const TECHNICAL_FACT_KIND_KEYS: Record<string, string> = {
  url: 'documents.factKinds.url',
  endpoint_path: 'documents.factKinds.endpointPath',
  http_method: 'documents.factKinds.httpMethod',
  port: 'documents.factKinds.port',
  parameter_name: 'documents.factKinds.parameterName',
  status_code: 'documents.factKinds.statusCode',
  protocol: 'documents.factKinds.protocol',
  auth_rule: 'documents.factKinds.authRule',
  identifier: 'documents.factKinds.identifier',
  environment_variable: 'documents.factKinds.environmentVariable',
  version_number: 'documents.factKinds.versionNumber',
  database_name: 'documents.factKinds.databaseName',
  configuration_key: 'documents.factKinds.configurationKey',
  error_code: 'documents.factKinds.errorCode',
  rate_limit: 'documents.factKinds.rateLimit',
  dependency_declaration: 'documents.factKinds.dependencyDeclaration',
  code_identifier: 'documents.factKinds.codeIdentifier',
}

export function technicalFactKindLabel(kind: string, t: TFunction): string {
  const key = TECHNICAL_FACT_KIND_KEYS[kind]
  return key ? translate(t, key, kind) : humanize(kind)
}

const REVISION_SOURCE_KIND_KEYS: Record<string, string> = {
  upload: 'documents.sourceKinds.upload',
  append: 'documents.sourceKinds.append',
  replace: 'documents.sourceKinds.replace',
  web_page: 'documents.sourceKinds.webPage',
  edit: 'documents.sourceKinds.edit',
  connector_sync: 'documents.sourceKinds.connectorSync',
  import: 'documents.sourceKinds.import',
}

export function revisionSourceKindLabel(kind: string, t: TFunction): string {
  const key = REVISION_SOURCE_KIND_KEYS[kind]
  return key ? translate(t, key, kind) : humanize(kind)
}

const MIME_TYPE_KEYS: Record<string, string> = {
  'application/pdf': 'documents.mimeTypes.pdf',
  'application/json': 'documents.mimeTypes.json',
  'application/msword': 'documents.mimeTypes.word',
  'application/vnd.openxmlformats-officedocument.wordprocessingml.document':
    'documents.mimeTypes.word',
  'application/vnd.ms-excel': 'documents.mimeTypes.spreadsheet',
  'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet':
    'documents.mimeTypes.spreadsheet',
  'application/vnd.ms-powerpoint': 'documents.mimeTypes.presentation',
  'application/vnd.openxmlformats-officedocument.presentationml.presentation':
    'documents.mimeTypes.presentation',
  'text/csv': 'documents.mimeTypes.csv',
  'text/html': 'documents.mimeTypes.html',
  'text/markdown': 'documents.mimeTypes.markdown',
  'text/plain': 'documents.mimeTypes.plainText',
}

export function mimeTypeLabel(mimeType: string, t: TFunction): string {
  const normalized = (mimeType.split(';')[0] ?? mimeType).trim().toLowerCase()
  const key = MIME_TYPE_KEYS[normalized]
  if (key) return translate(t, key, mimeType)
  const subtype = normalized.split('/').at(-1)
  return humanize(subtype || normalized)
}

const BINDING_STATE_KEYS: Record<string, string> = {
  active: 'admin.enumLabels.bindingState.active',
  disabled: 'admin.enumLabels.bindingState.disabled',
  invalid: 'admin.enumLabels.bindingState.invalid',
  missing: 'admin.enumLabels.bindingState.missing',
}

export function bindingStateLabel(state: string, t: TFunction): string {
  const key = BINDING_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const WEBHOOK_EVENT_KEYS: Record<string, string> = {
  'revision.ready': 'admin.enumLabels.webhookEvent.revisionReady',
  'document.deleted': 'admin.enumLabels.webhookEvent.documentDeleted',
}

export function webhookEventLabel(event: string, t: TFunction): string {
  const key = WEBHOOK_EVENT_KEYS[event]
  return key ? translate(t, key, event) : humanize(event)
}

const HEALTH_STATUS_KEYS: Record<string, string> = {
  ok: 'admin.enumLabels.health.ok',
  down: 'admin.enumLabels.health.down',
  misconfigured: 'admin.enumLabels.health.misconfigured',
  unsupported: 'admin.enumLabels.health.unsupported',
}

export function healthStatusLabel(status: string, t: TFunction): string {
  const key = HEALTH_STATUS_KEYS[status]
  return key ? translate(t, key, status) : humanize(status)
}

const SYSTEM_DETAIL_KEYS: Record<string, string> = {
  bundled: 'admin.enumLabels.systemDetail.bundled',
  external: 'admin.enumLabels.systemDetail.external',
  disabled: 'admin.enumLabels.systemDetail.disabled',
  filesystem: 'admin.enumLabels.systemDetail.filesystem',
  s3: 'admin.enumLabels.systemDetail.s3',
  single_node: 'admin.enumLabels.systemDetail.singleNode',
  shared_cluster: 'admin.enumLabels.systemDetail.sharedCluster',
}

export function systemDetailLabel(detail: string, t: TFunction): string {
  const key = SYSTEM_DETAIL_KEYS[detail]
  return key ? translate(t, key, detail) : humanize(detail)
}

const PROVIDER_MODE_KEYS: Record<string, string> = {
  fixed: 'admin.enumLabels.providerMode.fixed',
  required: 'admin.enumLabels.providerMode.required',
  optional: 'admin.enumLabels.providerMode.optional',
  shared: 'admin.enumLabels.providerMode.shared',
  credential: 'admin.enumLabels.providerMode.credential',
  unsupported: 'admin.enumLabels.providerMode.unsupported',
  chat_round_trip: 'admin.enumLabels.providerMode.chatRoundTrip',
  model_list: 'admin.enumLabels.providerMode.modelList',
  none: 'admin.enumLabels.providerMode.none',
}

export function providerModeLabel(mode: string, t: TFunction): string {
  const key = PROVIDER_MODE_KEYS[mode]
  return key ? translate(t, key, mode) : humanize(mode)
}

const PROVIDER_CAPABILITY_KEYS: Record<string, string> = {
  chat: 'admin.enumLabels.providerCapability.chat',
  embeddings: 'admin.enumLabels.providerCapability.embeddings',
  modelDiscovery: 'admin.enumLabels.providerCapability.modelDiscovery',
  streaming: 'admin.enumLabels.providerCapability.streaming',
  tools: 'admin.enumLabels.providerCapability.tools',
  vision: 'admin.enumLabels.providerCapability.vision',
}

export function providerCapabilityLabel(capability: string, t: TFunction): string {
  const key = PROVIDER_CAPABILITY_KEYS[capability]
  return key ? translate(t, key, capability) : humanize(capability)
}

const CAPABILITY_STATE_KEYS: Record<string, string> = {
  supported: 'admin.enumLabels.capabilityState.supported',
  unsupported: 'admin.enumLabels.capabilityState.unsupported',
  unknown: 'admin.enumLabels.capabilityState.unknown',
}

export function capabilityStateLabel(state: string, t: TFunction): string {
  const key = CAPABILITY_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const LIFECYCLE_STATE_KEYS: Record<string, string> = {
  active: 'admin.enumLabels.lifecycle.active',
  preview: 'admin.enumLabels.lifecycle.preview',
  deprecated: 'admin.enumLabels.lifecycle.deprecated',
  disabled: 'admin.enumLabels.lifecycle.disabled',
}

export function lifecycleStateLabel(state: string, t: TFunction): string {
  const key = LIFECYCLE_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const LIBRARY_LIFECYCLE_STATE_KEYS: Record<string, string> = {
  active: 'admin.enumLabels.libraryLifecycle.active',
  archived: 'admin.enumLabels.libraryLifecycle.archived',
}

export function libraryLifecycleStateLabel(state: string, t: TFunction): string {
  const key = LIBRARY_LIFECYCLE_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const MODEL_AVAILABILITY_KEYS: Record<string, string> = {
  available: 'admin.enumLabels.modelAvailability.available',
  unavailable: 'admin.enumLabels.modelAvailability.unavailable',
  unknown: 'admin.enumLabels.modelAvailability.unknown',
}

export function modelAvailabilityStateLabel(state: string, t: TFunction): string {
  const key = MODEL_AVAILABILITY_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const VALIDATION_STATE_KEYS: Record<string, string> = {
  pending: 'admin.enumLabels.validation.pending',
  succeeded: 'admin.enumLabels.validation.succeeded',
  failed: 'admin.enumLabels.validation.failed',
}

export function validationStateLabel(state: string, t: TFunction): string {
  const key = VALIDATION_STATE_KEYS[state]
  return key ? translate(t, key, state) : humanize(state)
}

const TOKEN_KIND_KEYS: Record<string, string> = {
  session: 'admin.enumLabels.tokenKind.session',
  user: 'admin.enumLabels.tokenKind.user',
  api_token: 'admin.enumLabels.tokenKind.apiToken',
  worker: 'admin.enumLabels.tokenKind.worker',
  bootstrap: 'admin.enumLabels.tokenKind.bootstrap',
}

export function tokenKindLabel(kind: string, t: TFunction): string {
  const key = TOKEN_KIND_KEYS[kind]
  return key ? translate(t, key, kind) : humanize(kind)
}
