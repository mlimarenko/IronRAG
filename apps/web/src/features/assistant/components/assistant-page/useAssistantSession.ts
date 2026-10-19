import { useCallback, useEffect, useMemo, useRef, useState, type SetStateAction } from 'react'
import { useMutation, useQuery, useQueryClient, type QueryKey } from '@tanstack/react-query'
import type { TFunction } from 'i18next'
import { toast } from 'sonner'
import {
  mapAssistantTurnToEvidence,
  mapAssistantMessage,
  mapAssistantSession,
} from '@/features/assistant/model/assistantAdapter'
import { queryApi, queries } from '@/shared/api'
import { getApiProblem } from '@/shared/api/runtime'
import type { AssistantSessionListItem, QuerySessionListResponse } from '@/shared/api/generated'
import { AssistantTurnFailedEventError } from '@/shared/api/query'
import type { AssistantTurnExecutionResponse, LlmContextDebugResponse } from '@/shared/api/query'
import { errorMessage } from '@/shared/lib/errorMessage'
import type {
  AssistantAgentActivityEvent,
  AssistantMessage,
  AssistantSession,
} from '@/shared/types'
import {
  applyTurnResultToMessages,
  createPendingAssistantMessage,
  createUserMessage,
  EMPTY_MESSAGES,
  isInProgressAssistantTurn,
  latestEvidenceFromMessages,
  resolveStateAction,
  sessionMessagesHydrationKey,
  serverTurnDurationMs,
  type RetryableAssistantTurn,
} from './assistantPageState'

type UseAssistantSessionParams = {
  workspaceId: string | undefined
  libraryId: string | undefined
  t: TFunction
}

type SendQuestionVariables = {
  existingSessionId: string | null
  libraryId: string
  optimisticSessionId: string | null
  pendingMessage: AssistantMessage
  questionText: string
  recoveryMessageStartIndex: number
  requestScope: string
  sessionsQueryKey: QueryKey
  userMessage: AssistantMessage
  workspaceId: string
}

type SendQuestionResult = {
  pendingMessageId: string
  result: AssistantTurnExecutionResponse
  sessionId: string
}

type SendQuestionContext = {
  previousActiveSession: string | null
  previousMessages: AssistantMessage[]
  previousSessions: QuerySessionListResponse | undefined
}

type RenameSessionVariables = {
  sessionId: string
  title: string
}

type LocalPendingTurn = {
  optimisticSessionId: string | null
  pendingMessage: AssistantMessage
  questionText: string
  requestScope: string
  sessionId: string | null
  userMessage: AssistantMessage
}

const ACTIVE_SESSION_STORAGE_PREFIX = 'ironrag.v1.assistant.active-session'
const ACTIVE_SESSION_POLL_INTERVAL_MS = 1000

type AssistantFailurePresentation = {
  inlineMessage: string
  retryDiagnosis: string | null
  unusableLibrary: boolean
}

export function classifyAssistantFailure(
  error: unknown,
  t: TFunction,
): AssistantFailurePresentation {
  const problem = getApiProblem(error)
  // A turn that fails mid-stream arrives as a stream event, not as a problem
  // response, and carries the same code. Reading only the problem body meant
  // every streamed failure — which is every failure the assistant screen
  // produces — landed on the generic apology below.
  const streamedCode =
    error instanceof AssistantTurnFailedEventError ? (error.code ?? undefined) : undefined
  const code = (problem?.code ?? streamedCode)?.toLowerCase()
  const isKnowledgeUnavailable =
    code === 'knowledge_not_ready' || code === 'empty_library' || code === 'empty-library'
  if (isKnowledgeUnavailable) {
    return {
      inlineMessage: t('assistant.libraryContentUnavailableInline'),
      retryDiagnosis: null,
      unusableLibrary: true,
    }
  }
  if (problem?.status === 503) {
    const message = t('assistant.temporarilyUnavailable')
    return { inlineMessage: message, retryDiagnosis: message, unusableLibrary: false }
  }
  if (problem && problem.status >= 400 && problem.status < 500) {
    const title = typeof problem.body.title === 'string' ? problem.body.title.trim() : ''
    const detail = typeof problem.body.detail === 'string' ? problem.body.detail.trim() : ''
    return {
      inlineMessage: [title, detail].filter(Boolean).join('\n\n') || t('assistant.requestRejected'),
      retryDiagnosis: null,
      unusableLibrary: false,
    }
  }
  const message = t('assistant.serverFailure')
  return { inlineMessage: message, retryDiagnosis: message, unusableLibrary: false }
}

