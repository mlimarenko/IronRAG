import type {
  ContextAssemblyMetadata,
  GraphGroundingStatus,
  QueryPlanningMetadata,
  RerankMetadata,
} from './graph'

export type ChatQueryMode = 'document' | 'local' | 'global' | 'hybrid' | 'mix'
export type ChatPromptState = 'default' | 'customized'
export type AssistantWarningLevel = 'info' | 'warning' | 'critical'

export interface ChatSessionSummary {
  sessionId: string
  title: string
  messageCount: number
  lastMessagePreview: string | null
  updatedAt: string
  promptState: ChatPromptState
  preferredMode: ChatQueryMode
  isEmpty: boolean
}

export interface ChatSessionDetail {
  sessionId: string
  title: string
  messageCount: number
  lastMessagePreview: string | null
  createdAt: string
  updatedAt: string
  promptState: ChatPromptState
  preferredMode: ChatQueryMode
  isEmpty: boolean
}

export interface ChatSessionSettings {
  sessionId: string
  systemPrompt: string
  promptState: ChatPromptState
  preferredMode: ChatQueryMode
  defaultPromptAvailable: boolean
}

export interface ChatSessionEnvelope {
  session: ChatSessionDetail
  settings: ChatSessionSettings
}

export interface ChatSettingsDraft {
  systemPrompt: string
  preferredMode: ChatQueryMode
  initialSystemPrompt: string
  initialPreferredMode: ChatQueryMode
  isDirty: boolean
  canRestoreDefault: boolean
  validationError: string | null
}

export interface ChatThreadProvider {
  providerKind: string
  modelName: string
}

export interface ChatThreadReference {
  kind: string
  referenceId: string
  excerpt: string | null
  rank: number
  score: number | null
}

export interface AnswerSourceGroup {
  groupKey: string
  label: string
  itemCount: number
  items: ChatThreadReference[]
}

export interface ChatFocusContext {
  nodeId: string
  label: string
  summary: string
  removable: boolean
}

export interface ChatThreadMessage {
  id: string
  role: string
  content: string
  createdAt: string
  queryId: string | null
  mode: ChatQueryMode | null
  groundingStatus: GraphGroundingStatus | null
  provider: ChatThreadProvider | null
  references: ChatThreadReference[]
  planning: QueryPlanningMetadata | null
  rerank: RerankMetadata | null
  contextAssembly: ContextAssemblyMetadata | null
  warning: string | null
  warningKind: string | null
  warningLevel: AssistantWarningLevel | null
  sourceGroups: AnswerSourceGroup[]
  pending: boolean
}

export function buildChatSettingsDraft(settings: ChatSessionSettings): ChatSettingsDraft {
  return {
    systemPrompt: settings.systemPrompt,
    preferredMode: settings.preferredMode,
    initialSystemPrompt: settings.systemPrompt,
    initialPreferredMode: settings.preferredMode,
    isDirty: false,
    canRestoreDefault: settings.promptState === 'customized',
    validationError: null,
  }
}

export function updateChatSettingsDraft(
  draft: ChatSettingsDraft,
  patch: Partial<Pick<ChatSettingsDraft, 'systemPrompt' | 'preferredMode'>>,
): ChatSettingsDraft {
  const next = {
    ...draft,
    ...patch,
  }
  const normalizedPrompt = next.systemPrompt.trim()
  const isDirty =
    normalizedPrompt !== next.initialSystemPrompt.trim() ||
    next.preferredMode !== next.initialPreferredMode
  const validationError =
    normalizedPrompt.length === 0
      ? 'empty'
      : normalizedPrompt.length > 4000
        ? 'too_long'
        : null

  return {
    ...next,
    isDirty,
    canRestoreDefault: draft.canRestoreDefault || isDirty,
    validationError,
  }
}

export function resolveAssistantWarningLevel(
  warningKind: string | null,
  warning: string | null,
): AssistantWarningLevel | null {
  if (!warning) {
    return null
  }
  if (warningKind === 'partial_convergence') {
    return 'warning'
  }
  if (warningKind === 'ungrounded' || warningKind === 'no_grounding') {
    return 'critical'
  }
  return 'info'
}

export function buildAnswerSourceGroups(
  references: ChatThreadReference[],
): AnswerSourceGroup[] {
  const groups = new Map<string, ChatThreadReference[]>()

  for (const reference of references) {
    const key = `${reference.kind}:${reference.referenceId}`
    if (!groups.has(reference.kind)) {
      groups.set(reference.kind, [])
    }
    const items = groups.get(reference.kind) ?? []
    if (!items.some((item) => `${item.kind}:${item.referenceId}` === key)) {
      items.push(reference)
    }
    groups.set(reference.kind, items)
  }

  return Array.from(groups.entries()).map(([groupKey, items]) => ({
    groupKey,
    label: groupKey,
    itemCount: items.length,
    items: [...items].sort((left, right) => left.rank - right.rank),
  }))
}

export function decorateChatThreadMessage(message: Omit<ChatThreadMessage, 'warningLevel' | 'sourceGroups' | 'pending'> & {
  warningLevel?: AssistantWarningLevel | null
  sourceGroups?: AnswerSourceGroup[]
  pending?: boolean
}): ChatThreadMessage {
  return {
    ...message,
    warningLevel: message.warningLevel ?? resolveAssistantWarningLevel(message.warningKind, message.warning),
    sourceGroups: message.sourceGroups ?? buildAnswerSourceGroups(message.references),
    pending: message.pending ?? false,
  }
}
