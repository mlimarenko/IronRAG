import { describe, expect, it, vi } from 'vitest'

import {
  adminApi,
  adminModelCatalogOptions,
  parseModelCatalogResponse,
  parseProviderCatalogResponse,
} from '@/shared/api/admin'
import type {
  AiAccountResponse,
  AiBindingResponse,
  AuditEventPageResponse,
  AuditEventResponse,
  ModelCatalogEntryResponse,
  OpsLibraryStateResponse,
  TokenResponse,
} from '@/shared/api/generated'
import { mapAudit, mapAuditPage, mapOps, mapToken } from './adminAdapter'
import { mapAccountList, mapBindingList, mapModelList, mapProviderList } from './aiAdapter'

describe('mapToken', () => {
  it('maps generated token responses without a raw shadow type', () => {
    const token = mapToken({
      principalId: 'principal-1',
      label: 'Ops token',
      tokenPrefix: 'irr_abc',
      status: 'active',
      expiresAt: '2026-05-01T00:00:00Z',
      lastUsedAt: '2026-04-10T00:00:00Z',
      issuer: {
        principalId: 'admin-1',
        displayLabel: 'Admin',
      },
      scope: {
        kind: 'library',
        workspace: { id: 'workspace-1', displayName: 'Workspace 1' },
        libraries: [{ id: 'library-1', workspaceId: 'workspace-1', displayName: 'Library 1' }],
      },
      grants: [
        {
          resourceKind: 'library',
          resourceId: 'library-1',
          permissionKind: 'library_read',
          workspace: { id: 'workspace-1', displayName: 'Workspace 1' },
          library: { id: 'library-1', workspaceId: 'workspace-1', displayName: 'Library 1' },
        },
      ],
    } satisfies TokenResponse)

    expect(token).toMatchObject({
      id: 'principal-1',
      label: 'Ops token',
      status: 'active',
      scope: {
        kind: 'library',
        workspace: { id: 'workspace-1', displayName: 'Workspace 1' },
        libraries: [{ id: 'library-1', workspaceId: 'workspace-1', displayName: 'Library 1' }],
      },
      grants: [{ resourceKind: 'library', permission: 'library_read' }],
    })
  })

  it('rejects malformed token status instead of rewriting it', () => {
    expect(() =>
      mapToken({
        principalId: 'principal-1',
        label: 'Bad token',
        tokenPrefix: 'irr_bad',
        status: 'enabled',
        scope: { kind: 'system', libraries: [] },
        grants: [],
      }),
    ).toThrow('invalid status')
  })
})

describe('mapOps', () => {
  it('maps generated operations responses without optional raw defaults', () => {
    const ops = mapOps({
      state: {
        libraryId: 'library-1',
        queueDepth: 2,
        runningAttempts: 1,
        readableDocumentCount: 10,
        failedDocumentCount: 1,
        degradedState: 'processing',
        knowledgeGenerationState: 'graph_ready',
        lastRecomputedAt: '2026-04-10T10:00:00Z',
      },
      warnings: [
        {
          id: 'warning-1',
          libraryId: 'library-1',
          warningKind: 'index_lag',
          severity: 'warning',
          createdAt: '2026-04-10T10:01:00Z',
          resolvedAt: null,
        },
      ],
      knowledgeGenerations: [],
    } satisfies OpsLibraryStateResponse)

    expect(ops).toMatchObject({
      queueDepth: 2,
      runningAttempts: 1,
      readableDocCount: 10,
      failedDocCount: 1,
      status: 'processing',
      warnings: [{ id: 'warning-1', warningKind: 'index_lag' }],
    })
  })
})

