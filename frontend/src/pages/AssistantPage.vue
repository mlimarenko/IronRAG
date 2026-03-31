<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { storeToRefs } from 'pinia'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import FeedbackState from 'src/components/design-system/FeedbackState.vue'
import AssistantEvidencePanel from 'src/components/assistant/AssistantEvidencePanel.vue'
import AssistantMarkdownContent from 'src/components/assistant/AssistantMarkdownContent.vue'
import { useDisplayFormatters } from 'src/composables/useDisplayFormatters'
import {
  DOCUMENT_UPLOAD_FORMAT_TOKENS,
  buildDocumentUploadAcceptString,
} from 'src/models/ui/documentFormats'
import { uploadDocuments } from 'src/services/api/documents'
import type {
  ExecuteQueryTurnPayload,
  QuerySession,
  QuerySessionDetail,
  QueryTurnStreamStage,
  QueryTurn,
} from 'src/services/api/query'
import { useQueryStore } from 'src/stores/query'
import { useShellStore } from 'src/stores/shell'

interface AssistantNotice {
  kind: 'busy' | 'error' | 'info'
  message: string
}

interface RenderedChatMessage {
  id: string
  author: 'user' | 'assistant'
  content: string
  createdAt: string | null
  pending?: boolean
  variant?: 'progress' | 'stream'
}

type AssistantProgressStage = 'retrieving' | 'grounding' | 'answering'

const MAX_LIBRARY_CHAT_SESSIONS = 5

const queryStore = useQueryStore()
const shellStore = useShellStore()
const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const { formatCompactDateTime } = useDisplayFormatters()
const {
  activeBundle,
  activeExecution,
  activeSession,
  error,
  executingTurn,
  groundedChunkReferences,
  groundedEntityReferences,
  groundedEvidenceReferences,
  groundedRelationReferences,
  loadingExecution,
  loadingSession,
  loadingSessions,
  sessions,
} = storeToRefs(queryStore)
const { activeLibrary, activeWorkspace } = storeToRefs(shellStore)

const transientNotice = ref<string | null>(null)
const syncingLibrary = ref(false)
const sessionSearch = ref('')
const composerText = ref('')
const composerFiles = ref<File[]>([])
const conversationBodyRef = ref<HTMLElement | null>(null)
const composerFileInputRef = ref<HTMLInputElement | null>(null)
const composerInputRef = ref<HTMLTextAreaElement | null>(null)
const pendingUserMessage = ref<string | null>(null)
const pendingAssistantStage = ref<AssistantProgressStage>('retrieving')
const retainedSession = ref<QuerySessionDetail | null>(null)
const conversationPinnedToBottom = ref(true)
const showEvidencePanel = ref(false)
const showSessionRail = ref(true)
const compactAssistantLayout = ref(false)
const streamedAssistantText = ref('')
const assistantResponseStreaming = ref(false)
let syncToken = 0

const sessionQueryId = computed(() =>
  typeof route.query.session === 'string' ? route.query.session : null,
)
const activeLibraryId = computed(() => activeLibrary.value?.id ?? null)
const activeWorkspaceId = computed(() => activeWorkspace.value?.id ?? null)
const assistantReady = computed(() => {
  if (!activeLibrary.value) {
    return false
  }
  return !activeLibrary.value.ingestionReadiness.missingBindingPurposes.includes('query_answer')
})

const acceptedFiles = buildDocumentUploadAcceptString([...DOCUMENT_UPLOAD_FORMAT_TOKENS])
const viewerUserName = computed(() => t('assistant.you'))
const activeThreadSession = computed(() => activeSession.value ?? retainedSession.value)
const selectedSessionId = computed(
  () => sessionQueryId.value ?? activeThreadSession.value?.session.id ?? null,
)
const loadingState = computed(
  () =>
    loadingSessions.value || loadingSession.value || loadingExecution.value || syncingLibrary.value,
)
const atSessionCapacity = computed(
  () => sessions.value.length >= MAX_LIBRARY_CHAT_SESSIONS,
)
const sessionCapacityLabel = computed(
  () => `${sessions.value.length}/${MAX_LIBRARY_CHAT_SESSIONS}`,
)
const toolbarHint = computed(() => (atSessionCapacity.value ? t('assistant.toolbar.rollover') : null))
const composerDisabled = computed(
  () =>
    !assistantReady.value ||
    syncingLibrary.value ||
    loadingSession.value ||
    executingTurn.value,
)
const sessionTransitioning = computed(
  () => syncingLibrary.value || loadingSession.value,
)
const sessionsToggleLabel = computed(() =>
  showSessionRail.value
    ? t('assistant.actions.hideSessions')
    : t('assistant.actions.showSessions'),
)
const contextToggleLabel = computed(() =>
  showEvidencePanel.value
    ? t('assistant.actions.hideContext')
    : t('assistant.actions.showContext'),
)
const showSessionSearch = computed(() => sessions.value.length >= 4)
const starterPrompts = computed(() => [
  t('assistant.starters.summary'),
  t('assistant.starters.risks'),
  t('assistant.starters.documents'),
])

