import { useState, useEffect, useCallback } from 'react';
import type { TFunction } from 'i18next';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { useApp } from '@/contexts/AppContext';
import { adminApi, dashboardApi } from '@/api';
import { AVAILABLE_LOCALES } from '@/types';
import type { Locale } from '@/types';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Checkbox } from '@/components/ui/checkbox';
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter,
} from '@/components/ui/dialog';
import AiConfigurationPanel from '@/components/admin/AiConfigurationPanel';
import { mapProvider } from '@/lib/ai-mappers';
import type {
  RawModelCatalogEntry,
  RawTokenResponse,
  RawPricingResponse,
  RawOpsResponse,
  RawAuditEventResponse,
  RawAuditPageResponse,
} from '@/types/api-responses';
import {
  Key, Search, Plus, Copy, Trash2,
  Settings, Brain, DollarSign, CheckCircle2,
  AlertTriangle, XCircle, Loader2, Terminal, Code2, ExternalLink,
  Activity, RefreshCw
} from 'lucide-react';
import type {
  APIToken, AIProvider, PricingRule, OperationsSnapshot,
  OperationsWarning, AuditEvent, AuditEventPage,
} from '@/types';

const AUDIT_PAGE_SIZE_OPTIONS = [50, 100, 250, 1000] as const;
const AUDIT_SURFACE_OPTIONS = ['all', 'rest', 'mcp', 'worker', 'bootstrap'] as const;
const AUDIT_RESULT_OPTIONS = ['all', 'succeeded', 'rejected', 'failed'] as const;

type AuditResultFilter = (typeof AUDIT_RESULT_OPTIONS)[number];
type AuditSurfaceFilter = (typeof AUDIT_SURFACE_OPTIONS)[number];
type OperationsActionItemTone = 'ready' | 'warning' | 'failed';

type OperationsActionItem = {
  key: string;
  tone: OperationsActionItemTone;
  title: string;
  detail: string;
  actionLabel?: string;
  actionPath?: string;
};

type OperationsStatusMeta = {
  label: string;
  badgeClass: string;
  description: string;
};

function errorMessage(err: unknown, fallback: string): string {
  if (err instanceof Error) return err.message;
  if (typeof err === 'object' && err !== null && 'message' in err) {
    const msg = (err as { message?: unknown }).message;
    if (typeof msg === 'string') return msg;
  }
  return fallback;
}

// ── Response mappers ──

function mapToken(raw: RawTokenResponse): APIToken {
  return {
    id: raw.principalId ?? raw.id ?? '',
    label: raw.label ?? '',
    tokenPrefix: raw.tokenPrefix ?? '',
    status: raw.status === 'active' ? 'active' : raw.status === 'expired' ? 'expired' : 'revoked',
    expiresAt: raw.expiresAt ?? undefined,
    revokedAt: raw.revokedAt ?? undefined,
    issuedBy: raw.issuedByPrincipalId ?? 'system',
    lastUsedAt: raw.lastUsedAt ?? undefined,
    grants: [],
    scopeSummary: raw.status ?? '',
    principalLabel: raw.label ?? '',
  };
}

function mapPricing(raw: RawPricingResponse, providers: AIProvider[], models: RawModelCatalogEntry[]): PricingRule {
  const model = models.find(m => m.id === raw.modelCatalogId);
  const provider = model ? providers.find(p => p.id === model.providerCatalogId) : undefined;
  return {
    id: raw.id,
    provider: provider?.displayName ?? '',
    model: model?.modelName ?? raw.modelCatalogId ?? '',
    billingUnit: raw.billingUnit ?? '',
    unitPrice: parseFloat(raw.unitPrice ?? '') || 0,
    currency: raw.currencyCode ?? 'USD',
    effectiveFrom: raw.effectiveFrom ? new Date(raw.effectiveFrom).toISOString().slice(0, 10) : '',
    effectiveTo: raw.effectiveTo ? new Date(raw.effectiveTo).toISOString().slice(0, 10) : undefined,
    sourceOrigin: raw.catalogScope ?? 'catalog',
  };
}

function mapOps(raw: RawOpsResponse): OperationsSnapshot {
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
    warnings: (raw.warnings ?? []).map((warning): OperationsWarning => ({
      id: warning.id ?? crypto.randomUUID(),
      warningKind: warning.warningKind ?? 'unknown',
      severity: warning.severity ?? 'warning',
      createdAt: warning.createdAt ?? '',
      resolvedAt: warning.resolvedAt ?? undefined,
    })),
  };
}

function mapAudit(raw: RawAuditEventResponse): AuditEvent {
  const resultKind =
    raw.resultKind === 'rejected' || raw.resultKind === 'failed' ? raw.resultKind : 'succeeded';
  return {
    id: raw.id,
    action: raw.actionKind ?? '',
    resultKind,
    surfaceKind: (raw.surfaceKind ?? 'rest') as AuditEvent['surfaceKind'],
    timestamp: raw.createdAt ?? '',
    message: raw.redactedMessage ?? raw.actionKind ?? '',
    subjectSummary: (raw.subjects ?? []).map(s => `${s.subjectKind}:${s.subjectId}`).join(', ') || '',
    actor: raw.actorPrincipalId ?? 'system',
  };
}

function mapAuditPage(raw: RawAuditPageResponse): AuditEventPage {
  return {
    items: Array.isArray(raw.items) ? raw.items.map(mapAudit) : [],
    total: typeof raw.total === 'number' ? raw.total : 0,
    limit: typeof raw.limit === 'number' ? raw.limit : AUDIT_PAGE_SIZE_OPTIONS[0],
    offset: typeof raw.offset === 'number' ? raw.offset : 0,
  };
}

function buildDocumentsPath(params: Record<string, string | null | undefined>) {
  const searchParams = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value) {
      searchParams.set(key, value);
    }
  }

  const query = searchParams.toString();
  return query ? `/documents?${query}` : '/documents';
}

