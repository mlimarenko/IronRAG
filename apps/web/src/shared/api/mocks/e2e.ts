import { http, HttpResponse, type HttpHandler } from 'msw'

import type {
  AssistantSessionListItem,
  AssistantHydratedConversation,
  GetLibraryDashboardResponse,
  ListQuerySessionsResponse,
  LoginIamSessionResponse,
} from '../generated'
import { iamSession, opsLibraryDashboard } from './fixtures'

type SessionResolvePayload = {
  bootstrapStatus: { setupRequired: boolean }
  locale: 'en'
  me: {
    principal: { displayLabel: string; id: string }
    user: { displayName: string; login: string }
  } | null
  message: string | null
  mode: 'authenticated' | 'guest'
  session: { expiresAt: string; id: string } | null
  shellBootstrap: {
    libraries: Array<{
      id: string
      ingestionReady: boolean
      missingBindingPurposes: string[]
      name: string
      slug: string
      workspaceId: string
    }>
    workspaces: Array<{ id: string; name: string; slug: string }>
  } | null
}

export type BrowserMockConfig = {
  authenticated?: boolean
  bootstrapRequired?: boolean
  dashboard?: GetLibraryDashboardResponse
  queryConversations?: Record<string, AssistantHydratedConversation>
  querySessions?: AssistantSessionListItem[]
  session?: LoginIamSessionResponse
}

declare global {
  interface Window {
    __IRONRAG_E2E_MOCKS__?: BrowserMockConfig
    __IRONRAG_MSW_READY__?: boolean
  }
}

const WORKSPACE_ID = 'workspace-alpha'
const LIBRARY_ID = 'library-demo-1'

function resolveSessionPayload(
  authenticated: boolean,
  session: LoginIamSessionResponse,
  bootstrapRequired: boolean,
): SessionResolvePayload {
  if (!authenticated) {
    return {
      bootstrapStatus: { setupRequired: bootstrapRequired },
      locale: 'en',
      me: null,
      message: null,
      mode: 'guest',
      session: null,
      shellBootstrap: null,
    }
  }

  return {
    bootstrapStatus: { setupRequired: false },
    locale: 'en',
    me: {
      principal: {
        displayLabel: session.user.displayName,
        id: session.user.principalId,
      },
      user: {
        displayName: session.user.displayName,
        login: session.user.login,
      },
    },
    message: null,
    mode: 'authenticated',
    session: {
      expiresAt: session.expiresAt,
      id: session.sessionId,
    },
    shellBootstrap: {
      libraries: [
        {
          id: LIBRARY_ID,
          ingestionReady: true,
          missingBindingPurposes: [],
          name: 'Default Library',
          slug: 'default-library',
          workspaceId: WORKSPACE_ID,
        },
      ],
      workspaces: [
        {
          id: WORKSPACE_ID,
          name: 'Default Workspace',
          slug: 'default',
        },
      ],
    },
  }
}