const visibleTurns = computed(() =>
  (activeThreadSession.value?.turns ?? []).filter(
    (turn) => turn.turnKind === 'user' || turn.turnKind === 'assistant',
  ),
)

const filteredSessions = computed(() => {
  const query = sessionSearch.value.trim().toLowerCase()
  if (!query) {
    return sessions.value
  }
  return sessions.value.filter((session) =>
    composeSessionPreview(session).toLowerCase().includes(query),
  )
})

const activeSessionTitle = computed(() => {
  if (!activeThreadSession.value) {
    return t('assistant.chat.roomEmpty')
  }
  return resolveSessionTitle(
    activeThreadSession.value.session,
    activeThreadSession.value.turns,
    pendingUserMessage.value,
  )
})

const activeSessionMeta = computed(() => {
  const libraryName = activeLibrary.value?.name ?? '—'
  if (!activeThreadSession.value) {
    return libraryName
  }
  return `${libraryName} · ${formatCompactDateTime(activeThreadSession.value.session.updatedAt)}`
})

const emptyConversation = computed(
  () =>
    !loadingState.value &&
    visibleTurns.value.length === 0 &&
    !pendingUserMessage.value &&
    !assistantResponseStreaming.value &&
    !executingTurn.value,
)

const pendingAssistantLabel = computed(() =>
  t(`assistant.progress.${pendingAssistantStage.value}`),
)

const pendingAssistantSteps = computed(() => {
  const stages: AssistantProgressStage[] = ['retrieving', 'grounding', 'answering']
  const activeIndex = stages.indexOf(pendingAssistantStage.value)
  return stages.map((stage, index) => ({
    key: stage,
    label: t(`assistant.progress.${stage}`),
    state:
      index < activeIndex
        ? 'complete'
        : index === activeIndex
          ? 'active'
          : 'idle',
  }))
})

const renderedMessages = computed<RenderedChatMessage[]>(() => {
  const mapped = visibleTurns.value.map<RenderedChatMessage>((turn) => {
    return {
      id: turn.id,
      author: turn.turnKind === 'assistant' ? 'assistant' : 'user',
      content: turn.contentText,
      createdAt: turn.createdAt,
    }
  })

  if (pendingUserMessage.value) {
    mapped.push({
      id: 'pending-user',
      author: 'user',
      content: pendingUserMessage.value,
      createdAt: null,
      pending: true,
    })
  }

  if (assistantResponseStreaming.value && streamedAssistantText.value) {
    mapped.push({
      id: 'streaming-assistant',
      author: 'assistant',
      content: streamedAssistantText.value,
      createdAt: null,
      pending: true,
      variant: 'stream',
    })
  } else if (executingTurn.value || pendingUserMessage.value) {
    mapped.push({
      id: 'pending-assistant',
      author: 'assistant',
      content: pendingAssistantLabel.value,
      createdAt: null,
      pending: true,
      variant: 'progress',
    })
  }

  return mapped
})

const chatNotice = computed<AssistantNotice | null>(() => {
  if (error.value) {
    return {
      kind: 'error',
      message: error.value,
    }
  }
  if (transientNotice.value) {
    return {
      kind: 'info',
      message: transientNotice.value,
    }
  }
  return null
})

function isWeakSessionTitle(value: string | null): boolean {
  const normalized = value?.trim() ?? ''
  if (!normalized) {
    return true
  }
  const words = normalized.split(/\s+/).filter(Boolean)
  return normalized.length <= 6 || (words.length <= 1 && normalized.length <= 14)
}