describe('mapAudit', () => {
  it('maps assistant call summaries from the audit payload', () => {
    const audit = mapAudit({
      id: 'evt-1',
      actionKind: 'query.execution.run',
      resultKind: 'succeeded',
      surfaceKind: 'mcp',
      createdAt: '2026-04-17T10:00:00Z',
      redactedMessage: 'assistant call completed',
      actorPrincipalId: 'principal-1',
      actorPrincipal: {
        id: 'principal-1',
        principalKind: 'user',
        status: 'active',
        displayLabel: 'Operator One',
        login: 'operator.one',
        displayName: 'Operator One',
        role: 'operator',
      },
      subjects: [{ auditEventId: 'evt-1', subjectKind: 'query_execution', subjectId: 'exec-1' }],
      assistantCall: {
        queryExecutionId: 'exec-1',
        conversationId: 'conv-1',
        runtimeExecutionId: 'run-1',
        models: [{ providerKind: 'provider_alpha', modelName: 'alpha-chat' }],
        totalCost: '0.0123',
        currencyCode: 'USD',
        providerCallCount: 2,
      },
    } satisfies AuditEventResponse)

    expect(audit.assistantCall).toEqual({
      queryExecutionId: 'exec-1',
      conversationId: 'conv-1',
      runtimeExecutionId: 'run-1',
      models: [{ providerKind: 'provider_alpha', modelName: 'alpha-chat' }],
      totalCost: '0.0123',
      currencyCode: 'USD',
      providerCallCount: 2,
    })
    expect(audit.actor).toBe('Operator One (operator.one)')
  })

  it('maps generated audit pages canonically', () => {
    const page = mapAuditPage({
      items: [
        {
          id: 'evt-1',
          actionKind: 'token.mint',
          resultKind: 'succeeded',
          surfaceKind: 'rest',
          createdAt: '2026-04-17T10:00:00Z',
          redactedMessage: 'token minted',
          actorPrincipalId: null,
          subjects: [{ auditEventId: 'evt-1', subjectKind: 'token', subjectId: 'principal-1' }],
          assistantCall: null,
        },
      ],
      total: 1,
      limit: 50,
      offset: 0,
    } satisfies AuditEventPageResponse)

    expect(page).toMatchObject({
      total: 1,
      limit: 50,
      offset: 0,
      items: [{ id: 'evt-1', action: 'token.mint', resultKind: 'succeeded' }],
    })
  })
})

describe('mapProviderList', () => {
  it('maps provider metadata and keeps generic derived conveniences', () => {
    expect(
      mapProviderList([
        {
          id: 'provider-alpha',
          providerKind: 'provider_alpha',
          displayName: 'Provider Alpha',
          apiStyle: 'openai_compatible',
          lifecycleState: 'active',
          defaultBaseUrl: 'https://alpha.example/v1',
          apiKeyRequired: true,
          baseUrlRequired: false,
          credentialPolicy: {
            apiKeyRequired: true,
            baseUrlRequired: true,
            baseUrlMode: 'required',
            validationMode: 'model_list',
          },
          baseUrlPolicy: {
            allowOverride: false,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: ['/v1'],
          },
          modelDiscovery: {
            mode: 'credential',
            paths: [{ capabilityKind: 'chat', path: '/models' }],
          },
          capabilities: {
            chat: 'supported',
            embeddings: 'unsupported',
            modelDiscovery: 'supported',
            streaming: 'unknown',
            tools: 'unsupported',
            vision: 'unsupported',
          },
          runtime: {
            kind: 'compatible_chat',
            authScheme: 'bearer',
            chatPath: '/chat/completions',
            modelsPath: '/models',
            structuredOutput: 'json_object',
            tokenLimitParameter: 'max_tokens',
          },
          uiHints: { baseUrlHint: 'Use the hosted endpoint.' },
        },
      ]),
    ).toMatchObject([
      {
        id: 'provider-alpha',
        kind: 'provider_alpha',
        apiKeyRequired: true,
        baseUrlRequired: true,
        credentialPolicy: {
          baseUrlMode: 'required',
        },
        baseUrlPolicy: {
          allowOverride: false,
          trimSuffixes: ['/v1'],
        },
        modelDiscovery: {
          mode: 'credential',
          paths: [{ capabilityKind: 'chat', path: '/models' }],
        },
        capabilities: { chat: 'supported' },
        runtime: { kind: 'compatible_chat' },
        uiHints: { baseUrlHint: 'Use the hosted endpoint.' },
      },
    ])
  })

  it('does not invent provider credential source from provider catalog metadata', () => {
    const providers = mapProviderList([
      {
        id: 'provider-alpha',
        providerKind: 'provider_alpha',
        displayName: 'Provider Alpha',
        apiStyle: 'openai_compatible',
        lifecycleState: 'active',
        apiKeyRequired: true,
        baseUrlRequired: true,
        credentialPolicy: {
          apiKeyRequired: true,
          baseUrlRequired: true,
          baseUrlMode: 'required',
          validationMode: 'model_list',
        },
        baseUrlPolicy: {
          allowOverride: true,
          requireHttps: true,
          allowPrivateNetwork: false,
          trimSuffixes: [],
        },
        modelDiscovery: {
          mode: 'credential',
          paths: [{ capabilityKind: 'chat', path: '/models' }],
        },
        capabilities: {
          chat: 'supported',
          embeddings: 'unsupported',
          modelDiscovery: 'supported',
          streaming: 'unknown',
          tools: 'unsupported',
          vision: 'unsupported',
        },
        runtime: {
          kind: 'compatible_chat',
          authScheme: 'bearer',
          chatPath: '/chat/completions',
          modelsPath: '/models',
          structuredOutput: 'json_object',
          tokenLimitParameter: 'max_tokens',
        },
        uiHints: {},
      },
    ])

    expect(providers[0]).not.toHaveProperty('credentialSource')
  })
})