function activeSessionStorageKey(scopeKey: string | null): string | null {
  return scopeKey ? `${ACTIVE_SESSION_STORAGE_PREFIX}:${scopeKey}` : null
}

function readActiveSession(scopeKey: string | null): string | null {
  const key = activeSessionStorageKey(scopeKey)
  if (!key || typeof window === 'undefined') return null
  try {
    const raw = window.localStorage.getItem(key)
    const parsed: unknown = raw ? JSON.parse(raw) : null
    return typeof parsed === 'string' && parsed.length > 0 ? parsed : null
  } catch {
    return null
  }
}

function writeActiveSession(scopeKey: string | null, sessionId: string | null) {
  const key = activeSessionStorageKey(scopeKey)
  if (!key || typeof window === 'undefined') return
  try {
    if (sessionId?.startsWith('optimistic-session-')) {
      window.localStorage.removeItem(key)
    } else if (sessionId) {
      window.localStorage.setItem(key, JSON.stringify(sessionId))
    } else {
      window.localStorage.removeItem(key)
    }
  } catch {
    // Persistence is optional; blocked or full storage must not break chat.
  }
}

function useScopedState<T>(
  scopeKey: string | null,
  initialValue: T,
): [T, (action: SetStateAction<T>) => void] {
  const [state, setState] = useState<{ scopeKey: string | null; value: T }>(() => ({
    scopeKey,
    value: initialValue,
  }))
  const value = state.scopeKey === scopeKey ? state.value : initialValue
  const setScopedState = useCallback(
    (action: SetStateAction<T>) => {
      setState((current) => {
        const previous = current.scopeKey === scopeKey ? current.value : initialValue
        return {
          scopeKey,
          value: resolveStateAction(action, previous),
        }
      })
    },
    [initialValue, scopeKey],
  )
  return [value, setScopedState]
}

/// Applies an item-level update to the session list page envelope the list
/// endpoint returns, keeping the cached shape identical to the wire shape.
function withSessionItems(
  current: QuerySessionListResponse | undefined,
  update: (items: AssistantSessionListItem[]) => AssistantSessionListItem[],
): QuerySessionListResponse {
  const items = update(current?.items ?? [])
  return { items, nextCursor: current?.nextCursor ?? null, total: items.length }
}

function withRenamedSession(
  current: QuerySessionListResponse | undefined,
  renamed: AssistantSessionListItem,
): QuerySessionListResponse {
  return withSessionItems(current, (items) =>
    items.map((session) => (session.id === renamed.id ? renamed : session)),
  )
}

function withoutSession(
  current: QuerySessionListResponse | undefined,
  sessionId: string,
): QuerySessionListResponse {
  return withSessionItems(current, (items) => items.filter((session) => session.id !== sessionId))
}

function withCreatedSession(
  current: QuerySessionListResponse | undefined,
  sessionItem: AssistantSessionListItem,
  optimisticSessionId: string | null,
): QuerySessionListResponse {
  return withSessionItems(current, (items) => [
    sessionItem,
    ...items.filter(
      (candidate) => candidate.id !== optimisticSessionId && candidate.id !== sessionItem.id,
    ),
  ])
}

function appendPendingActivity(
  message: AssistantMessage,
  event: AssistantAgentActivityEvent,
): AssistantMessage {
  const elapsedMs = event.elapsed_ms
  return {
    ...message,
    ...(typeof elapsedMs === 'number' && Number.isFinite(elapsedMs) && elapsedMs >= 0
      ? { elapsedMs }
      : {}),
    activityEvents: [...(message.activityEvents ?? []), event].slice(-24),
  }
}

