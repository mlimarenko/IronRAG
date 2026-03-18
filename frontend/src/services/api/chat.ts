import type {
  ChatPromptState,
  ChatQueryMode,
  ChatSessionDetail,
  ChatSessionEnvelope,
  ChatSessionSettings,
  ChatSessionSummary,
  ChatThreadMessage,
} from 'src/models/ui/chat'
import { decorateChatThreadMessage } from 'src/models/ui/chat'
import type {
  ContextAssemblyMetadata,
  GraphContextAssemblyStatus,
  GraphGroundingStatus,
  GraphRerankStatus,
  QueryIntentCacheStatus,
  QueryPlanningMetadata,
  RerankMetadata,
} from 'src/models/ui/graph'
import { apiHttp, unwrap } from './http'

interface RawChatSessionSummary {
  session_id: string
  title: string
  message_count: number
  last_message_preview: string | null
  updated_at: string
  prompt_state: 'default' | 'customized'
  preferred_mode: ChatQueryMode
  is_empty: boolean
}

interface RawChatSessionDetail {
  session_id: string
  title: string
  message_count: number
  last_message_preview: string | null
  created_at: string
  updated_at: string
  prompt_state: 'default' | 'customized'
  preferred_mode: ChatQueryMode
  is_empty: boolean
}

interface RawChatSessionSettings {
  session_id: string
  system_prompt: string
  prompt_state: 'default' | 'customized'
  preferred_mode: ChatQueryMode
  default_prompt_available: boolean
}

interface RawChatSessionEnvelope {
  session: RawChatSessionDetail
  settings: RawChatSessionSettings
}

interface RawChatThreadProvider {
  provider_kind: string
  model_name: string
}

interface RawChatThreadReference {
  kind: string
  reference_id: string
  excerpt: string | null
  rank: number
  score: number | null
}

interface RawIntentKeywords {
  highLevel: string[]
  lowLevel: string[]
}

interface RawQueryPlanningMetadata {
  requestedMode: ChatQueryMode
  plannedMode: ChatQueryMode
  intentCacheStatus: QueryIntentCacheStatus
  keywords: RawIntentKeywords
  warnings: string[]
}

interface RawRerankMetadata {
  status: GraphRerankStatus
  candidateCount: number
  reorderedCount: number | null
}

interface RawContextAssemblyMetadata {
  status: GraphContextAssemblyStatus
  warning: string | null
}

interface RawChatThreadMessage {
  id: string
  role: string
  content: string
  created_at: string
  query_id: string | null
  mode: ChatQueryMode | null
  grounding_status: GraphGroundingStatus | null
  provider: RawChatThreadProvider | null
  references: RawChatThreadReference[]
  planning: RawQueryPlanningMetadata | null
  rerank: RawRerankMetadata | null
  context_assembly: RawContextAssemblyMetadata | null
  warning: string | null
  warning_kind: string | null
}

interface CreateChatSessionPayload {
  title?: string
  system_prompt?: string
  preferred_mode?: ChatQueryMode
}

interface UpdateChatSessionPayload {
  title?: string
  system_prompt?: string
  prompt_state?: ChatPromptState
  preferred_mode?: ChatQueryMode
  restore_default?: boolean
}

function mapPlanningMetadata(metadata: RawQueryPlanningMetadata): QueryPlanningMetadata {
  return {
    requestedMode: metadata.requestedMode,
    plannedMode: metadata.plannedMode,
    intentCacheStatus: metadata.intentCacheStatus,
    keywords: {
      highLevel: metadata.keywords.highLevel,
      lowLevel: metadata.keywords.lowLevel,
    },
    warnings: metadata.warnings,
  }
}

function mapRerankMetadata(metadata: RawRerankMetadata): RerankMetadata {
  return {
    status: metadata.status,
    candidateCount: metadata.candidateCount,
    reorderedCount: metadata.reorderedCount,
  }
}

function mapContextAssemblyMetadata(metadata: RawContextAssemblyMetadata): ContextAssemblyMetadata {
  return {
    status: metadata.status,
    warning: metadata.warning,
  }
}