describe('parseProviderCatalogResponse', () => {
  it('rejects non-canonical provider policy vocabulary from untyped payloads', () => {
    expect(() =>
      parseProviderCatalogResponse([
        {
          id: 'provider-alpha',
          providerKind: 'provider_alpha',
          displayName: 'Provider Alpha',
          apiKeyRequired: true,
          baseUrlRequired: true,
          credentialPolicy: {
            apiKeyRequired: true,
            baseUrlRequired: true,
            baseUrlMode: 'editable',
            validationMode: 'ping',
          },
          baseUrlPolicy: {
            allowOverride: true,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: [],
          },
          modelDiscovery: {
            mode: 'credential',
            paths: [{ capabilityKind: 'chat', path: '/models' }],
          },
          capabilities: {
            chat: 'supported',
            embeddings: 'unsupported',
            modelDiscovery: 'supported',
            streaming: 'unknown',
            tools: 'unsupported',
            vision: 'unsupported',
          },
          runtime: {},
          uiHints: {},
        },
      ]),
    ).toThrow('credentialPolicy.baseUrlMode')
  })
})

describe('parseModelCatalogResponse', () => {
  it('rejects malformed model catalog entries missing required availability fields', () => {
    expect(() =>
      parseModelCatalogResponse([
        {
          id: 'model-alpha',
          providerCatalogId: 'provider-alpha',
          modelName: 'alpha-chat',
          capabilityKind: 'chat',
          modalityKind: 'text',
          allowedBindingPurposes: ['extract_text', 'query_compile', 'query_answer'],
        },
      ]),
    ).toThrow('availabilityState')

    expect(() =>
      parseModelCatalogResponse([
        {
          id: 'model-alpha',
          providerCatalogId: 'provider-alpha',
          modelName: 'alpha-chat',
          capabilityKind: 'chat',
          modalityKind: 'text',
          allowedBindingPurposes: ['extract_text', 'query_compile', 'query_answer'],
          availabilityState: 'available',
        },
      ]),
    ).toThrow('availableAccountIds')
  })
})

describe('adminModelCatalogOptions', () => {
  it('routes model catalog queries through the validated adminApi boundary', async () => {
    const catalog: ModelCatalogEntryResponse[] = [
      {
        id: 'model-alpha',
        providerCatalogId: 'provider-alpha',
        modelName: 'alpha-chat',
        capabilityKind: 'chat',
        modalityKind: 'text',
        allowedBindingPurposes: ['extract_text', 'query_compile', 'query_answer'],
        availabilityState: 'available',
        availableAccountIds: ['account-alpha'],
        lifecycleState: 'active',
      },
    ]
    const listModels = vi.spyOn(adminApi, 'listModels').mockResolvedValueOnce(catalog)

    const options = adminModelCatalogOptions({
      providerCatalogId: 'provider-alpha',
      accountId: 'account-alpha',
    })

    await expect(options.queryFn()).resolves.toBe(catalog)
    expect(listModels).toHaveBeenCalledWith({
      providerCatalogId: 'provider-alpha',
      accountId: 'account-alpha',
    })
  })
})