function getOperationsStatusMeta(
  ops: OperationsSnapshot,
  t: TFunction,
): OperationsStatusMeta {
  if (
    ops.status === 'healthy' &&
    ops.readableDocCount === 0 &&
    ops.failedDocCount === 0 &&
    ops.queueDepth === 0 &&
    ops.runningAttempts === 0
  ) {
    return {
      label: t('admin.opsStatusLabels.healthy'),
      badgeClass: 'status-ready',
      description: t('admin.opsStatusDescriptions.empty'),
    };
  }

  switch (ops.status) {
    case 'processing':
      return {
        label: t('admin.opsStatusLabels.processing'),
        badgeClass: 'status-processing',
        description: t('admin.opsStatusDescriptions.processing'),
      };
    case 'rebuilding':
      return {
        label: t('admin.opsStatusLabels.rebuilding'),
        badgeClass: 'status-warning',
        description: t('admin.opsStatusDescriptions.rebuilding'),
      };
    case 'degraded':
      return {
        label: t('admin.opsStatusLabels.degraded'),
        badgeClass: 'status-failed',
        description: t('admin.opsStatusDescriptions.degraded'),
      };
    default:
      return {
        label: t('admin.opsStatusLabels.healthy'),
        badgeClass: 'status-ready',
        description: t('admin.opsStatusDescriptions.healthy'),
      };
  }
}

function getOperationsActionItems(
  ops: OperationsSnapshot,
  t: TFunction,
): OperationsActionItem[] {
  const items: OperationsActionItem[] = [];

  if (ops.failedDocCount > 0) {
    items.push({
      key: 'failed_documents',
      tone: 'failed',
      title: t('admin.opsActions.failedDocuments.title'),
      detail: t('admin.opsActions.failedDocuments.detail', { count: ops.failedDocCount }),
      actionLabel: t('admin.opsActions.failedDocuments.action'),
      actionPath: buildDocumentsPath({ status: 'failed' }),
    });
  }

  const queuedOrRunning = ops.queueDepth + ops.runningAttempts;
  if (queuedOrRunning > 0) {
    items.push({
      key: 'processing_queue',
      tone: 'warning',
      title: t('admin.opsActions.processingQueue.title'),
      detail: t('admin.opsActions.processingQueue.detail', { count: queuedOrRunning }),
      actionLabel: t('admin.opsActions.processingQueue.action'),
      actionPath: buildDocumentsPath({ status: 'in_progress' }),
    });
  }

  for (const warning of ops.warnings) {
    switch (warning.warningKind) {
      case 'stale_vectors':
        items.push({
          key: warning.warningKind,
          tone: 'warning',
          title: t('admin.opsActions.staleVectors.title'),
          detail: t('admin.opsActions.staleVectors.detail'),
          actionLabel: t('admin.opsActions.staleVectors.action'),
          actionPath: buildDocumentsPath({ status: 'in_progress' }),
        });
        break;
      case 'stale_relations':
        items.push({
          key: warning.warningKind,
          tone: 'warning',
          title: t('admin.opsActions.staleRelations.title'),
          detail: t('admin.opsActions.staleRelations.detail'),
          actionLabel: t('admin.opsActions.staleRelations.action'),
          actionPath: '/graph',
        });
        break;
      case 'failed_rebuilds':
        items.push({
          key: warning.warningKind,
          tone: 'failed',
          title: t('admin.opsActions.failedRebuilds.title'),
          detail: t('admin.opsActions.failedRebuilds.detail'),
          actionLabel: t('admin.opsActions.failedRebuilds.action'),
          actionPath: buildDocumentsPath({ status: 'failed' }),
        });
        break;
      case 'bundle_assembly_failures':
        items.push({
          key: warning.warningKind,
          tone: 'failed',
          title: t('admin.opsActions.bundleFailures.title'),
          detail: t('admin.opsActions.bundleFailures.detail'),
          actionLabel: t('admin.opsActions.bundleFailures.action'),
          actionPath: '/graph',
        });
        break;
      default:
        break;
    }
  }

  const deduped = new Map<string, OperationsActionItem>();
  for (const item of items) {
    deduped.set(item.key, item);
  }

  return Array.from(deduped.values()).sort((left, right) => {
    const priority = (tone: OperationsActionItemTone) =>
      tone === 'failed' ? 2 : tone === 'warning' ? 1 : 0;
    return priority(right.tone) - priority(left.tone);
  });
}

function getOperationsActionToneClass(tone: OperationsActionItemTone) {
  if (tone === 'failed') return 'text-status-failed border-status-failed/15 bg-status-failed/5';
  if (tone === 'warning') return 'text-status-warning border-status-warning/15 bg-status-warning/5';
  return 'text-status-ready border-status-ready/15 bg-status-ready/5';
}

function getAuditResultBadgeClass(resultKind: AuditEvent['resultKind']) {
  if (resultKind === 'failed') return 'status-failed';
  if (resultKind === 'rejected') return 'status-warning';
  return 'status-ready';
}

function getAuditResultIcon(resultKind: AuditEvent['resultKind']) {
  if (resultKind === 'failed') return XCircle;
  if (resultKind === 'rejected') return AlertTriangle;
  return CheckCircle2;
}

function humanizeTokenStatus(status: APIToken['status'], t: TFunction) {
  switch (status) {
    case 'active':
      return t('admin.active');
    case 'expired':
      return t('admin.expired');
    case 'revoked':
      return t('admin.revoked');
    default:
      return status;
  }
}

function humanizeGenerationState(state: string, t: TFunction) {
  switch (state) {
    case 'graph_ready':
      return t('admin.opsGenerationStates.graph_ready');
    case 'vector_ready':
      return t('admin.opsGenerationStates.vector_ready');
    case 'text_readable':
      return t('admin.opsGenerationStates.text_readable');
    case 'accepted':
    case 'unknown':
      return t('admin.opsGenerationStates.unknown');
    default:
      return state;
  }
}

function humanizeAuditSurface(surfaceKind: string, t: TFunction) {
  switch (surfaceKind) {
    case 'mcp':
    case 'worker':
    case 'bootstrap':
    case 'rest':
      return t(`admin.auditSurfaceLabels.${surfaceKind}`);
    default:
      return surfaceKind;
  }
}

function humanizeAuditResult(resultKind: AuditEvent['resultKind'], t: TFunction) {
  return t(`admin.auditResultLabels.${resultKind}`);
}

// ── Static data ──

const WS_PERMISSIONS = ['workspace_admin', 'workspace_read', 'library_read', 'library_write', 'document_read', 'document_write', 'connector_admin', 'credential_admin', 'binding_admin', 'query_run', 'ops_read', 'audit_read', 'iam_admin'];
const LIB_PERMISSIONS = ['library_read', 'library_write', 'document_read', 'document_write', 'connector_admin', 'binding_admin', 'query_run'];