function deriveSessionTitleFromTurns(turns: QueryTurn[]): string | null {
  const candidate = [...turns]
    .reverse()
    .find((turn) => turn.turnKind === 'user' && turn.contentText.trim().length >= 8)
  return candidate ? deriveSessionTitleFromContent(candidate.contentText) : null
}

function deriveSessionTitleFromContent(content: string): string | null {
  const collapsed = content.split(/\s+/).filter(Boolean).join(' ')
  if (!collapsed) {
    return null
  }
  if (collapsed.length <= 72) {
    return collapsed
  }
  return `${collapsed.slice(0, 72).trimEnd()}…`
}

function fallbackSessionTitle(title: string | null, createdAt: string): string {
  return title ?? t('assistant.sessionFallback', { createdAt: formatCompactDateTime(createdAt) })
}

function resolveSessionTitle(
  session: QuerySession,
  turns: QueryTurn[] = [],
  optimisticContent: string | null = null,
): string {
  if (!isWeakSessionTitle(session.title)) {
    return fallbackSessionTitle(session.title, session.createdAt)
  }
  return (
    deriveSessionTitleFromContent(optimisticContent ?? '') ??
    deriveSessionTitleFromTurns(turns) ??
    fallbackSessionTitle(session.title, session.createdAt)
  )
}

function sessionUpdatedLabel(session: QuerySession): string {
  return formatCompactDateTime(session.updatedAt)
}

function messageTimeLabel(turn: QueryTurn | RenderedChatMessage): string {
  return turn.createdAt ? formatCompactDateTime(turn.createdAt) : t('assistant.chat.now')
}

function composeSessionPreview(session: QuerySession): string {
  const title =
    session.id === activeThreadSession.value?.session.id
      ? resolveSessionTitle(
          session,
          activeThreadSession.value?.turns ?? [],
          pendingUserMessage.value,
        )
      : resolveSessionTitle(session)
  return title.length > 74 ? `${title.slice(0, 74)}…` : title
}

function startAssistantProgress(): void {
  pendingAssistantStage.value = 'retrieving'
}

function stopAssistantProgress(): void {
  pendingAssistantStage.value = 'retrieving'
}

function syncComposerHeight(): void {
  const textarea = composerInputRef.value
  if (!textarea) {
    return
  }
  textarea.style.height = 'auto'
  textarea.style.height = `${Math.min(textarea.scrollHeight, 220)}px`
}

function isConversationNearBottom(element: HTMLElement, threshold = 96): boolean {
  return element.scrollHeight - element.scrollTop - element.clientHeight <= threshold
}

function handleConversationScroll(): void {
  const element = conversationBodyRef.value
  if (!element) {
    return
  }
  conversationPinnedToBottom.value = isConversationNearBottom(element)
}

function focusComposer(): void {
  composerInputRef.value?.focus()
}

function stopAssistantResponseStream(): void {
  streamedAssistantText.value = ''
  assistantResponseStreaming.value = false
}

function handleAssistantStreamStage(stage: QueryTurnStreamStage): void {
  pendingAssistantStage.value = stage
}

function appendAssistantResponseDelta(delta: string): void {
  if (!delta) {
    return
  }
  if (!assistantResponseStreaming.value) {
    streamedAssistantText.value = ''
    assistantResponseStreaming.value = true
  }
  streamedAssistantText.value += delta
  const element = conversationBodyRef.value
  if (element && conversationPinnedToBottom.value) {
    element.scrollTop = element.scrollHeight
  }
}

async function scrollConversationToBottom(behavior: ScrollBehavior = 'auto'): Promise<void> {
  await nextTick()
  const element = conversationBodyRef.value
  if (!element) {
    return
  }
  conversationPinnedToBottom.value = true
  element.scrollTo({ top: element.scrollHeight, behavior })
}

watch(
  () => [activeThreadSession.value?.session.id, visibleTurns.value.length],
  () => {
    conversationPinnedToBottom.value = true
    void scrollConversationToBottom('auto')
  },
)

watch(
  () => [pendingUserMessage.value, executingTurn.value],
  () => {
    if (conversationPinnedToBottom.value || pendingUserMessage.value) {
      void scrollConversationToBottom('smooth')
    }
  },
)

watch(
  activeSession,
  (session) => {
    if (session) {
      retainedSession.value = session
    }
  },
  { immediate: true },
)