function appendActivityToPendingMessage(
  messages: AssistantMessage[],
  pendingMessageId: string,
  event: AssistantAgentActivityEvent,
): AssistantMessage[] {
  return messages.map((message) =>
    message.id === pendingMessageId ? appendPendingActivity(message, event) : message,
  )
}

function isCurrentDebugRequest(
  requestId: number,
  currentRequestId: number,
  requestSession: string | null,
  activeSession: string | null,
): boolean {
  return requestId === currentRequestId && requestSession === activeSession
}

function hasPendingAssistantTurn(messages: AssistantMessage[]): boolean {
  return messages.some(isInProgressAssistantTurn)
}

function mergeHydratedMessagesWithLocalPendingTurn(
  messages: AssistantMessage[],
  sessionId: string,
  requestScope: string | null,
  pendingTurn: LocalPendingTurn | null,
): AssistantMessage[] {
  if (
    pendingTurn?.requestScope !== requestScope ||
    (pendingTurn?.sessionId !== sessionId && pendingTurn?.optimisticSessionId !== sessionId)
  )
    return messages
  if (messages.some((message) => message.id === pendingTurn.pendingMessage.id)) {
    return messages
  }

  const pendingQuestion = pendingTurn.questionText.trim()
  let matchingUserIndex = -1
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]
    if (message?.role === 'user' && message.content.trim() === pendingQuestion) {
      matchingUserIndex = index
      break
    }
  }

  if (matchingUserIndex >= 0) {
    const assistantAlreadyExists = messages
      .slice(matchingUserIndex + 1)
      .some((message) => message.role === 'assistant')
    if (assistantAlreadyExists) return messages
    return [
      ...messages.slice(0, matchingUserIndex + 1),
      pendingTurn.pendingMessage,
      ...messages.slice(matchingUserIndex + 1),
    ]
  }

  return [...messages, pendingTurn.userMessage, pendingTurn.pendingMessage]
}

function finalizedAssistantMessageFromResult(
  pendingMessage: AssistantMessage,
  result: AssistantTurnExecutionResponse,
  emptyAnswerText: string,
): AssistantMessage {
  const durationMs = serverTurnDurationMs(result)
  return {
    id: result.responseTurn?.id ?? pendingMessage.id,
    role: 'assistant',
    content: result.responseTurn?.contentText ?? emptyAnswerText,
    timestamp:
      result.runtimeSummary?.completedAt ??
      result.responseTurn?.createdAt ??
      pendingMessage.timestamp,
    ...(durationMs !== undefined ? { durationMs } : {}),
    executionId: result.responseTurn?.executionId ?? null,
    evidence: mapAssistantTurnToEvidence(result),
  }
}

function isOptionalDebugSnapshotMiss(error: unknown): boolean {
  if (typeof error !== 'object' || error === null || !('status' in error)) {
    return false
  }
  return error.status === 404
}