function getMcpConfigs(origin: string) {
  const mcpUrl = `${origin}/v1/mcp`;
  return [
    { name: 'Codex', icon: Terminal, config: `{\n  "mcpServers": {\n    "rustrag": {\n      "url": "${mcpUrl}",\n      "env": { "RUSTRAG_API_KEY": "<your-token>" }\n    }\n  }\n}` },
    { name: 'Cursor', icon: Code2, config: `// .cursor/mcp.json\n{\n  "mcpServers": {\n    "rustrag": {\n      "url": "${mcpUrl}",\n      "env": { "RUSTRAG_API_KEY": "<your-token>" }\n    }\n  }\n}` },
    { name: 'Claude Code', icon: Terminal, config: `claude mcp add rustrag -- \\\n  npx @anthropic-ai/mcp-proxy@latest \\\n  "${mcpUrl}"` },
    { name: 'Claude Desktop', icon: Brain, config: `{\n  "mcpServers": {\n    "rustrag": {\n      "url": "${mcpUrl}",\n      "env": { "RUSTRAG_API_KEY": "<your-token>" }\n    }\n  }\n}` },
    { name: 'VS Code', icon: Code2, config: `// .vscode/settings.json\n{\n  "mcp.servers": {\n    "rustrag": {\n      "url": "${mcpUrl}",\n      "env": { "RUSTRAG_API_KEY": "<your-token>" }\n    }\n  }\n}` },
  ];
}

// ── Component ──