watch(activeLibraryId, (nextLibraryId, previousLibraryId) => {
  if (nextLibraryId !== previousLibraryId) {
    retainedSession.value = null
    stopAssistantResponseStream()
  }
})

watch(composerText, () => {
  void nextTick(syncComposerHeight)
})

function syncAssistantViewportState(): void {
  const viewportWidth = window.innerWidth
  compactAssistantLayout.value = viewportWidth <= 1080
  showSessionRail.value = viewportWidth > 1080
}

onMounted(() => {
  syncAssistantViewportState()
  syncComposerHeight()
  window.addEventListener('resize', syncAssistantViewportState)
})

onBeforeUnmount(() => {
  stopAssistantResponseStream()
  window.removeEventListener('resize', syncAssistantViewportState)
})

async function syncLatestExecution(): Promise<void> {
  const latestExecutionId = activeSession.value?.executions[0]?.id ?? null
  if (!latestExecutionId) {
    queryStore.activeExecution = null
    queryStore.activeBundle = null
    return
  }
  if (activeExecution.value?.execution.id === latestExecutionId) {
    return
  }
  await queryStore.loadExecution(latestExecutionId)
}

async function replaceSessionQuery(sessionId: string): Promise<void> {
  if (route.query.session === sessionId) {
    return
  }
  await router.replace({
    query: {
      ...route.query,
      session: sessionId,
    },
  })
}

async function ensureActiveSession(): Promise<string> {
  const currentSessionId = selectedSessionId.value ?? activeThreadSession.value?.session.id ?? null
  if (currentSessionId) {
    if (activeSession.value?.session.id !== currentSessionId) {
      await queryStore.loadSession(currentSessionId)
    }
    return currentSessionId
  }

  const firstSession = sessions.value[0] ?? null
  if (firstSession) {
    await queryStore.loadSession(firstSession.id)
    await replaceSessionQuery(firstSession.id)
    return firstSession.id
  }

  const created = await queryStore.createSession({
    workspaceId: activeWorkspaceId.value ?? undefined,
    libraryId: activeLibraryId.value ?? undefined,
  })
  await replaceSessionQuery(created.id)
  return created.id
}

async function openSession(sessionId: string): Promise<void> {
  if (sessionTransitioning.value) {
    return
  }
  stopAssistantResponseStream()
  if (selectedSessionId.value === sessionId) {
    await replaceSessionQuery(sessionId)
    if (compactAssistantLayout.value) {
      showSessionRail.value = false
    }
    focusComposer()
    return
  }
  await queryStore.loadSession(sessionId)
  await syncLatestExecution()
  await replaceSessionQuery(sessionId)
  if (compactAssistantLayout.value) {
    showSessionRail.value = false
  }
  focusComposer()
}

async function syncAssistantLibrary(libraryId: string): Promise<void> {
  const token = ++syncToken
  syncingLibrary.value = true
  transientNotice.value = null
  stopAssistantResponseStream()
  queryStore.reset()

  try {
    await queryStore.loadSessions(libraryId)
    if (token !== syncToken) {
      return
    }

    const targetSessionId =
      sessions.value.find((session) => session.id === sessionQueryId.value)?.id ??
      sessions.value[0]?.id ??
      null

    if (targetSessionId) {
      await queryStore.loadSession(targetSessionId)
      if (token !== syncToken) {
        return
      }
      await syncLatestExecution()
      await replaceSessionQuery(targetSessionId)
      return
    }

    const created = await queryStore.createSession({
      workspaceId: activeWorkspaceId.value ?? undefined,
      libraryId,
    })
    if (token !== syncToken) {
      return
    }
    await syncLatestExecution()
    await replaceSessionQuery(created.id)
  } finally {
    if (token === syncToken) {
      syncingLibrary.value = false
    }
  }
}

watch(
  [activeLibraryId, assistantReady],
  async ([libraryId, ready]) => {
    if (!libraryId || !ready) {
      queryStore.reset()
      syncingLibrary.value = false
      return
    }

    await syncAssistantLibrary(libraryId)
  },
  { immediate: true },
)

watch(sessionQueryId, async (nextSessionId) => {
  if (syncingLibrary.value || !assistantReady.value || !activeLibraryId.value || !nextSessionId) {
    return
  }

  if (nextSessionId === activeSession.value?.session.id) {
    return
  }

  const knownSession = sessions.value.find((session) => session.id === nextSessionId)
  if (!knownSession) {
    await syncAssistantLibrary(activeLibraryId.value)
    return
  }

  await queryStore.loadSession(knownSession.id)
  await syncLatestExecution()
})