function mapSessionSummary(summary: RawChatSessionSummary): ChatSessionSummary {
  return {
    sessionId: summary.session_id,
    title: summary.title,
    messageCount: summary.message_count,
    lastMessagePreview: summary.last_message_preview,
    updatedAt: summary.updated_at,
    promptState: summary.prompt_state,
    preferredMode: summary.preferred_mode,
    isEmpty: summary.is_empty,
  }
}

function mapSessionDetail(session: RawChatSessionDetail): ChatSessionDetail {
  return {
    sessionId: session.session_id,
    title: session.title,
    messageCount: session.message_count,
    lastMessagePreview: session.last_message_preview,
    createdAt: session.created_at,
    updatedAt: session.updated_at,
    promptState: session.prompt_state,
    preferredMode: session.preferred_mode,
    isEmpty: session.is_empty,
  }
}

function mapSessionSettings(settings: RawChatSessionSettings): ChatSessionSettings {
  return {
    sessionId: settings.session_id,
    systemPrompt: settings.system_prompt,
    promptState: settings.prompt_state,
    preferredMode: settings.preferred_mode,
    defaultPromptAvailable: settings.default_prompt_available,
  }
}

function mapEnvelope(envelope: RawChatSessionEnvelope): ChatSessionEnvelope {
  return {
    session: mapSessionDetail(envelope.session),
    settings: mapSessionSettings(envelope.settings),
  }
}

function mapMessage(message: RawChatThreadMessage): ChatThreadMessage {
  return decorateChatThreadMessage({
    id: message.id,
    role: message.role,
    content: message.content,
    createdAt: message.created_at,
    queryId: message.query_id,
    mode: message.mode,
    groundingStatus: message.grounding_status,
    provider: message.provider
      ? {
          providerKind: message.provider.provider_kind,
          modelName: message.provider.model_name,
        }
      : null,
    references: message.references.map((reference) => ({
      kind: reference.kind,
      referenceId: reference.reference_id,
      excerpt: reference.excerpt,
      rank: reference.rank,
      score: reference.score,
    })),
    planning: message.planning ? mapPlanningMetadata(message.planning) : null,
    rerank: message.rerank ? mapRerankMetadata(message.rerank) : null,
    contextAssembly: message.context_assembly
      ? mapContextAssemblyMetadata(message.context_assembly)
      : null,
    warning: message.warning,
    warningKind: message.warning_kind,
  })
}

export async function listChatSessions(projectId: string): Promise<ChatSessionSummary[]> {
  return (
    await unwrap(
      apiHttp.get<RawChatSessionSummary[]>('/chat/sessions', {
        params: { project_id: projectId },
      }),
    )
  ).map(mapSessionSummary)
}

export async function createChatSession(
  projectId: string,
  payload?: CreateChatSessionPayload,
): Promise<ChatSessionEnvelope> {
  const response = await unwrap(
    apiHttp.post<RawChatSessionEnvelope>('/chat/sessions', {
      project_id: projectId,
      ...payload,
    }),
  )
  return mapEnvelope(response)
}

export async function fetchChatSession(sessionId: string): Promise<ChatSessionEnvelope> {
  const response = await unwrap(apiHttp.get<RawChatSessionEnvelope>(`/chat/sessions/${sessionId}`))
  return mapEnvelope(response)
}

export async function fetchChatSessionMessages(sessionId: string): Promise<ChatThreadMessage[]> {
  return (
    await unwrap(apiHttp.get<RawChatThreadMessage[]>(`/chat/sessions/${sessionId}/messages`))
  ).map(mapMessage)
}

export async function updateChatSession(
  sessionId: string,
  payload: UpdateChatSessionPayload,
): Promise<ChatSessionEnvelope> {
  const response = await unwrap(
    apiHttp.patch<RawChatSessionEnvelope>(`/chat/sessions/${sessionId}`, payload),
  )
  return mapEnvelope(response)
}