describe('mapModelList', () => {
  it('maps generated model catalog availability without inventing defaults', () => {
    const models = mapModelList([
      {
        id: 'model-alpha',
        providerCatalogId: 'provider-alpha',
        modelName: 'alpha-chat',
        capabilityKind: 'chat',
        modalityKind: 'text',
        allowedBindingPurposes: ['extract_text', 'query_compile', 'query_answer'],
        availabilityState: 'unknown',
        availableAccountIds: ['account-alpha'],
        lifecycleState: 'active',
      } satisfies ModelCatalogEntryResponse,
    ])

    expect(models[0]).toMatchObject({
      id: 'model-alpha',
      allowedBindingPurposes: ['extract_text', 'query_compile', 'query_answer'],
      availabilityState: 'unknown',
      availableAccountIds: ['account-alpha'],
    })
  })
})

describe('mapBindingList', () => {
  it('keeps every generated binding purpose without local narrowing casts, including inline parameters', () => {
    const bindings = mapBindingList([
      {
        id: 'binding-alpha',
        scopeKind: 'workspace',
        bindingPurpose: 'embed_chunk',
        bindingState: 'active',
        accountId: 'account-alpha',
        modelCatalogId: 'model-alpha',
        extraParametersJson: { response_format: { type: 'json_object' } },
      } satisfies AiBindingResponse,
    ])

    expect(bindings[0]?.purpose).toBe('embed_chunk')
    expect(bindings[0]?.accountId).toBe('account-alpha')
    expect(bindings[0]?.modelCatalogId).toBe('model-alpha')
    expect(bindings[0]?.extraParams).toEqual({ response_format: { type: 'json_object' } })
  })

  it('does not parse legacy JSON strings as extra parameter objects', () => {
    const bindings = mapBindingList([
      {
        id: 'binding-alpha',
        scopeKind: 'workspace',
        bindingPurpose: 'embed_chunk',
        bindingState: 'active',
        accountId: 'account-alpha',
        modelCatalogId: 'model-alpha',
        extraParametersJson: '{"response_format":{"type":"json_object"}}',
      } satisfies AiBindingResponse,
    ])

    expect(bindings[0]).not.toHaveProperty('extraParams')
  })

  it.each(['active', 'invalid', 'disabled'] as const)(
    'preserves the canonical %s binding state in the view model',
    (bindingState) => {
      const bindings = mapBindingList([
        {
          id: `binding-${bindingState}`,
          scopeKind: 'instance',
          bindingPurpose: 'extract_text',
          bindingState,
          accountId: 'account-alpha',
          modelCatalogId: 'model-alpha',
          extraParametersJson: {},
        } satisfies AiBindingResponse,
      ])

      expect(bindings[0]?.state).toBe(bindingState)
    },
  )

  it('rejects a non-canonical inactive binding state', () => {
    expect(() =>
      mapBindingList([
        {
          id: 'binding-malformed',
          scopeKind: 'instance',
          bindingPurpose: 'extract_text',
          bindingState: 'inactive',
          accountId: 'account-alpha',
          modelCatalogId: 'model-alpha',
          extraParametersJson: {},
        } satisfies AiBindingResponse,
      ]),
    ).toThrow('invalid bindingState')
  })
})

describe('AI scope handling', () => {
  it('throws on malformed generated scopeKind instead of defaulting to workspace', () => {
    expect(() =>
      mapAccountList(
        [
          {
            id: 'account-alpha',
            scopeKind: 'organization',
            providerCatalogId: 'provider-alpha',
            label: 'Account Alpha',
            credentialState: 'active',
            createdAt: '2026-04-01T00:00:00Z',
            updatedAt: '2026-04-01T00:00:00Z',
          } as unknown as AiAccountResponse,
        ],
        [],
      ),
    ).toThrow('invalid scopeKind')
  })
})