async function startNewSession(): Promise<void> {
  if (!assistantReady.value || sessionTransitioning.value) {
    return
  }
  const willRollover = sessions.value.length >= MAX_LIBRARY_CHAT_SESSIONS
  const session = await queryStore.createSession({
    workspaceId: activeWorkspaceId.value ?? undefined,
    libraryId: activeLibraryId.value ?? undefined,
  })
  sessionSearch.value = ''
  transientNotice.value = willRollover
    ? t('assistant.notices.sessionRolledOver')
    : t('assistant.notices.newSession')
  await syncLatestExecution()
  await replaceSessionQuery(session.id)
  if (compactAssistantLayout.value) {
    showSessionRail.value = false
  }
  focusComposer()
}

function buildUploadNotice(uploadedCount: number, withQuestion: boolean): string {
  return withQuestion
    ? t('assistant.notices.filesUploadedWithQuestion', { count: uploadedCount })
    : t('assistant.notices.filesUploadedOnly', { count: uploadedCount })
}

async function uploadComposerFiles(files: File[]): Promise<number> {
  if (files.length === 0) {
    return 0
  }
  const result = await uploadDocuments(files)
  return result.acceptedRows.length
}

function openComposerFilePicker(): void {
  composerFileInputRef.value?.click()
}

function handleComposerFiles(event: Event): void {
  const input = event.target as HTMLInputElement | null
  const nextFiles = Array.from(input?.files ?? [])
  if (nextFiles.length === 0) {
    return
  }

  const knownFiles = new Map(
    composerFiles.value.map((file) => [
      `${file.name}:${file.size}:${file.lastModified}`,
      file,
    ]),
  )
  for (const file of nextFiles) {
    knownFiles.set(`${file.name}:${file.size}:${file.lastModified}`, file)
  }
  composerFiles.value = Array.from(knownFiles.values())

  if (input) {
    input.value = ''
  }
}

function removeComposerFile(index: number): void {
  composerFiles.value = composerFiles.value.filter((_, currentIndex) => currentIndex !== index)
}

function handleComposerKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' || event.shiftKey) {
    return
  }
  event.preventDefault()
  void sendPrompt()
}

function toggleSessionRail(): void {
  showSessionRail.value = !showSessionRail.value
}

function closeSessionRail(): void {
  showSessionRail.value = false
}

function toggleEvidencePanel(): void {
  showEvidencePanel.value = !showEvidencePanel.value
}

function closeEvidencePanel(): void {
  showEvidencePanel.value = false
}

function applyStarterPrompt(prompt: string): void {
  composerText.value = prompt
  syncComposerHeight()
  focusComposer()
}

async function sendPrompt(): Promise<void> {
  if (composerDisabled.value) {
    return
  }

  const content = composerText.value.trim()
  const files = [...composerFiles.value]
  if (!content && files.length === 0) {
    return
  }

  const restoreText = content
  const restoreFiles = files
  composerText.value = ''
  composerFiles.value = []
  syncComposerHeight()

  if (content) {
    pendingUserMessage.value = content
    startAssistantProgress()
    conversationPinnedToBottom.value = true
    void scrollConversationToBottom('smooth')
  }

  try {
    const uploadedCount = await uploadComposerFiles(files)

    if (uploadedCount > 0) {
      transientNotice.value = buildUploadNotice(uploadedCount, content.length > 0)
    }

    if (!content) {
      if (activeLibraryId.value) {
        await queryStore.loadSessions(activeLibraryId.value)
      }
      focusComposer()
      return
    }

    const sessionId = await ensureActiveSession()
    await replaceSessionQuery(sessionId)
    const payloadBody: ExecuteQueryTurnPayload = {
      contentText: content,
      topK: 48,
      includeDebug: false,
    }
    await queryStore.runTurn(sessionId, payloadBody, {
      onStage: handleAssistantStreamStage,
      onAnswerDelta: appendAssistantResponseDelta,
    })
    pendingUserMessage.value = null
    stopAssistantProgress()
    stopAssistantResponseStream()
    if (compactAssistantLayout.value) {
      showSessionRail.value = false
    }
    focusComposer()
    await scrollConversationToBottom('smooth')
  } catch (sendError) {
    pendingUserMessage.value = null
    stopAssistantProgress()
    stopAssistantResponseStream()
    composerText.value = restoreText
    composerFiles.value = restoreFiles
    syncComposerHeight()
  }
}