export function createBrowserMockHandlers(config: BrowserMockConfig = {}): HttpHandler[] {
  const session = config.session ?? iamSession()
  const dashboard = config.dashboard ?? opsLibraryDashboard()
  const queryConversations = { ...config.queryConversations }
  let querySessions = [...(config.querySessions ?? [])]
  let bootstrapRequired = config.bootstrapRequired ?? false
  let authenticated = config.authenticated ?? false
  const bootstrapStatus = () => ({
    setupRequired: bootstrapRequired,
    aiSetup: bootstrapRequired
      ? {
          bindingBundles: [
            {
              providerCatalogId: 'provider-hosted-router',
              providerKind: 'hosted-router',
              displayName: 'Hosted Router',
              credentialSource: 'missing',
              defaultBaseUrl: 'https://router.example/api/v1',
              credentialPolicy: {
                apiKeyRequired: true,
                baseUrlRequired: false,
                baseUrlMode: 'fixed',
                validationMode: 'model_list',
              },
              baseUrlPolicy: {
                allowOverride: false,
                requireHttps: true,
                allowPrivateNetwork: false,
                trimSuffixes: [],
              },
              modelDiscovery: {
                mode: 'credential',
                paths: [
                  { capabilityKind: 'chat', path: '/models' },
                  { capabilityKind: 'embedding', path: '/embeddings/models' },
                ],
              },
              capabilities: {},
              runtime: {
                kind: 'openai_compatible',
                authScheme: 'bearer',
                chatPath: '/chat/completions',
                embeddingsPath: '/embeddings',
                modelsPath: '/models',
                structuredOutput: 'json_object',
                tokenLimitParameter: 'max_tokens',
              },
              uiHints: {},
              bindings: [
                {
                  bindingPurpose: 'extract_graph',
                  modelCatalogId: 'model-hosted-router-chat',
                  modelName: 'hosted/chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'embed_chunk',
                  modelCatalogId: 'model-hosted-router-embedding',
                  modelName: 'hosted/text-embedding-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_compile',
                  modelCatalogId: 'model-hosted-router-chat',
                  modelName: 'hosted/chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_answer',
                  modelCatalogId: 'model-hosted-router-answer',
                  modelName: 'hosted/chat-answer',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'extract_text',
                  modelCatalogId: 'model-hosted-router-answer',
                  modelName: 'hosted/chat-answer',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'agent',
                  modelCatalogId: 'model-hosted-router-answer',
                  modelName: 'hosted/chat-answer',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
              ],
            },
            {
              providerCatalogId: 'provider-local-runtime',
              providerKind: 'local-runtime',
              displayName: 'Local Runtime',
              credentialSource: 'missing',
              defaultBaseUrl: 'http://127.0.0.1:18080/v1',
              credentialPolicy: {
                apiKeyRequired: false,
                baseUrlRequired: true,
                baseUrlMode: 'required',
                validationMode: 'model_list',
              },
              baseUrlPolicy: {
                allowOverride: true,
                requireHttps: false,
                allowPrivateNetwork: true,
                trimSuffixes: ['/v1'],
              },
              modelDiscovery: {
                mode: 'credential',
                paths: [
                  { capabilityKind: 'chat', path: '/models' },
                  { capabilityKind: 'embedding', path: '/models' },
                ],
              },
              capabilities: {},
              runtime: {
                kind: 'openai_compatible',
                authScheme: 'none',
                chatPath: '/chat/completions',
                embeddingsPath: '/embeddings',
                modelsPath: '/models',
                structuredOutput: 'json_object',
                tokenLimitParameter: 'max_tokens',
              },
              uiHints: {},
              bindings: [
                {
                  bindingPurpose: 'extract_graph',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'embed_chunk',
                  modelCatalogId: 'model-local-runtime-embedding',
                  modelName: 'local-embedding-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_compile',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'query_answer',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'extract_text',
                  modelCatalogId: 'model-local-runtime-vision',
                  modelName: 'local-vision-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
                {
                  bindingPurpose: 'agent',
                  modelCatalogId: 'model-local-runtime-chat',
                  modelName: 'local-chat-small',
                  systemPrompt: null,
                  temperature: null,
                  topP: null,
                  maxOutputTokensOverride: null,
                },
              ],
            },
          ],
        }
      : null,
  })

  return [
    http.get('/v1/iam/session/resolve', () =>
      HttpResponse.json(resolveSessionPayload(authenticated, session, bootstrapRequired)),
    ),
    http.get('/v1/iam/bootstrap/status', () => HttpResponse.json(bootstrapStatus())),
    http.post('/v1/iam/bootstrap/setup', async () => {
      bootstrapRequired = false
      authenticated = true
      return HttpResponse.json(session)
    }),
    http.post('/v1/iam/session/login', () => {
      authenticated = true
      return HttpResponse.json(session)
    }),
    http.post('/v1/iam/session/logout', () => {
      authenticated = false
      return HttpResponse.json({})
    }),
    http.get('/v1/ops/libraries/:libraryId/dashboard', () => HttpResponse.json(dashboard)),
    http.get('/v1/query/libraries/:libraryId/sessions', () =>
      HttpResponse.json({
        items: querySessions,
        nextCursor: null,
        total: querySessions.length,
      } satisfies ListQuerySessionsResponse),
    ),
    http.patch('/v1/query/sessions/:sessionId', async ({ params, request }) => {
      const sessionId = String(params.sessionId)
      const current = querySessions.find((session) => session.id === sessionId)
      if (!current) {
        return HttpResponse.json({ message: 'query session not found' }, { status: 404 })
      }

      const payload: unknown = await request.json()
      const rawTitle =
        typeof payload === 'object' && payload !== null && 'title' in payload
          ? (payload as { title?: unknown }).title
          : undefined
      const title = typeof rawTitle === 'string' ? rawTitle.trim().split(/\s+/u).join(' ') : ''
      if (!title || Array.from(title).length > 72) {
        return HttpResponse.json({ message: 'invalid query session title' }, { status: 400 })
      }

      const updatedAt = new Date().toISOString()
      querySessions = querySessions.map((session) =>
        session.id === sessionId ? { ...session, title, updatedAt } : session,
      )
      const hydrated = queryConversations[sessionId]
      if (hydrated) {
        queryConversations[sessionId] = {
          ...hydrated,
          session: { ...hydrated.session, title, updatedAt },
        }
      }

      return HttpResponse.json({ ...current, title, updatedAt })
    }),
    http.delete('/v1/query/sessions/:sessionId', ({ params }) => {
      const sessionId = String(params.sessionId)
      const exists = querySessions.some((session) => session.id === sessionId)
      if (!exists) {
        return HttpResponse.json({ message: 'query session not found' }, { status: 404 })
      }
      querySessions = querySessions.filter((session) => session.id !== sessionId)
      delete queryConversations[sessionId]
      return new HttpResponse(null, { status: 204 })
    }),
    http.get('/v1/query/sessions/:sessionId', ({ params }) => {
      const conversation = queryConversations[String(params.sessionId)]
      return conversation
        ? HttpResponse.json(conversation)
        : HttpResponse.json({ message: 'query session not found' }, { status: 404 })
    }),
    http.get('/v1/version/update', () =>
      HttpResponse.json({
        checkedAt: '2026-05-05T15:30:00.000Z',
        currentVersion: '0.4.1',
        latestVersion: null,
        releaseUrl: null,
        repositoryUrl: 'https://github.com/mlimarenko/IronRAG',
        status: 'up_to_date',
      }),
    ),
  ]
}