export function useAssistantSession({ workspaceId, libraryId, t }: UseAssistantSessionParams) {
  const queryClient = useQueryClient()
  const libraryScopeKey = workspaceId && libraryId ? `${workspaceId}:${libraryId}` : null
  const [activeSession, setActiveSessionState] = useScopedState<string | null>(
    libraryScopeKey,
    null,
  )
  const [messages, setMessages] = useScopedState<AssistantMessage[]>(
    libraryScopeKey,
    EMPTY_MESSAGES,
  )
  const [retryable, setRetryable] = useScopedState<RetryableAssistantTurn | null>(
    libraryScopeKey,
    null,
  )
  const [libraryContentUnavailable, setLibraryContentUnavailable] = useScopedState(
    libraryScopeKey,
    false,
  )
  const [sessionSearch, setSessionSearch] = useScopedState(libraryScopeKey, '')
  const [debugContext, setDebugContext] = useScopedState<LlmContextDebugResponse | null>(
    libraryScopeKey,
    null,
  )
  const [debugLoadingId, setDebugLoadingId] = useScopedState<string | null>(libraryScopeKey, null)
  const [debugError, setDebugError] = useScopedState<string | null>(libraryScopeKey, null)
  const [debugErrorExecutionId, setDebugErrorExecutionId] = useScopedState<string | null>(
    libraryScopeKey,
    null,
  )
  const [localPendingTurn, setLocalPendingTurn] = useScopedState<LocalPendingTurn | null>(
    libraryScopeKey,
    null,
  )
  const libraryScopeRef = useRef<string | null>(libraryScopeKey)
  const activeSessionRef = useRef<string | null>(activeSession)
  const debugRequestRef = useRef(0)
  const debugRequestsInFlightRef = useRef<Set<string>>(new Set())
  const unavailableDebugExecutionsRef = useRef<Set<string>>(new Set())
  const executingScopesRef = useRef<Set<string>>(new Set())
  const [executingScopes, setExecutingScopes] = useState<ReadonlySet<string>>(() => new Set())
  const hydratedSessionRef = useRef<string | null>(null)
  const [optimisticSessionId, setOptimisticSessionId] = useState<string | null>(null)
  const isScopeExecuting = useCallback(
    (scopeKey: string | null) => scopeKey !== null && executingScopesRef.current.has(scopeKey),
    [],
  )
  const setScopeExecuting = useCallback((scopeKey: string | null, executing: boolean) => {
    if (scopeKey === null) return
    const scopes = executingScopesRef.current
    if (scopes.has(scopeKey) === executing) return
    const next = new Set(scopes)
    if (executing) next.add(scopeKey)
    else next.delete(scopeKey)
    executingScopesRef.current = next
    setExecutingScopes(next)
  }, [])
  const isExecuting = libraryScopeKey !== null && executingScopes.has(libraryScopeKey)
  const sessionHasPendingAssistantTurn = useMemo(
    () => hasPendingAssistantTurn(messages),
    [messages],
  )

  const setActiveSession = useCallback(
    (action: SetStateAction<string | null>) => {
      setActiveSessionState((current) => {
        const next = resolveStateAction(action, current)
        writeActiveSession(libraryScopeKey, next)
        return next
      })
    },
    [libraryScopeKey, setActiveSessionState],
  )

  useEffect(() => {
    libraryScopeRef.current = libraryScopeKey
  }, [libraryScopeKey])

  useEffect(() => {
    const storedSessionId = readActiveSession(libraryScopeKey)
    hydratedSessionRef.current = null
    activeSessionRef.current = storedSessionId
    setActiveSessionState(storedSessionId)
  }, [libraryScopeKey, setActiveSessionState])

  useEffect(() => {
    activeSessionRef.current = activeSession
    debugRequestRef.current += 1
    setDebugContext(null)
    setDebugError(null)
    setDebugErrorExecutionId(null)
    setDebugLoadingId(null)
  }, [activeSession, setDebugContext, setDebugError, setDebugErrorExecutionId, setDebugLoadingId])

  const sessionsQueryOptions = queries.listQuerySessionsOptions({
    path: { libraryId: libraryId ?? '' },
  })

  const {
    data: sessionsData,
    error: sessionsError,
    isLoading: sessionsLoading,
    refetch: refetchSessions,
  } = useQuery({
    ...sessionsQueryOptions,
    enabled: !!libraryId && !!libraryScopeKey,
  })

  useEffect(() => {
    if (sessionsError) {
      toast.error(errorMessage(sessionsError, t('assistant.loadSessionsFailed')))
    }
  }, [sessionsError, t])

  const activeSessionIsOptimistic = activeSession !== null && activeSession === optimisticSessionId

  const { data: sessionData, error: sessionError } = useQuery({
    ...queries.getQuerySessionOptions({
      path: { sessionId: activeSession ?? '' },
    }),
    enabled: !!activeSession && !activeSessionIsOptimistic && !!libraryScopeKey,
    refetchInterval: sessionHasPendingAssistantTurn ? ACTIVE_SESSION_POLL_INTERVAL_MS : false,
    refetchIntervalInBackground: true,
  })

  useEffect(() => {
    if (!activeSession) {
      hydratedSessionRef.current = null
      return
    }
    if (!sessionData) {
      if (sessionError) {
        const errorHydrationKey = `error:${activeSession}`
        hydratedSessionRef.current = errorHydrationKey
        queueMicrotask(() => {
          if (hydratedSessionRef.current === errorHydrationKey) {
            setMessages(EMPTY_MESSAGES)
          }
        })
      }
      return
    }
    const data = sessionData
    if (data.session.libraryId !== libraryId) return
    const sessionId = activeSession
    const nextMessages = mergeHydratedMessagesWithLocalPendingTurn(
      data.messages.map(mapAssistantMessage),
      sessionId,
      libraryScopeKey,
      localPendingTurn,
    )
    const hydrationKey = sessionMessagesHydrationKey(sessionId, nextMessages)
    if (hydratedSessionRef.current === hydrationKey) return
    hydratedSessionRef.current = hydrationKey
    queueMicrotask(() => {
      if (hydratedSessionRef.current === hydrationKey && activeSessionRef.current === sessionId) {
        setMessages(nextMessages)
      }
    })
  }, [
    activeSession,
    libraryId,
    libraryScopeKey,
    localPendingTurn,
    sessionData,
    sessionError,
    setMessages,
  ])

  const sessions = useMemo<AssistantSession[]>(() => {
    if (!sessionsData || !libraryId) return []
    return sessionsData.items
      .map(mapAssistantSession)
      .filter((session) => session.libraryId === libraryId)
  }, [libraryId, sessionsData])

  const renameSessionMutation = useMutation({
    mutationKey: ['assistant', 'rename-session', libraryId],
    mutationFn: ({ sessionId, title }: RenameSessionVariables) =>
      queryApi.renameSession(sessionId, title),
    onSuccess: (renamed) => {
      queryClient.setQueryData<QuerySessionListResponse>(sessionsQueryOptions.queryKey, (current) =>
        withRenamedSession(current, renamed),
      )
    },
    onError: (error) => {
      toast.error(
        t('assistant.mutations.renameSession.failed', {
          error: errorMessage(error, t('assistant.unknownError')),
        }),
      )
    },
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: sessionsQueryOptions.queryKey })
    },
  })
  const { isPending: isRenameSessionPending, mutate: mutateRenameSession } = renameSessionMutation

  const deleteSessionMutation = useMutation({
    mutationKey: ['assistant', 'delete-session', libraryId],
    mutationFn: (sessionId: string) => queryApi.deleteSession(sessionId),
    onSuccess: (_result, sessionId) => {
      queryClient.setQueryData<QuerySessionListResponse>(sessionsQueryOptions.queryKey, (current) =>
        withoutSession(current, sessionId),
      )
      queryClient.removeQueries({
        queryKey: queries.getQuerySessionOptions({ path: { sessionId } }).queryKey,
      })
      if (activeSessionRef.current === sessionId) {
        activeSessionRef.current = null
        hydratedSessionRef.current = null
        setActiveSession(null)
        setMessages(EMPTY_MESSAGES)
        setLocalPendingTurn(null)
        setRetryable(null)
        setLibraryContentUnavailable(false)
        setDebugContext(null)
        setDebugError(null)
        setDebugErrorExecutionId(null)
      }
    },
    onError: (error) => {
      toast.error(
        t('assistant.mutations.deleteSession.failed', {
          error: errorMessage(error, t('assistant.unknownError')),
        }),
      )
    },
    onSettled: async () => {
      await queryClient.invalidateQueries({ queryKey: sessionsQueryOptions.queryKey })
    },
  })
  const { isPending: isDeleteSessionPending, mutate: mutateDeleteSession } = deleteSessionMutation

  const renameSession = useCallback(
    (sessionId: string, rawTitle: string) => {
      const title = rawTitle.trim()
      if (!title || isScopeExecuting(libraryScopeRef.current) || isRenameSessionPending) return
      mutateRenameSession({ sessionId, title })
    },
    [isRenameSessionPending, isScopeExecuting, mutateRenameSession],
  )

  const deleteSession = useCallback(
    (sessionId: string) => {
      if (isScopeExecuting(libraryScopeRef.current) || isDeleteSessionPending) return
      mutateDeleteSession(sessionId)
    },
    [isDeleteSessionPending, isScopeExecuting, mutateDeleteSession],
  )

  useEffect(() => {
    if (!sessionsData || !activeSession || activeSessionIsOptimistic) return
    if (sessions.some((session) => session.id === activeSession)) return
    activeSessionRef.current = null
    setActiveSession(null)
  }, [activeSession, activeSessionIsOptimistic, sessions, sessionsData, setActiveSession])

  const sendQuestionMutation = useMutation<
    SendQuestionResult,
    unknown,
    SendQuestionVariables,
    SendQuestionContext
  >({
    mutationKey: ['assistant', 'send-turn', libraryId],
    scope: { id: `assistant:send-turn:${libraryScopeKey ?? 'none'}` },
    mutationFn: async (variables) => {
      let sessionId = variables.existingSessionId
      if (!sessionId) {
        const session = await queryApi.createSession(variables.workspaceId, variables.libraryId)
        sessionId = session.id
        hydratedSessionRef.current = sessionId
        setOptimisticSessionId(null)
        const sessionItem: AssistantSessionListItem = {
          ...session,
          title: session.title || variables.questionText,
          turnCount: 1,
        }
        if (libraryScopeRef.current === variables.requestScope) {
          activeSessionRef.current = sessionId
          setActiveSession(sessionId)
          setLocalPendingTurn((current) =>
            current?.pendingMessage.id === variables.pendingMessage.id
              ? { ...current, sessionId }
              : current,
          )
        }
        queryClient.setQueryData<QuerySessionListResponse>(variables.sessionsQueryKey, (current) =>
          withCreatedSession(current, sessionItem, variables.optimisticSessionId),
        )
      }

      const result = await queryApi.createTurnStream(
        sessionId,
        variables.questionText,
        variables.recoveryMessageStartIndex,
        (event) => {
          if (
            libraryScopeRef.current !== variables.requestScope ||
            activeSessionRef.current !== sessionId
          ) {
            return
          }
          setMessages((current) =>
            appendActivityToPendingMessage(current, variables.pendingMessage.id, event),
          )
        },
      )

      return {
        pendingMessageId: variables.pendingMessage.id,
        result,
        sessionId,
      }
    },
    onMutate: async (variables) => {
      await queryClient.cancelQueries({ queryKey: variables.sessionsQueryKey })
      const previousSessions = queryClient.getQueryData<QuerySessionListResponse>(
        variables.sessionsQueryKey,
      )
      const previousActiveSession = activeSessionRef.current
      const previousMessages = messages

      if (libraryScopeRef.current !== variables.requestScope) {
        return {
          previousActiveSession,
          previousMessages,
          previousSessions,
        }
      }

      if (variables.optimisticSessionId) {
        setOptimisticSessionId(variables.optimisticSessionId)
        activeSessionRef.current = variables.optimisticSessionId
        setActiveSession(variables.optimisticSessionId)
        queryClient.setQueryData<QuerySessionListResponse>(variables.sessionsQueryKey, (current) =>
          withSessionItems(current, (items) => [
            {
              conversationState: 'active',
              createdAt: variables.userMessage.timestamp,
              id: variables.optimisticSessionId as string,
              libraryId: variables.libraryId,
              title: variables.questionText,
              turnCount: 1,
              updatedAt: variables.userMessage.timestamp,
              workspaceId: variables.workspaceId,
            },
            ...items,
          ]),
        )
      }

      setMessages((current) => [...current, variables.userMessage, variables.pendingMessage])
      setLocalPendingTurn({
        optimisticSessionId: variables.optimisticSessionId,
        pendingMessage: variables.pendingMessage,
        questionText: variables.questionText,
        requestScope: variables.requestScope,
        sessionId: variables.existingSessionId ?? variables.optimisticSessionId,
        userMessage: variables.userMessage,
      })
      setRetryable(null)
      return {
        previousActiveSession,
        previousMessages,
        previousSessions,
      }
    },
    onSuccess: async ({ pendingMessageId, result, sessionId }, variables) => {
      await queryClient.invalidateQueries({
        queryKey: queries.getQuerySessionOptions({
          path: { sessionId },
        }).queryKey,
      })
      if (
        libraryScopeRef.current === variables.requestScope &&
        activeSessionRef.current === sessionId
      ) {
        setMessages((current) =>
          applyTurnResultToMessages(
            current,
            pendingMessageId,
            result,
            t('assistant.noResponseGenerated'),
          ),
        )
        setLocalPendingTurn((current) =>
          current?.pendingMessage.id === pendingMessageId
            ? {
                ...current,
                pendingMessage: finalizedAssistantMessageFromResult(
                  current.pendingMessage,
                  result,
                  t('assistant.noResponseGenerated'),
                ),
                sessionId,
              }
            : current,
        )
        setRetryable(null)
      }
    },
    onError: (err, variables, context) => {
      const failure = classifyAssistantFailure(err, t)
      const inlineError = failure.inlineMessage
      if (context) {
        if (libraryScopeRef.current === variables.requestScope) {
          const activeIsUnresolvedOptimistic =
            variables.optimisticSessionId !== null &&
            activeSessionRef.current === variables.optimisticSessionId
          if (activeIsUnresolvedOptimistic) {
            queryClient.setQueryData(variables.sessionsQueryKey, context.previousSessions)
            setOptimisticSessionId(null)
            activeSessionRef.current = context.previousActiveSession
            setActiveSession(context.previousActiveSession)
          }
          setMessages((current) => {
            const hasPendingMessage = current.some(
              (message) => message.id === variables.pendingMessage.id,
            )
            if (!hasPendingMessage) return context.previousMessages
            return current.map((message) =>
              message.id === variables.pendingMessage.id
                ? {
                    ...message,
                    content: inlineError,
                  }
                : message,
            )
          })
          setRetryable(
            failure.retryDiagnosis
              ? { question: variables.questionText, diagnosis: failure.retryDiagnosis }
              : null,
          )
          setLibraryContentUnavailable(failure.unusableLibrary)
          setLocalPendingTurn((current) =>
            current?.pendingMessage.id === variables.pendingMessage.id
              ? {
                  ...current,
                  pendingMessage: {
                    ...current.pendingMessage,
                    content: inlineError,
                  },
                }
              : current,
          )
        }
      }
      // The failed turn itself carries the error and its retry action now;
      // a global toast repeated the same sentence in a second surface.
    },
    onSettled: async (_data, _err, variables) => {
      setScopeExecuting(variables ? variables.requestScope : libraryScopeRef.current, false)
      if (variables) {
        await queryClient.invalidateQueries({ queryKey: variables.sessionsQueryKey })
      }
      if (!variables || libraryScopeRef.current === variables.requestScope) {
        await refetchSessions()
      }
    },
  })
  const { mutate: mutateSendQuestion } = sendQuestionMutation

  const selectSession = useCallback(
    (sessionId: string) => {
      if (isScopeExecuting(libraryScopeRef.current)) return
      const sessionExists = sessions.some(
        (candidate) => candidate.id === sessionId && candidate.libraryId === libraryId,
      )
      if (!sessionExists) return
      activeSessionRef.current = sessionId
      setActiveSession(sessionId)
    },
    [isScopeExecuting, libraryId, sessions, setActiveSession],
  )

  const newSession = useCallback(() => {
    if (isScopeExecuting(libraryScopeRef.current)) return
    activeSessionRef.current = null
    setActiveSession(null)
    setMessages([])
    setLocalPendingTurn(null)
    setRetryable(null)
    setDebugContext(null)
    setDebugError(null)
    setDebugErrorExecutionId(null)
  }, [
    isScopeExecuting,
    setActiveSession,
    setDebugContext,
    setDebugError,
    setDebugErrorExecutionId,
    setMessages,
    setLocalPendingTurn,
    setRetryable,
  ])

  const openDebugFor = useCallback(
    async (executionId: string) => {
      if (unavailableDebugExecutionsRef.current.has(executionId)) {
        setDebugContext(null)
        setDebugError(t('assistant.llmContextUnavailable'))
        setDebugErrorExecutionId(executionId)
        setDebugLoadingId(null)
        return
      }
      if (debugRequestsInFlightRef.current.has(executionId)) return

      debugRequestsInFlightRef.current.add(executionId)
      const requestId = debugRequestRef.current + 1
      debugRequestRef.current = requestId
      const requestSession = activeSessionRef.current
      const isCurrentRequest = () =>
        isCurrentDebugRequest(
          requestId,
          debugRequestRef.current,
          requestSession,
          activeSessionRef.current,
        )

      setDebugLoadingId(executionId)
      setDebugError(null)
      setDebugErrorExecutionId(null)
      try {
        const snapshot = await queryApi.getExecutionLlmContext(executionId)
        if (!isCurrentRequest()) return
        setDebugContext(snapshot)
        setDebugError(null)
        setDebugErrorExecutionId(null)
      } catch (err: unknown) {
        if (!isCurrentRequest()) return
        const snapshotMissing = isOptionalDebugSnapshotMiss(err)
        const message = snapshotMissing
          ? t('assistant.llmContextUnavailable')
          : errorMessage(err, t('assistant.llmContextUnavailable'))
        if (snapshotMissing) unavailableDebugExecutionsRef.current.add(executionId)
        setDebugContext(null)
        setDebugError(message)
        setDebugErrorExecutionId(executionId)
        if (!snapshotMissing) toast.error(message)
      } finally {
        debugRequestsInFlightRef.current.delete(executionId)
        if (isCurrentRequest()) setDebugLoadingId(null)
      }
    },
    [setDebugContext, setDebugError, setDebugErrorExecutionId, setDebugLoadingId, t],
  )

  const sendQuestion = useCallback(
    (rawQuestion: string): boolean => {
      const questionText = rawQuestion.trim()
      if (
        !questionText ||
        !workspaceId ||
        !libraryId ||
        !libraryScopeKey ||
        isScopeExecuting(libraryScopeKey) ||
        sessionHasPendingAssistantTurn
      ) {
        return false
      }

      const requestScope = libraryScopeKey
      setScopeExecuting(requestScope, true)
      const now = Date.now()
      const pendingMessage = createPendingAssistantMessage(now)
      const currentSessionId = activeSessionRef.current
      const existingSessionId = sessions.some(
        (session) => session.id === currentSessionId && session.libraryId === libraryId,
      )
        ? currentSessionId
        : null

      mutateSendQuestion({
        existingSessionId,
        libraryId,
        optimisticSessionId: existingSessionId ? null : `optimistic-session-${now}`,
        pendingMessage,
        questionText,
        recoveryMessageStartIndex: messages.length,
        requestScope,
        sessionsQueryKey: sessionsQueryOptions.queryKey,
        userMessage: createUserMessage(questionText, now),
        workspaceId,
      })

      return true
    },
    [
      isScopeExecuting,
      libraryId,
      libraryScopeKey,
      messages.length,
      mutateSendQuestion,
      sessionHasPendingAssistantTurn,
      sessions,
      sessionsQueryOptions.queryKey,
      setScopeExecuting,
      workspaceId,
    ],
  )

  const prepareRetry = useCallback(() => {
    if (!retryable) return null
    setRetryable(null)
    return retryable.question
  }, [retryable, setRetryable])

  const latestEvidence = useMemo(() => latestEvidenceFromMessages(messages), [messages])

  return {
    activeSession,
    debugContext,
    debugError,
    debugErrorExecutionId,
    debugLoadingId,
    deleteSession,
    isExecuting: isExecuting || sessionHasPendingAssistantTurn,
    isSessionMutationPending: isRenameSessionPending || isDeleteSessionPending,
    latestEvidence,
    libraryContentUnavailable,
    messages,
    newSession,
    openDebugFor,
    prepareRetry,
    renameSession,
    retryable,
    selectSession,
    sessionSearch,
    sessionsError,
    sessionsLoading,
    sessions,
    setDebugContext,
    setDebugError,
    setDebugErrorExecutionId,
    setSessionSearch,
    refetchSessions,
    sendQuestion,
  }
}
