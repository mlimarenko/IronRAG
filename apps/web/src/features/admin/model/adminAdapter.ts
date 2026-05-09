import type {
  AuditEventPageResponse,
  AuditEventResponse,
  OpsLibraryStateResponse,
  PriceCatalogEntryResponse,
  TokenResponse,
} from '@/shared/api/generated';
import type {
  AIModelOption,
  AIProvider,
  APIToken,
  AuditEvent,
  AuditEventPage,
  OperationsSnapshot,
  OperationsWarning,
  PricingRule,
} from '@/shared/types';

function tokenStatus(value: string): APIToken['status'] {
  if (value === 'active' || value === 'expired' || value === 'revoked') {
    return value;
  }
  throw new Error(`Token response has invalid status: ${value}`);
}

function operationsStatus(value: string): OperationsSnapshot['status'] {
  if (
    value === 'processing' ||
    value === 'rebuilding' ||
    value === 'degraded' ||
    value === 'healthy'
  ) {
    return value;
  }
  throw new Error(`Operations response has invalid degradedState: ${value}`);
}

function auditResultKind(value: string): AuditEvent['resultKind'] {
  if (value === 'succeeded' || value === 'rejected' || value === 'failed') {
    return value;
  }
  throw new Error(`Audit event response has invalid resultKind: ${value}`);
}

export function mapToken(raw: TokenResponse): APIToken {
  const scope: APIToken['scope'] = {
    kind: raw.scope.kind,
    libraries: raw.scope.libraries.map((library) => ({
      id: library.id,
      workspaceId: library.workspaceId,
      displayName: library.displayName,
    })),
  };
  if (raw.scope.workspace) {
    scope.workspace = {
      id: raw.scope.workspace.id,
      displayName: raw.scope.workspace.displayName,
    };
  }

  const token: APIToken = {
    id: raw.principalId,
    label: raw.label,
    tokenPrefix: raw.tokenPrefix,
    status: tokenStatus(raw.status),
    scope,
    grants: raw.grants.map((grant): APIToken['grants'][number] => {
      const mappedGrant: APIToken['grants'][number] = {
        resourceKind: grant.resourceKind,
        resourceId: grant.resourceId,
        permission: grant.permissionKind,
      };
      if (grant.workspace) {
        mappedGrant.workspace = {
          id: grant.workspace.id,
          displayName: grant.workspace.displayName,
        };
      }
      if (grant.library) {
        mappedGrant.library = {
          id: grant.library.id,
          workspaceId: grant.library.workspaceId,
          displayName: grant.library.displayName,
        };
      }
      return mappedGrant;
    }),
  };
  if (raw.expiresAt) {
    token.expiresAt = raw.expiresAt;
  }
  if (raw.revokedAt) {
    token.revokedAt = raw.revokedAt;
  }
  if (raw.issuer) {
    token.issuedBy = {
      id: raw.issuer.principalId,
      displayLabel: raw.issuer.displayLabel,
    };
  }
  if (raw.lastUsedAt) {
    token.lastUsedAt = raw.lastUsedAt;
  }
  return token;
}

export function mapPricing(
  raw: PriceCatalogEntryResponse,
  providers: AIProvider[],
  models: AIModelOption[],
): PricingRule {
  const model = models.find((m) => m.id === raw.modelCatalogId);
  const provider = model ? providers.find((p) => p.id === model.providerCatalogId) : undefined;
  const pricing: PricingRule = {
    id: raw.id,
    provider: provider?.displayName ?? '',
    model: model?.modelName ?? raw.modelCatalogId ?? '',
    billingUnit: raw.billingUnit ?? '',
    unitPrice: parseFloat(raw.unitPrice ?? '') || 0,
    currency: raw.currencyCode ?? 'USD',
    effectiveFrom: raw.effectiveFrom
      ? new Date(raw.effectiveFrom).toISOString().slice(0, 10)
      : '',
    sourceOrigin: raw.catalogScope ?? 'catalog',
  };
  if (raw.effectiveTo) {
    pricing.effectiveTo = new Date(raw.effectiveTo).toISOString().slice(0, 10);
  }
  return pricing;
}

export function mapOps(raw: OpsLibraryStateResponse): OperationsSnapshot {
  const state = raw.state;
  return {
    queueDepth: state.queueDepth,
    runningAttempts: state.runningAttempts,
    readableDocCount: state.readableDocumentCount,
    failedDocCount: state.failedDocumentCount,
    status: operationsStatus(state.degradedState),
    knowledgeGenerationState: state.knowledgeGenerationState ?? 'unknown',
    lastRecomputedAt: state.lastRecomputedAt,
    warnings: raw.warnings.map((warning): OperationsWarning => {
      const mappedWarning: OperationsWarning = {
        id: warning.id,
        warningKind: warning.warningKind,
        severity: warning.severity,
        createdAt: warning.createdAt,
      };
      if (warning.resolvedAt) {
        mappedWarning.resolvedAt = warning.resolvedAt;
      }
      return mappedWarning;
    }),
  };
}

export function mapAudit(raw: AuditEventResponse): AuditEvent {
  const resultKind = auditResultKind(raw.resultKind);
  const assistantCall = raw.assistantCall
    ? (() => {
        const mappedCall: NonNullable<AuditEvent['assistantCall']> = {
          queryExecutionId: raw.assistantCall.queryExecutionId,
          models: raw.assistantCall.models.map((model) => ({
            providerKind: model.providerKind,
            modelName: model.modelName,
          })),
          totalCost: raw.assistantCall.totalCost ?? null,
          currencyCode: raw.assistantCall.currencyCode ?? null,
          providerCallCount: raw.assistantCall.providerCallCount,
        };
        if (raw.assistantCall.conversationId) {
          mappedCall.conversationId = raw.assistantCall.conversationId;
        }
        if (raw.assistantCall.runtimeExecutionId) {
          mappedCall.runtimeExecutionId = raw.assistantCall.runtimeExecutionId;
        }
        return mappedCall;
      })()
    : undefined;
  const event: AuditEvent = {
    id: raw.id,
    action: raw.actionKind,
    resultKind,
    surfaceKind: raw.surfaceKind,
    timestamp: raw.createdAt,
    message: raw.redactedMessage ?? raw.actionKind,
    subjectSummary:
      raw.subjects.map((s) => `${s.subjectKind}:${s.subjectId}`).join(', ') || '',
    actor: raw.actorPrincipalId ?? 'system',
  };
  if (assistantCall) {
    event.assistantCall = assistantCall;
  }
  return event;
}

export function mapAuditPage(raw: AuditEventPageResponse): AuditEventPage {
  return {
    items: raw.items.map(mapAudit),
    total: raw.total,
    limit: raw.limit,
    offset: raw.offset,
  };
}