function openDocuments(): void {
  void router.push('/documents')
}

function openGraph(): void {
  const firstEntityReference = groundedEntityReferences.value[0]
  void router.push(
    firstEntityReference
      ? { path: '/graph', query: { node: firstEntityReference.entityId } }
      : '/graph',
  )
}

function openAiAdmin(): void {
  void router.push('/admin?section=ai')
}

function retryAssistant(): void {
  if (!activeLibraryId.value) {
    return
  }
  void syncAssistantLibrary(activeLibraryId.value)
}
</script>

<template>
  <div class="rr-assistant-page">
    <div class="rr-assistant-page__header">
      <div class="rr-assistant-page__copy">
        <h1>{{ t('assistant.title') }}</h1>
      </div>
    </div>

    <FeedbackState
      v-if="!activeLibraryId"
      :title="t('assistant.states.noLibraryTitle')"
      :message="t('assistant.states.noLibraryBody')"
      kind="empty"
      :action-label="t('assistant.actions.openDocuments')"
      @action="openDocuments"
    />

    <FeedbackState
      v-else-if="!assistantReady"
      :title="t('assistant.states.bindingTitle')"
      :message="t('assistant.states.bindingBody')"
      :details="[t('assistant.states.bindingDetail')]"
      kind="warning"
      :action-label="t('assistant.actions.openAiAdmin')"
      @action="openAiAdmin"
    />

    <FeedbackState
      v-else-if="error && !activeSession && !sessions.length"
      :title="t('assistant.states.errorTitle')"
      :message="error"
      kind="error"
      :action-label="t('assistant.actions.retry')"
      @action="retryAssistant"
    />

    <div
      v-else
      class="rr-assistant-page__layout"
      :class="{
        'rr-assistant-page__layout--context-hidden': !showEvidencePanel,
        'rr-assistant-page__layout--compact': compactAssistantLayout,
      }"
    >
      <section class="rr-assistant-page__chat-shell">
        <div
          class="rr-assistant-chat"
          :class="{ 'rr-assistant-chat--rail-hidden': compactAssistantLayout && !showSessionRail }"
        >
          <button
            v-if="compactAssistantLayout && showSessionRail"
            type="button"
            class="rr-assistant-chat__rail-backdrop"
            :aria-label="t('assistant.actions.hideSessions')"
            @click="closeSessionRail"
          />

          <aside
            v-if="!compactAssistantLayout || showSessionRail"
            class="rr-assistant-chat__rail"
            :class="{ 'rr-assistant-chat__rail--overlay': compactAssistantLayout }"
          >
            <div class="rr-assistant-chat__rail-head">
              <div class="rr-assistant-chat__rail-copy">
                <div class="rr-assistant-chat__rail-headline">
                  <strong>{{ t('assistant.toolbar.sessionsTitle') }}</strong>
                  <span class="rr-assistant-chat__rail-count">{{ sessionCapacityLabel }}</span>
                </div>
                <span v-if="toolbarHint">{{ toolbarHint }}</span>
              </div>
              <button
                type="button"
                class="rr-assistant-chat__new-session"
                :disabled="!assistantReady || sessionTransitioning"
                :aria-label="t('assistant.actions.newSession')"
                :title="t('assistant.actions.newSession')"
                @click="startNewSession"
              >
                <svg viewBox="0 0 18 18" aria-hidden="true">
                  <path
                    d="M9 3v12M3 9h12"
                    fill="none"
                    stroke="currentColor"
                    stroke-linecap="round"
                    stroke-width="1.8"
                  />
                </svg>
              </button>
            </div>

            <label
              v-if="showSessionSearch"
              class="rr-field rr-assistant-chat__search"
            >
              <input
                v-model="sessionSearch"
                type="search"
                :placeholder="t('assistant.chat.search')"
              >
            </label>

            <div
              v-if="filteredSessions.length > 0"
              class="rr-assistant-chat__session-list"
            >
              <button
                v-for="session in filteredSessions"
                :key="session.id"
                type="button"
                class="rr-assistant-chat__session"
                :class="{
                  'rr-assistant-chat__session--active': session.id === selectedSessionId,
                }"
                :disabled="sessionTransitioning"
                @click="openSession(session.id)"
              >
                <strong>{{ composeSessionPreview(session) }}</strong>
                <span>{{ sessionUpdatedLabel(session) }}</span>
              </button>
            </div>

            <div
              v-else
              class="rr-assistant-chat__rail-empty"
            >
              <strong>{{ t('assistant.chat.searchEmptyTitle') }}</strong>
              <p>{{ t('assistant.chat.searchEmptyBody') }}</p>
            </div>
          </aside>

          <section class="rr-assistant-chat__thread">
            <div class="rr-assistant-chat__thread-head">
              <div class="rr-assistant-chat__thread-head-inner">
                <div class="rr-assistant-chat__thread-copy">
                  <strong>{{ activeSessionTitle }}</strong>
                  <span>{{ activeSessionMeta }}</span>
                </div>

                <div class="rr-assistant-chat__thread-actions">
                  <button
                    v-if="compactAssistantLayout"
                    type="button"
                    class="rr-assistant-chat__toolbar-button"
                    :class="{ 'is-active': showSessionRail }"
                    :aria-label="sessionsToggleLabel"
                    :title="sessionsToggleLabel"
                    @click="toggleSessionRail"
                  >
                    {{ t('assistant.actions.sessions') }}
                  </button>

                  <button
                    type="button"
                    class="rr-assistant-chat__toolbar-button"
                    :class="{ 'is-active': showEvidencePanel }"
                    :disabled="sessionTransitioning"
                    :aria-label="contextToggleLabel"
                    :title="contextToggleLabel"
                    @click="toggleEvidencePanel"
                  >
                    {{ t('assistant.actions.context') }}
                  </button>
                </div>
              </div>
            </div>

            <div
              v-if="chatNotice"
              class="rr-assistant-page__notice"
              :class="`rr-assistant-page__notice--${chatNotice.kind}`"
            >
              {{ chatNotice.message }}
            </div>

            <div
              ref="conversationBodyRef"
              class="rr-assistant-chat__thread-body"
              @scroll="handleConversationScroll"
            >
              <div
              v-if="emptyConversation"
                class="rr-assistant-chat__empty"
              >
                <strong>{{ t('assistant.chat.emptyConversationTitle') }}</strong>
                <p>{{ t('assistant.chat.emptyConversationBody') }}</p>
                <div class="rr-assistant-chat__starter-list">
                  <button
                    v-for="prompt in starterPrompts"
                    :key="prompt"
                    type="button"
                    class="rr-assistant-chat__starter"
                    @click="applyStarterPrompt(prompt)"
                  >
                    {{ prompt }}
                  </button>
                </div>
              </div>

              <div
                v-else
                class="rr-assistant-chat__message-list"
              >
                <article
                  v-for="message in renderedMessages"
                  :key="message.id"
                  class="rr-assistant-chat__message"
                  :class="`rr-assistant-chat__message--${message.author}`"
                >
                  <div class="rr-assistant-chat__message-meta">
                    <strong>{{ message.author === 'assistant' ? 'RustRAG AI' : viewerUserName }}</strong>
                    <span>{{ messageTimeLabel(message) }}</span>
                  </div>

                  <div
                    class="rr-assistant-chat__bubble"
                    :class="{
                      'rr-assistant-chat__bubble--pending': message.pending,
                    }"
                  >
                    <template
                      v-if="
                        message.pending &&
                        message.author === 'assistant' &&
                        message.variant === 'progress'
                      "
                    >
                      <div class="rr-assistant-chat__thinking">
                        <div class="rr-assistant-chat__thinking-head">
                          <div class="rr-assistant-chat__thinking-dots">
                            <span />
                            <span />
                            <span />
                          </div>
                          <strong>{{ t('assistant.status.executingTitle') }}</strong>
                        </div>
                        <p>{{ t('assistant.status.executing') }}</p>
                        <div class="rr-assistant-chat__thinking-steps">
                          <span
                            v-for="step in pendingAssistantSteps"
                            :key="step.key"
                            class="rr-assistant-chat__thinking-step"
                            :class="`rr-assistant-chat__thinking-step--${step.state}`"
                          >
                            {{ step.label }}
                          </span>
                        </div>
                      </div>
                    </template>
                    <AssistantMarkdownContent
                      v-if="message.author === 'assistant'"
                      class="rr-assistant-chat__bubble-copy"
                      :content="message.content"
                      :class="{
                        'rr-assistant-chat__bubble-copy--streaming':
                          message.pending && message.variant === 'stream',
                      }"
                    />
                    <p
                      v-else
                      class="rr-assistant-chat__bubble-copy rr-assistant-chat__bubble-copy--user"
                    >
                      {{ message.content }}
                    </p>
                    <span
                      v-if="message.pending && message.variant === 'stream'"
                      class="rr-assistant-chat__stream-caret"
                    />
                  </div>
                </article>
              </div>
            </div>

            <div class="rr-assistant-chat__composer">
              <div class="rr-assistant-chat__composer-inner">
                <div
                  v-if="composerFiles.length > 0"
                  class="rr-assistant-chat__composer-files"
                >
                  <button
                    v-for="(file, index) in composerFiles"
                    :key="`${file.name}-${file.size}-${file.lastModified}`"
                    type="button"
                    class="rr-assistant-chat__composer-file"
                    @click="removeComposerFile(index)"
                  >
                    <span>{{ file.name }}</span>
                    <strong>x</strong>
                  </button>
                </div>

                <div class="rr-assistant-chat__composer-shell">
                  <button
                    type="button"
                    class="rr-assistant-chat__composer-icon"
                    :disabled="composerDisabled"
                    :aria-label="t('assistant.actions.attach')"
                    :title="t('assistant.actions.attach')"
                    @click="openComposerFilePicker"
                  >
                    <svg viewBox="0 0 18 18" aria-hidden="true">
                      <path
                        d="M6.5 9.5 10.8 5.2a2.6 2.6 0 1 1 3.7 3.7l-5.9 5.9a4.2 4.2 0 0 1-5.9-5.9l5.5-5.5"
                        fill="none"
                        stroke="currentColor"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        stroke-width="1.5"
                      />
                    </svg>
                  </button>
                  <textarea
                    ref="composerInputRef"
                    v-model="composerText"
                    class="rr-assistant-chat__composer-input"
                    :placeholder="t('assistant.chat.typeMessage')"
                    :disabled="composerDisabled"
                    rows="1"
                    @input="syncComposerHeight"
                    @keydown="handleComposerKeydown"
                  />

                  <input
                    ref="composerFileInputRef"
                    type="file"
                    hidden
                    multiple
                    :accept="acceptedFiles"
                    @change="handleComposerFiles"
                  >

                  <button
                    type="button"
                    class="rr-assistant-chat__composer-send"
                    :disabled="composerDisabled || (!composerText.trim() && composerFiles.length === 0)"
                    :aria-label="t('assistant.actions.send')"
                    :title="t('assistant.actions.send')"
                    @click="sendPrompt"
                  >
                    <svg viewBox="0 0 18 18" aria-hidden="true">
                      <path
                        d="M3 9h9"
                        fill="none"
                        stroke="currentColor"
                        stroke-linecap="round"
                        stroke-width="1.7"
                      />
                      <path
                        d="m9.5 4.5 4.5 4.5-4.5 4.5"
                        fill="none"
                        stroke="currentColor"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                        stroke-width="1.7"
                      />
                    </svg>
                  </button>
                </div>
              </div>
            </div>
          </section>
        </div>
      </section>

      <button
        v-if="compactAssistantLayout && showEvidencePanel"
        type="button"
        class="rr-assistant-page__context-backdrop"
        :aria-label="t('assistant.actions.hideContext')"
        @click="closeEvidencePanel"
      />

      <section
        v-if="showEvidencePanel"
        class="rr-assistant-page__context-shell"
        :class="{ 'rr-assistant-page__context-shell--overlay': compactAssistantLayout }"
      >
        <AssistantEvidencePanel
          :library-name="activeLibrary?.name ?? '—'"
          :executing="executingTurn || assistantResponseStreaming"
          :error="error"
          :execution="activeExecution"
          :bundle="activeBundle"
          :chunk-references="groundedChunkReferences"
          :entity-references="groundedEntityReferences"
          :relation-references="groundedRelationReferences"
          :evidence-references="groundedEvidenceReferences"
          :closable="compactAssistantLayout"
          @open-documents="openDocuments"
          @open-graph="openGraph"
          @close="closeEvidencePanel"
        />
      </section>
    </div>
  </div>
</template>
