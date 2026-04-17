import type {
  RawAuditEventResponse,
  RawAuditPageResponse,
  RawOpsResponse,
  RawPricingResponse,
  RawTokenResponse,
} from '@/types/api-responses';
import type {
  AIModelOption,
  AIProvider,
  APIToken,
  AuditEvent,
  AuditEventPage,
  OperationsSnapshot,
  OperationsWarning,
  PricingRule,
} from '@/types';

const DEFAULT_AUDIT_PAGE_LIMIT = 50;

export function mapToken(raw: RawTokenResponse): APIToken {
  const scopeKind = raw.scope?.kind;
  return {
    id: raw.principalId ?? raw.id ?? '',
    label: raw.label ?? '',
    tokenPrefix: raw.tokenPrefix ?? '',
    status:
      raw.status === 'active' ? 'active' : raw.status === 'expired' ? 'expired' : 'revoked',
    expiresAt: raw.expiresAt ?? undefined,
    revokedAt: raw.revokedAt ?? undefined,
    issuedBy: raw.issuer
      ? {
          id: raw.issuer.principalId,
          displayLabel: raw.issuer.displayLabel,
        }
      : undefined,
    lastUsedAt: raw.lastUsedAt ?? undefined,
    scope: {
      kind: scopeKind === 'workspace' || scopeKind === 'library' ? scopeKind : 'system',
      workspace: raw.scope?.workspace
        ? {
            id: raw.scope.workspace.id,
            displayName: raw.scope.workspace.displayName,
          }
        : undefined,
      libraries: (raw.scope?.libraries ?? []).map((library) => ({
        id: library.id,
        workspaceId: library.workspaceId,
        displayName: library.displayName,
      })),
    },
    grants: (raw.grants ?? []).map((grant) => ({
      resourceKind: grant.resourceKind ?? '',
      resourceId: grant.resourceId ?? '',
      permission: grant.permissionKind ?? '',
      workspace: grant.workspace
        ? {
            id: grant.workspace.id,
            displayName: grant.workspace.displayName,
          }
        : undefined,
      library: grant.library
        ? {
            id: grant.library.id,
            workspaceId: grant.library.workspaceId,
            displayName: grant.library.displayName,
          }
        : undefined,
    })),
  };
}

export function mapPricing(
  raw: RawPricingResponse,
  providers: AIProvider[],
  models: AIModelOption[],
): PricingRule {
  const model = models.find((m) => m.id === raw.modelCatalogId);
  const provider = model ? providers.find((p) => p.id === model.providerCatalogId) : undefined;
  return {
    id: raw.id,
    provider: provider?.displayName ?? '',
    model: model?.modelName ?? raw.modelCatalogId ?? '',
    billingUnit: raw.billingUnit ?? '',
    unitPrice: parseFloat(raw.unitPrice ?? '') || 0,
    currency: raw.currencyCode ?? 'USD',
    effectiveFrom: raw.effectiveFrom
      ? new Date(raw.effectiveFrom).toISOString().slice(0, 10)
      : '',
    effectiveTo: raw.effectiveTo
      ? new Date(raw.effectiveTo).toISOString().slice(0, 10)
      : undefined,
    sourceOrigin: raw.catalogScope ?? 'catalog',
  };
}

export function mapOps(raw: RawOpsResponse): OperationsSnapshot {
  const state = raw.state ?? {};
  const degradedState =
    state.degradedState === 'processing' ||
    state.degradedState === 'rebuilding' ||
    state.degradedState === 'degraded' ||
    state.degradedState === 'healthy'
      ? state.degradedState
      : 'healthy';
  return {
    queueDepth: state.queueDepth ?? 0,
    runningAttempts: state.runningAttempts ?? 0,
    readableDocCount: state.readableDocumentCount ?? 0,
    failedDocCount: state.failedDocumentCount ?? 0,
    status: degradedState,
    knowledgeGenerationState: state.knowledgeGenerationState ?? 'unknown',
    lastRecomputedAt: state.lastRecomputedAt ?? '',
    warnings: (raw.warnings ?? []).map(
      (warning): OperationsWarning => ({
        id: warning.id ?? crypto.randomUUID(),
        warningKind: warning.warningKind ?? 'unknown',
        severity: warning.severity ?? 'warning',
        createdAt: warning.createdAt ?? '',
        resolvedAt: warning.resolvedAt ?? undefined,
      }),
    ),
  };
}

export function mapAudit(raw: RawAuditEventResponse): AuditEvent {
  const resultKind =
    raw.resultKind === 'rejected' || raw.resultKind === 'failed'
      ? raw.resultKind
      : 'succeeded';
  const assistantCall = raw.assistantCall
    ? {
        queryExecutionId: raw.assistantCall.queryExecutionId ?? '',
        conversationId: raw.assistantCall.conversationId ?? undefined,
        runtimeExecutionId: raw.assistantCall.runtimeExecutionId ?? undefined,
        models: Array.isArray(raw.assistantCall.models)
          ? raw.assistantCall.models
              .map((model) => ({
                providerKind: model.providerKind ?? '',
                modelName: model.modelName ?? '',
              }))
              .filter((model) => model.providerKind || model.modelName)
          : [],
        totalCost: raw.assistantCall.totalCost ?? null,
        currencyCode: raw.assistantCall.currencyCode ?? null,
        providerCallCount:
          typeof raw.assistantCall.providerCallCount === 'number'
            ? raw.assistantCall.providerCallCount
            : 0,
      }
    : undefined;
  return {
    id: raw.id,
    action: raw.actionKind ?? '',
    resultKind,
    surfaceKind: (raw.surfaceKind ?? 'rest') as AuditEvent['surfaceKind'],
    timestamp: raw.createdAt ?? '',
    message: raw.redactedMessage ?? raw.actionKind ?? '',
    subjectSummary:
      (raw.subjects ?? []).map((s) => `${s.subjectKind}:${s.subjectId}`).join(', ') || '',
    actor: raw.actorPrincipalId ?? 'system',
    assistantCall,
  };
}

export function mapAuditPage(raw: RawAuditPageResponse): AuditEventPage {
  return {
    items: Array.isArray(raw.items) ? raw.items.map(mapAudit) : [],
    total: typeof raw.total === 'number' ? raw.total : 0,
    limit: typeof raw.limit === 'number' ? raw.limit : DEFAULT_AUDIT_PAGE_LIMIT,
    offset: typeof raw.offset === 'number' ? raw.offset : 0,
  };
}