export default function AdminPage() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [searchParams, setSearchParams] = useSearchParams();
  const { activeWorkspace, activeLibrary, locale, setLocale } = useApp();
  const [activeTab, setActiveTab] = useState(() => {
    const requestedTab = searchParams.get('tab');
    return requestedTab && ['access', 'mcp', 'operations', 'ai', 'pricing', 'settings'].includes(requestedTab)
      ? requestedTab
      : 'access';
  });

  // Access tab state
  const [tokens, setTokens] = useState<APIToken[]>([]);
  const [tokensLoading, setTokensLoading] = useState(false);
  const [tokensError, setTokensError] = useState<string | null>(null);
  const [selectedToken, setSelectedToken] = useState<APIToken | null>(null);
  const [createTokenOpen, setCreateTokenOpen] = useState(false);
  const [createdToken, setCreatedToken] = useState<string | null>(null);
  const [showToken, setShowToken] = useState(false);
  const [tokenSearch, setTokenSearch] = useState('');

  const [tokenLabel, setTokenLabel] = useState('');
  const [tokenExpiry, setTokenExpiry] = useState('90');
  const [tokenScope, setTokenScope] = useState<'workspace' | 'library'>('workspace');
  const [selectedPermissions, setSelectedPermissions] = useState<string[]>([]);
  const [mintingToken, setMintingToken] = useState(false);

  // Shared admin catalog state
  const [providers, setProviders] = useState<AIProvider[]>([]);

  const [createPricingOpen, setCreatePricingOpen] = useState(false);
  const [pricingModelId, setPricingModelId] = useState('');
  const [pricingBillingUnit, setPricingBillingUnit] = useState('');
  const [pricingUnitPrice, setPricingUnitPrice] = useState('');
  const [pricingCurrency, setPricingCurrency] = useState('USD');
  const [pricingFrom, setPricingFrom] = useState('');
  const [pricingTo, setPricingTo] = useState('');
  const [pricingSaving, setPricingSaving] = useState(false);

  // Pricing tab state
  const [pricing, setPricing] = useState<PricingRule[]>([]);
  const [pricingLoading, setPricingLoading] = useState(false);
  const [pricingSearch, setPricingSearch] = useState('');
  const [pricingProvider, setPricingProvider] = useState('all');

  // Operations tab state
  const [ops, setOps] = useState<OperationsSnapshot | null>(null);
  const [opsLoading, setOpsLoading] = useState(false);
  const [opsError, setOpsError] = useState<string | null>(null);

  // Audit state
  const [audit, setAudit] = useState<AuditEventPage>({
    items: [],
    total: 0,
    limit: AUDIT_PAGE_SIZE_OPTIONS[0],
    offset: 0,
  });
  const [auditLoading, setAuditLoading] = useState(false);
  const [auditSearch, setAuditSearch] = useState('');
  const [auditResultFilter, setAuditResultFilter] = useState<AuditResultFilter>('all');
  const [auditSurfaceFilter, setAuditSurfaceFilter] = useState<AuditSurfaceFilter>('all');
  const [auditPageSize, setAuditPageSize] = useState<(typeof AUDIT_PAGE_SIZE_OPTIONS)[number]>(
    AUDIT_PAGE_SIZE_OPTIONS[0],
  );
  const [auditPage, setAuditPage] = useState(1);

  // Raw model catalog for pricing resolution
  const [rawModels, setRawModels] = useState<RawModelCatalogEntry[]>([]);

  // ── Data fetchers ──

  const loadTokens = useCallback(() => {
    setTokensLoading(true);
    setTokensError(null);
    adminApi.listTokens()
      .then((data) => {
        const list = Array.isArray(data) ? data : [];
        setTokens(list.map(mapToken));
      })
      .catch((err: unknown) => setTokensError(errorMessage(err, t('admin.loadTokensFailed'))))
      .finally(() => setTokensLoading(false));
  }, [t]);

  const loadPricing = useCallback(() => {
    setPricingLoading(true);
    adminApi.listPrices()
      .then((data) => {
        const list = Array.isArray(data) ? data : [];
        setPricing(list.map((p) => mapPricing(p, providers, rawModels)));
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.loadPricingFailed'))))
      .finally(() => setPricingLoading(false));
  }, [providers, rawModels, t]);

  const loadOps = useCallback(() => {
    if (!activeLibrary) {
      setOps(null);
      return;
    }
    setOpsLoading(true);
    setOpsError(null);
    dashboardApi.getLibraryState(activeLibrary.id)
      .then(data => setOps(mapOps(data)))
      .catch((err: unknown) => setOpsError(errorMessage(err, t('admin.loadOperationsFailed'))))
      .finally(() => setOpsLoading(false));
  }, [activeLibrary, t]);

  const loadAudit = useCallback(() => {
    if (!activeWorkspace && !activeLibrary) {
      setAudit({
        items: [],
        total: 0,
        limit: auditPageSize,
        offset: 0,
      });
      return;
    }

    setAuditLoading(true);
    adminApi.listAuditEvents({
      workspaceId: activeLibrary ? undefined : activeWorkspace?.id,
      libraryId: activeLibrary?.id,
      search: auditSearch || undefined,
      surfaceKind: auditSurfaceFilter === 'all' ? undefined : auditSurfaceFilter,
      resultKind: auditResultFilter === 'all' ? undefined : auditResultFilter,
      limit: auditPageSize,
      offset: (auditPage - 1) * auditPageSize,
    })
      .then((data) => {
        const pageData = mapAuditPage(data);
        const totalPages = Math.max(1, Math.ceil(pageData.total / auditPageSize));
        if (pageData.total > 0 && auditPage > totalPages) {
          setAuditPage(totalPages);
          return;
        }
        setAudit(pageData);
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.loadAuditEventsFailed'))))
      .finally(() => setAuditLoading(false));
  }, [
    activeLibrary,
    activeWorkspace,
    auditPage,
    auditPageSize,
    auditResultFilter,
    auditSearch,
    auditSurfaceFilter,
    t,
  ]);

  // ── Load data per tab ──

  useEffect(() => {
    if (activeTab === 'access') loadTokens();
  }, [activeTab, loadTokens]);

  useEffect(() => {
    if (activeTab === 'pricing') {
      // Ensure providers/models are loaded for name resolution
      if (providers.length === 0) {
        Promise.all([adminApi.listProviders(), adminApi.listModels()])
          .then(([provRaw, modelRaw]) => {
            const provList = (Array.isArray(provRaw) ? provRaw : []).map(mapProvider);
            setProviders(provList);
            setRawModels(Array.isArray(modelRaw) ? modelRaw : []);
          })
          .then(() => loadPricing())
          .catch(() => loadPricing());
      } else {
        loadPricing();
      }
    }
  }, [activeTab, loadPricing, providers.length]);

  useEffect(() => {
    if (activeTab === 'operations') {
      loadOps();
      loadAudit();
    }
  }, [activeTab, loadOps, loadAudit]);

  useEffect(() => {
    const requestedTab = searchParams.get('tab');
    if (
      requestedTab
      && ['access', 'mcp', 'operations', 'ai', 'pricing', 'settings'].includes(requestedTab)
      && requestedTab !== activeTab
    ) {
      setActiveTab(requestedTab);
    }
  }, [activeTab, searchParams]);

  const handleTabChange = useCallback((nextTab: string) => {
    setActiveTab(nextTab);
    const nextParams = new URLSearchParams(searchParams);
    nextParams.set('tab', nextTab);
    setSearchParams(nextParams, { replace: true });
  }, [searchParams, setSearchParams]);

  useEffect(() => {
    setAuditPage(1);
  }, [activeLibrary?.id, activeWorkspace?.id]);

  // ── Actions ──

  const handleCreateToken = () => {
    setMintingToken(true);
    adminApi.mintToken(tokenLabel)
      .then((data) => {
        setCreatedToken(data.token ?? '');
        setCreateTokenOpen(false);
        setShowToken(true);
        loadTokens();
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.createTokenFailed'))))
      .finally(() => setMintingToken(false));
  };

  const handleRevokeToken = (token: APIToken) => {
    adminApi.revokeToken(token.id)
      .then(() => {
        loadTokens();
        setSelectedToken(null);
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.revokeTokenFailed'))));
  };


  const handleCreatePricing = () => {
    if (!activeWorkspace || !pricingModelId || !pricingBillingUnit || !pricingUnitPrice || !pricingFrom) return;
    setPricingSaving(true);
    adminApi.createPriceOverride({
      workspaceId: activeWorkspace.id,
      modelCatalogId: pricingModelId,
      billingUnit: pricingBillingUnit,
      unitPrice: pricingUnitPrice,
      currencyCode: pricingCurrency,
      effectiveFrom: new Date(pricingFrom).toISOString(),
      effectiveTo: pricingTo ? new Date(pricingTo).toISOString() : null,
    })
      .then(() => {
        toast.success(t('admin.pricingOverrideCreated'));
        setCreatePricingOpen(false);
        setPricingModelId(''); setPricingBillingUnit(''); setPricingUnitPrice('');
        setPricingCurrency('USD'); setPricingFrom(''); setPricingTo('');
        loadPricing();
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.createPricingFailed'))))
      .finally(() => setPricingSaving(false));
  };

  // ── Derived ──

  const tokenStatusCls = (s: string) => s === 'active' ? 'status-ready' : s === 'expired' ? 'status-warning' : 'status-failed';

  const filteredTokens = tokens.filter(t => !tokenSearch || t.label.toLowerCase().includes(tokenSearch.toLowerCase()));
  const filteredPricing = pricing.filter(p => {
    if (pricingProvider !== 'all' && p.provider !== pricingProvider) return false;
    if (pricingSearch && !p.model.toLowerCase().includes(pricingSearch.toLowerCase())) return false;
    return true;
  });
  const opsStatusMeta = ops ? getOperationsStatusMeta(ops, t) : null;
  const opsActionItems = ops ? getOperationsActionItems(ops, t) : [];
  const auditTotalPages = Math.max(1, Math.ceil(audit.total / auditPageSize));
  const auditFrom = audit.total === 0 ? 0 : (auditPage - 1) * auditPageSize + 1;
  const auditTo = audit.total === 0 ? 0 : Math.min(audit.total, auditFrom + audit.items.length - 1);

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="page-header">
        <h1 className="text-lg font-bold tracking-tight">{t('admin.title')}</h1>
        <p className="text-sm text-muted-foreground">
          {activeWorkspace?.name}{activeLibrary ? <><span className="mx-2 text-border">&middot;</span>{activeLibrary.name}</> : ''}
        </p>
      </div>

      <Tabs value={activeTab} onValueChange={handleTabChange} className="flex-1 flex flex-col overflow-hidden">
        <div className="border-b px-6" style={{
          background: 'linear-gradient(180deg, hsl(var(--card) / 0.8), transparent)',
        }}>
          <TabsList className="bg-transparent h-auto p-0 gap-0">
            {[
              { value: 'access', label: t('admin.access'), icon: Key },
              { value: 'mcp', label: t('admin.mcp'), icon: Terminal },
              { value: 'operations', label: t('admin.operations'), icon: Activity },
              { value: 'ai', label: t('admin.ai'), icon: Brain },
              { value: 'pricing', label: t('admin.pricing'), icon: DollarSign },
              { value: 'settings', label: t('admin.settings'), icon: Settings },
            ].map(tab => (
              <TabsTrigger key={tab.value} value={tab.value} className="rounded-none border-b-2 border-transparent data-[state=active]:border-primary data-[state=active]:bg-transparent data-[state=active]:shadow-none px-4 py-3 gap-1.5 font-semibold text-sm transition-all duration-200">
                <tab.icon className="h-3.5 w-3.5" /> {tab.label}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>

        <div className="flex-1 overflow-auto">
          {/* ACCESS TAB */}
          <TabsContent value="access" className="mt-0 p-6 animate-fade-in">
            <div className="flex items-center justify-between mb-5">
              <div className="flex gap-4 text-xs font-semibold">
                {tokensLoading ? (
                  <span className="text-muted-foreground flex items-center gap-1.5"><Loader2 className="h-3 w-3 animate-spin" /> {t('admin.loading')}</span>
                ) : tokensError ? (
                  <span className="text-status-failed">{tokensError}</span>
                ) : (
                  <>
                    <span className="text-muted-foreground">{tokens.length} {t('admin.total')}</span>
                    <span className="text-status-ready">{tokens.filter(t => t.status === 'active').length} {t('admin.active')}</span>
                  </>
                )}
              </div>
              <div className="flex gap-2">
                <div className="relative">
                  <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
                  <Input className="h-9 pl-9 w-48 text-sm" placeholder={t('admin.searchTokens')} value={tokenSearch} onChange={e => setTokenSearch(e.target.value)} />
                </div>
                <Button size="sm" onClick={() => setCreateTokenOpen(true)}><Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.createToken')}</Button>
              </div>
            </div>

            <div className="flex gap-6">
              <div className="flex-1 space-y-1.5">
                {filteredTokens.map(token => (
                  <button
                    key={token.id}
                    className={`w-full flex items-center gap-3 p-4 rounded-xl text-left transition-all duration-200 ${selectedToken?.id === token.id ? 'bg-card shadow-lifted border border-primary/15' : 'hover:bg-accent/50 border border-transparent hover:shadow-soft'}`}
                    onClick={() => setSelectedToken(token)}
                  >
                    <div className="w-9 h-9 rounded-xl bg-surface-sunken flex items-center justify-center shrink-0">
                      <Key className="h-4 w-4 text-muted-foreground" />
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="text-sm font-bold truncate">{token.label}</div>
                      <div className="text-xs text-muted-foreground mt-0.5 font-medium">{token.tokenPrefix}... · {token.scopeSummary}</div>
                    </div>
                    <span className={`status-badge ${tokenStatusCls(token.status)}`}>{humanizeTokenStatus(token.status, t)}</span>
                  </button>
                ))}
                {!tokensLoading && !tokensError && filteredTokens.length === 0 && (
                  <div className="text-sm text-muted-foreground text-center p-8">{t('admin.noTokens')}</div>
                )}
              </div>

              {selectedToken && (
                <div className="w-80 shrink-0 workbench-surface p-5 space-y-4 animate-slide-in-right">
                  <div className="flex items-center justify-between">
                    <h3 className="text-sm font-bold">{selectedToken.label}</h3>
                    <span className={`status-badge ${tokenStatusCls(selectedToken.status)}`}>{humanizeTokenStatus(selectedToken.status, t)}</span>
                  </div>
                  <div className="space-y-2.5 text-sm">
                    {[
                      [t('admin.prefix'), selectedToken.tokenPrefix + '...'],
                      [t('admin.scope'), selectedToken.scopeSummary],
                      [t('admin.principal'), selectedToken.principalLabel],
                      [t('admin.issuedBy'), selectedToken.issuedBy],
                      [t('admin.expires'), selectedToken.expiresAt ? new Date(selectedToken.expiresAt).toLocaleDateString() : t('admin.never')],
                      [t('admin.lastUsed'), selectedToken.lastUsedAt ? new Date(selectedToken.lastUsedAt).toLocaleDateString() : t('admin.never')],
                    ].map(([k, v]) => (
                      <div key={k} className="flex justify-between">
                        <span className="text-muted-foreground">{k}</span>
                        <span className="font-mono text-xs font-bold">{v}</span>
                      </div>
                    ))}
                  </div>
                  {selectedToken.status === 'active' && (
                    <Button variant="destructive" size="sm" className="w-full" onClick={() => handleRevokeToken(selectedToken)}>
                      <Trash2 className="h-3.5 w-3.5 mr-1.5" /> {t('admin.revokeToken')}
                    </Button>
                  )}
                </div>
              )}
            </div>
          </TabsContent>

          {/* MCP TAB */}
          <TabsContent value="mcp" className="mt-0 p-6 animate-fade-in">
            <div className="mb-5">
              <h2 className="text-base font-bold tracking-tight">{t('admin.mcpTitle')}</h2>
              <p className="text-sm text-muted-foreground mt-1">{t('admin.mcpDesc')}</p>
            </div>
            <div className="grid grid-cols-2 gap-3 mb-6 text-xs">
              <div className="workbench-surface p-4">
                <div className="section-label mb-1.5">{t('admin.mcpServerUrl')}</div>
                <code className="font-mono text-xs font-bold">{window.location.origin}/v1/mcp</code>
              </div>
              <div className="workbench-surface p-4">
                <div className="section-label mb-1.5">{t('admin.capabilitiesProbe')}</div>
                <code className="font-mono text-xs font-bold">{window.location.origin}/v1/mcp/capabilities</code>
              </div>
            </div>
            <div className="space-y-4">
              {getMcpConfigs(window.location.origin).map(cfg => (
                <div key={cfg.name} className="workbench-surface overflow-hidden transition-shadow duration-200 hover:shadow-lifted">
                  <div className="flex items-center gap-2.5 p-4 border-b">
                    <div className="w-8 h-8 rounded-xl bg-surface-sunken flex items-center justify-center">
                      <cfg.icon className="h-4 w-4 text-muted-foreground" />
                    </div>
                    <h3 className="text-sm font-bold">{cfg.name}</h3>
                  </div>
                  <div className="p-4">
                    <pre className="text-xs bg-surface-sunken p-4 rounded-xl overflow-x-auto font-mono leading-relaxed border border-border/50">{cfg.config}</pre>
                    <div className="flex gap-2 mt-3">
                      <Button variant="outline" size="sm" onClick={() => navigator.clipboard.writeText(cfg.config)}>
                        <Copy className="h-3 w-3 mr-1.5" /> {t('admin.copy')}
                      </Button>
                    </div>
                  </div>
                </div>
              ))}
            </div>
          </TabsContent>

          {/* OPERATIONS TAB */}
          <TabsContent value="operations" className="mt-0 p-6 animate-fade-in">
            <div className="mb-5 flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
              <div>
                <h2 className="text-base font-bold tracking-tight">{t('admin.operations')}</h2>
                {opsStatusMeta && (
                  <p className="text-sm text-muted-foreground mt-1">{opsStatusMeta.description}</p>
                )}
              </div>
              <div className="flex items-center gap-2">
                <Button size="sm" variant="outline" onClick={() => { loadOps(); loadAudit(); }}>
                  <RefreshCw className={`h-3.5 w-3.5 mr-1.5 ${opsLoading || auditLoading ? 'animate-spin' : ''}`} />
                  {t('dashboard.refresh')}
                </Button>
                {opsLoading ? (
                  <span className="text-xs text-muted-foreground flex items-center gap-1.5"><Loader2 className="h-3 w-3 animate-spin" /> {t('admin.loading')}</span>
                ) : opsError ? (
                  <span className="text-xs text-status-failed">{opsError}</span>
                ) : opsStatusMeta ? (
                  <span className={`status-badge ${opsStatusMeta.badgeClass}`}>{opsStatusMeta.label}</span>
                ) : !activeLibrary ? (
                  <span className="text-xs text-muted-foreground">{t('admin.selectLibraryOps')}</span>
                ) : null}
              </div>
            </div>
            {ops ? (
              <>
                <div className="grid grid-cols-2 md:grid-cols-4 gap-3 mb-6">
                  {[
                    { label: t('admin.queueDepth'), value: ops.queueDepth },
                    { label: t('admin.running'), value: ops.runningAttempts },
                    { label: t('admin.readableDocs'), value: ops.readableDocCount },
                    { label: t('admin.failedDocs'), value: ops.failedDocCount, color: ops.failedDocCount > 0 ? 'text-status-failed' : undefined },
                  ].map(s => (
                    <div key={s.label} className="stat-tile">
                      <div className="section-label">{s.label}</div>
                      <div className={`text-2xl font-bold mt-2 tracking-tight tabular-nums ${s.color ?? ''}`}>{s.value}</div>
                    </div>
                  ))}
                </div>
                <div className="grid gap-4 xl:grid-cols-[minmax(0,2fr)_minmax(320px,1fr)] mb-8">
                  <div className="workbench-surface p-5">
                    <div className="flex items-start justify-between gap-3 mb-4">
                      <div>
                        <div className="text-sm font-bold">{t('admin.opsGuidanceTitle')}</div>
                        {opsStatusMeta && (
                          <p className="text-sm text-muted-foreground mt-1">{opsStatusMeta.description}</p>
                        )}
                      </div>
                      {opsStatusMeta && (
                        <span className={`status-badge ${opsStatusMeta.badgeClass}`}>{opsStatusMeta.label}</span>
                      )}
                    </div>

                    {opsActionItems.length === 0 ? (
                      <div className="rounded-xl border border-status-ready/15 bg-status-ready/5 p-4">
                        <div className="text-sm font-semibold text-status-ready">{t('admin.opsNoActionTitle')}</div>
                        <p className="text-sm text-muted-foreground mt-1">{t('admin.opsNoActionDesc')}</p>
                        <Button className="mt-3" variant="outline" size="sm" onClick={() => navigate('/documents')}>
                          {t('dashboard.openDocuments')}
                        </Button>
                      </div>
                    ) : (
                      <div className="space-y-3">
                        {opsActionItems.map(item => (
                          <div key={item.key} className={`rounded-xl border p-4 ${getOperationsActionToneClass(item.tone)}`}>
                            <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
                              <div className="min-w-0">
                                <div className="text-sm font-semibold">{item.title}</div>
                                <p className="text-sm text-muted-foreground mt-1">{item.detail}</p>
                              </div>
                              {item.actionPath && item.actionLabel && (
                                <Button
                                  variant="outline"
                                  size="sm"
                                  className="shrink-0"
                                  onClick={() => navigate(item.actionPath!)}
                                >
                                  <ExternalLink className="h-3.5 w-3.5 mr-1.5" />
                                  {item.actionLabel}
                                </Button>
                              )}
                            </div>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>

                  <div className="workbench-surface p-5 space-y-4">
                    <div>
                      <div className="section-label mb-1.5">{t('admin.knowledgeGeneration')}</div>
                      <div className="text-lg font-bold tracking-tight">
                        {humanizeGenerationState(ops.knowledgeGenerationState, t)}
                      </div>
                    </div>
                    <div className="text-sm space-y-2.5">
                      <div className="flex items-center justify-between gap-3">
                        <span className="text-muted-foreground">{t('admin.lastRecomputed')}</span>
                        <span className="font-semibold text-right">
                          {ops.lastRecomputedAt ? new Date(ops.lastRecomputedAt).toLocaleString() : t('admin.never')}
                        </span>
                      </div>
                      <div className="flex items-center justify-between gap-3">
                        <span className="text-muted-foreground">{t('admin.readableDocs')}</span>
                        <span className="font-semibold tabular-nums">{ops.readableDocCount}</span>
                      </div>
                      <div className="flex items-center justify-between gap-3">
                        <span className="text-muted-foreground">{t('admin.opsSignals')}</span>
                        <span className="font-semibold tabular-nums">{opsActionItems.length}</span>
                      </div>
                    </div>
                  </div>
                </div>
              </>
            ) : !opsLoading && !opsError && (
              <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">
                {activeLibrary ? t('admin.noOpsData') : t('admin.selectLibraryOps')}
              </div>
            )}

            <h3 className="text-sm font-bold tracking-tight mb-3">{t('admin.auditLog')}</h3>
            <div className="workbench-surface p-4 mb-4 flex flex-col gap-3 xl:flex-row xl:items-center">
              <div className="relative flex-1">
                <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
                <Input
                  className="h-9 pl-9 text-sm"
                  placeholder={t('admin.auditSearchPlaceholder')}
                  value={auditSearch}
                  onChange={e => {
                    setAuditSearch(e.target.value);
                    setAuditPage(1);
                  }}
                />
              </div>
              <div className="flex flex-col gap-3 sm:flex-row sm:items-center">
                <Select
                  value={auditResultFilter}
                  onValueChange={value => {
                    setAuditResultFilter(value as AuditResultFilter);
                    setAuditPage(1);
                  }}
                >
                  <SelectTrigger className="h-9 w-full sm:w-40 text-sm">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {AUDIT_RESULT_OPTIONS.map(option => (
                      <SelectItem key={option} value={option}>
                        {option === 'all' ? t('admin.auditResultAll') : humanizeAuditResult(option, t)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Select
                  value={auditSurfaceFilter}
                  onValueChange={value => {
                    setAuditSurfaceFilter(value as AuditSurfaceFilter);
                    setAuditPage(1);
                  }}
                >
                  <SelectTrigger className="h-9 w-full sm:w-40 text-sm">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {AUDIT_SURFACE_OPTIONS.map(option => (
                      <SelectItem key={option} value={option}>
                        {option === 'all' ? t('admin.auditSurfaceAll') : humanizeAuditSurface(option, t)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
                <Select
                  value={String(auditPageSize)}
                  onValueChange={value => {
                    setAuditPageSize(Number(value) as (typeof AUDIT_PAGE_SIZE_OPTIONS)[number]);
                    setAuditPage(1);
                  }}
                >
                  <SelectTrigger className="h-9 w-full sm:w-32 text-sm">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {AUDIT_PAGE_SIZE_OPTIONS.map(option => (
                      <SelectItem key={option} value={String(option)}>
                        {t('admin.auditPageSizeOption', { count: option })}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
            {auditLoading ? (
              <div className="flex items-center gap-2 text-sm text-muted-foreground p-4"><Loader2 className="h-4 w-4 animate-spin" /> {t('admin.loadingAudit')}</div>
            ) : audit.items.length === 0 ? (
              <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">{t('admin.noAuditEvents')}</div>
            ) : (
              <>
                <div className="workbench-surface divide-y">
                  {audit.items.map(evt => {
                    const ResultIcon = getAuditResultIcon(evt.resultKind);
                    return (
                    <div key={evt.id} className="p-4 flex items-start gap-3 transition-colors hover:bg-accent/30">
                      <div className={`mt-0.5 ${evt.resultKind === 'failed' ? 'text-status-failed' : evt.resultKind === 'rejected' ? 'text-status-warning' : 'text-status-ready'}`}>
                        <ResultIcon className="h-4 w-4" />
                      </div>
                      <div className="flex-1 min-w-0">
                        <div className="flex flex-col gap-2 lg:flex-row lg:items-start lg:justify-between">
                          <div className="min-w-0">
                            <div className="text-sm font-semibold">{evt.message}</div>
                            <div className="text-xs text-muted-foreground mt-1 font-medium flex flex-wrap items-center gap-x-2 gap-y-1">
                              <span>{evt.action}</span>
                              <span>&middot;</span>
                              <span>{humanizeAuditSurface(evt.surfaceKind, t)}</span>
                              <span>&middot;</span>
                              <span>{evt.actor}</span>
                              <span>&middot;</span>
                              <span>{new Date(evt.timestamp).toLocaleString()}</span>
                            </div>
                            {evt.subjectSummary && (
                              <div className="text-xs text-muted-foreground mt-1 font-medium truncate">
                                {evt.subjectSummary}
                              </div>
                            )}
                          </div>
                          <span className={`status-badge shrink-0 ${getAuditResultBadgeClass(evt.resultKind)}`}>
                            {humanizeAuditResult(evt.resultKind, t)}
                          </span>
                        </div>
                      </div>
                    </div>
                    );
                  })}
                </div>
                <div className="mt-4 flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
                  <div className="text-xs text-muted-foreground">
                    {t('admin.auditSummary', { from: auditFrom, to: auditTo, total: audit.total })}
                  </div>
                  <div className="flex items-center gap-2">
                    <Button size="sm" variant="outline" disabled={auditPage <= 1} onClick={() => setAuditPage(current => Math.max(1, current - 1))}>
                      {t('admin.previous')}
                    </Button>
                    <span className="text-xs text-muted-foreground min-w-24 text-center">
                      {t('admin.auditPageLabel', { page: auditPage, total: auditTotalPages })}
                    </span>
                    <Button size="sm" variant="outline" disabled={auditPage >= auditTotalPages} onClick={() => setAuditPage(current => Math.min(auditTotalPages, current + 1))}>
                      {t('admin.next')}
                    </Button>
                  </div>
                </div>
              </>
            )}
          </TabsContent>

          {/* AI TAB */}
          <TabsContent value="ai" className="mt-0 p-6 animate-fade-in">
            <AiConfigurationPanel />
          </TabsContent>

          {/* PRICING TAB */}
          <TabsContent value="pricing" className="mt-0 p-6 animate-fade-in">
            <div className="flex items-center justify-between mb-5">
              <h2 className="text-base font-bold tracking-tight">{t('admin.pricing')}</h2>
              <div className="flex gap-2">
                <Select value={pricingProvider} onValueChange={setPricingProvider}>
                  <SelectTrigger className="h-9 w-36 text-sm"><SelectValue placeholder={t('admin.provider')} /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">{t('admin.allProviders')}</SelectItem>
                    {providers.map(p => <SelectItem key={p.id} value={p.displayName}>{p.displayName}</SelectItem>)}
                  </SelectContent>
                </Select>
                <div className="relative">
                  <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
                  <Input className="h-9 pl-9 w-48 text-sm" placeholder={t('admin.searchModels')} value={pricingSearch} onChange={e => setPricingSearch(e.target.value)} />
                </div>
                <Button size="sm" variant="outline" onClick={() => setCreatePricingOpen(true)}>
                  <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.override')}
                </Button>
              </div>
            </div>
            {pricingLoading ? (
              <div className="flex items-center gap-2 text-sm text-muted-foreground p-4"><Loader2 className="h-4 w-4 animate-spin" /> {t('admin.loadingPricing')}</div>
            ) : (
              <div className="workbench-surface overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b text-left">
                      <th className="px-4 py-3 section-label">{t('admin.provider')}</th>
                      <th className="px-4 py-3 section-label">{t('admin.model')}</th>
                      <th className="px-4 py-3 section-label">{t('admin.billingUnit')}</th>
                      <th className="px-4 py-3 section-label">{t('admin.price')}</th>
                      <th className="px-4 py-3 section-label">{t('admin.effectiveFrom')}</th>
                      <th className="px-4 py-3 section-label">{t('admin.source')}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {filteredPricing.map(p => (
                      <tr key={p.id} className="border-b hover:bg-accent/30 transition-colors">
                        <td className="px-4 py-3.5 font-semibold">{p.provider}</td>
                        <td className="px-4 py-3.5 font-mono text-xs font-bold">{p.model}</td>
                        <td className="px-4 py-3.5 text-xs text-muted-foreground font-medium">{p.billingUnit.replace(/_/g, ' ')}</td>
                        <td className="px-4 py-3.5 tabular-nums font-bold">${p.unitPrice.toFixed(2)} {p.currency}</td>
                        <td className="px-4 py-3.5 text-muted-foreground text-xs">{p.effectiveFrom}</td>
                        <td className="px-4 py-3.5 text-xs text-muted-foreground font-medium">{p.sourceOrigin}</td>
                      </tr>
                    ))}
                    {filteredPricing.length === 0 && (
                      <tr><td colSpan={6} className="text-center p-8 text-sm text-muted-foreground">{t('admin.noPricingData')}</td></tr>
                    )}
                  </tbody>
                </table>
              </div>
            )}
          </TabsContent>

          {/* SETTINGS TAB */}
          <TabsContent value="settings" className="mt-0 p-6 animate-fade-in">
            <h2 className="text-base font-bold tracking-tight mb-5">{t('admin.settings')}</h2>
            <div className="max-w-md space-y-6">
              <div>
                <Label className="text-sm font-semibold">{t('admin.language')}</Label>
                <p className="text-xs text-muted-foreground mt-1 mb-2">{t('admin.languageDesc')}</p>
                <Select value={locale} onValueChange={v => setLocale(v as Locale)}>
                  <SelectTrigger className="mt-1">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {AVAILABLE_LOCALES.map(l => (
                      <SelectItem key={l.code} value={l.code}>
                        {l.nativeLabel}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            </div>
          </TabsContent>
        </div>
      </Tabs>

      {/* Dialogs */}
      <Dialog open={createTokenOpen} onOpenChange={setCreateTokenOpen}>
        <DialogContent className="max-w-md">
          <DialogHeader><DialogTitle>{t('admin.createTokenTitle')}</DialogTitle><DialogDescription>{t('admin.createTokenDesc')}</DialogDescription></DialogHeader>
          <div className="space-y-4">
            <div><Label>{t('admin.tokenLabel')}</Label><Input value={tokenLabel} onChange={e => setTokenLabel(e.target.value)} placeholder={t('admin.tokenLabelPlaceholder')} className="mt-2" /></div>
            <div><Label>{t('admin.tokenExpiry')}</Label><Select value={tokenExpiry} onValueChange={setTokenExpiry}><SelectTrigger className="mt-2"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="30">{t('admin.tokenExpiry30')}</SelectItem><SelectItem value="90">{t('admin.tokenExpiry90')}</SelectItem><SelectItem value="365">{t('admin.tokenExpiry365')}</SelectItem><SelectItem value="never">{t('admin.never')}</SelectItem></SelectContent></Select></div>
            <div><Label>{t('admin.tokenScope')}</Label><Select value={tokenScope} onValueChange={v => { setTokenScope(v as typeof tokenScope); setSelectedPermissions([]); }}><SelectTrigger className="mt-2"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="workspace">{t('admin.workspace')}</SelectItem><SelectItem value="library">{t('admin.library')}</SelectItem></SelectContent></Select></div>
            <div>
              <Label>{t('admin.tokenPermissions')}</Label>
              <div className="mt-2 space-y-1.5 max-h-40 overflow-y-auto p-3 border rounded-xl bg-surface-sunken">
                {(tokenScope === 'workspace' ? WS_PERMISSIONS : LIB_PERMISSIONS).map(p => (
                  <div key={p} className="flex items-center gap-2.5">
                    <Checkbox id={p} checked={selectedPermissions.includes(p)} onCheckedChange={checked => setSelectedPermissions(prev => checked ? [...prev, p] : prev.filter(x => x !== p))} />
                    <Label htmlFor={p} className="text-sm font-normal font-mono">{p}</Label>
                  </div>
                ))}
              </div>
            </div>
          </div>
          <DialogFooter><Button variant="outline" onClick={() => setCreateTokenOpen(false)}>{t('admin.cancel')}</Button><Button onClick={handleCreateToken} disabled={!tokenLabel.trim() || mintingToken}>{mintingToken ? t('admin.creating') : t('admin.create')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={showToken} onOpenChange={setShowToken}>
        <DialogContent>
          <DialogHeader><DialogTitle>{t('admin.tokenCreated')}</DialogTitle><DialogDescription>{t('admin.tokenCreatedDesc')}</DialogDescription></DialogHeader>
          <div className="flex items-center gap-2">
            <Input readOnly value={createdToken ?? ''} className="font-mono text-xs" />
            <Button variant="outline" size="icon" onClick={() => navigator.clipboard.writeText(createdToken ?? '')}><Copy className="h-4 w-4" /></Button>
          </div>
          <DialogFooter><Button onClick={() => { setShowToken(false); setCreatedToken(null); }}>{t('admin.done')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={createPricingOpen} onOpenChange={v => { setCreatePricingOpen(v); if (!v) { setPricingModelId(''); setPricingBillingUnit(''); setPricingUnitPrice(''); setPricingCurrency('USD'); setPricingFrom(''); setPricingTo(''); } }}>
        <DialogContent className="max-w-md">
          <DialogHeader><DialogTitle>{t('admin.addPricingOverride')}</DialogTitle></DialogHeader>
          <div className="space-y-4">
            <div><Label>{t('admin.model')}</Label><Select value={pricingModelId} onValueChange={setPricingModelId}><SelectTrigger className="mt-2"><SelectValue placeholder={t('admin.selectModel')} /></SelectTrigger><SelectContent>{rawModels.map((m) => <SelectItem key={m.id} value={m.id}>{m.modelName ?? m.id}</SelectItem>)}</SelectContent></Select></div>
            <div><Label>{t('admin.billingUnit')}</Label><Select value={pricingBillingUnit} onValueChange={setPricingBillingUnit}><SelectTrigger className="mt-2"><SelectValue placeholder={t('admin.selectBillingUnit')} /></SelectTrigger><SelectContent><SelectItem value="per_1m_input_tokens">{t('admin.per1mInputTokens')}</SelectItem><SelectItem value="per_1m_cached_input_tokens">{t('admin.per1mCachedInputTokens')}</SelectItem><SelectItem value="per_1m_output_tokens">{t('admin.per1mOutputTokens')}</SelectItem></SelectContent></Select></div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>{t('admin.unitPrice')}</Label><Input type="number" step="0.01" placeholder="0.00" className="mt-2" value={pricingUnitPrice} onChange={e => setPricingUnitPrice(e.target.value)} /></div>
              <div><Label>{t('admin.currency')}</Label><Input className="mt-2" value={pricingCurrency} onChange={e => setPricingCurrency(e.target.value)} /></div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>{t('admin.effectiveFrom')}</Label><Input type="date" className="mt-2" value={pricingFrom} onChange={e => setPricingFrom(e.target.value)} /></div>
              <div><Label>{t('admin.effectiveTo')}</Label><Input type="date" className="mt-2" value={pricingTo} onChange={e => setPricingTo(e.target.value)} /></div>
            </div>
          </div>
          <DialogFooter><Button variant="outline" onClick={() => setCreatePricingOpen(false)}>{t('admin.cancel')}</Button><Button disabled={!pricingModelId || !pricingBillingUnit || !pricingUnitPrice || !pricingFrom || pricingSaving} onClick={handleCreatePricing}>{pricingSaving ? t('admin.saving') : t('admin.save')}</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
