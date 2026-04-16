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
  const scopeSummary = raw.workspaceId ? `workspace:${raw.workspaceId}` : 'system';
  return {
    id: raw.principalId ?? raw.id ?? '',
    label: raw.label ?? '',
    tokenPrefix: raw.tokenPrefix ?? '',
    status:
      raw.status === 'active' ? 'active' : raw.status === 'expired' ? 'expired' : 'revoked',
    expiresAt: raw.expiresAt ?? undefined,
    revokedAt: raw.revokedAt ?? undefined,
    issuedBy: raw.issuedByPrincipalId ?? 'system',
    lastUsedAt: raw.lastUsedAt ?? undefined,
    grants: [],
    scopeSummary,
    principalLabel: raw.label ?? '',
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
