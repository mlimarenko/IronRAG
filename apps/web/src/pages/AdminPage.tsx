import { useState, useEffect, useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { useApp } from '@/contexts/AppContext';
import { adminApi, dashboardApi } from '@/api';
import { AVAILABLE_LOCALES } from '@/types';
import type { Locale } from '@/types';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Checkbox } from '@/components/ui/checkbox';
import {
  Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription, DialogFooter,
} from '@/components/ui/dialog';
import {
  Key, Search, Plus, Copy, Eye, EyeOff, Shield, Trash2,
  Settings, Server, Brain, DollarSign, Clock, CheckCircle2,
  AlertTriangle, XCircle, Loader2, Terminal, Code2, ExternalLink,
  Activity, Users, RefreshCw
} from 'lucide-react';
import type {
  APIToken, AIProvider, AICredential, ModelPreset, LibraryBinding,
  PricingRule, OperationsSnapshot, AuditEvent, AIPurpose
} from '@/types';

// ── Response mappers ──

function mapToken(raw: any): APIToken {
  return {
    id: raw.principalId ?? raw.id,
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

function mapProvider(raw: any): AIProvider {
  return {
    id: raw.id,
    displayName: raw.displayName ?? raw.providerKind ?? '',
    kind: raw.providerKind ?? 'llm',
    apiStyle: raw.apiStyle ?? '',
    lifecycleState: raw.lifecycleState === 'active' ? 'active' : raw.lifecycleState === 'deprecated' ? 'deprecated' : 'preview',
    modelCount: 0,
    credentialCount: 0,
  };
}

function mapCredential(raw: any, providers: AIProvider[]): AICredential {
  const provider = providers.find(p => p.id === raw.providerCatalogId);
  return {
    id: raw.id,
    providerId: raw.providerCatalogId ?? '',
    providerName: provider?.displayName ?? '',
    label: raw.label ?? '',
    state: raw.credentialState === 'valid' ? 'valid' : raw.credentialState === 'invalid' ? 'invalid' : 'unchecked',
    createdAt: raw.createdAt ?? '',
    updatedAt: raw.updatedAt ?? '',
    apiKeySummary: raw.apiKeySummary ?? '',
  };
}

function mapPreset(raw: any): ModelPreset {
  return {
    id: raw.id,
    providerId: raw.modelCatalogId ?? '',
    model: raw.presetName ?? '',
    presetName: raw.presetName ?? '',
    systemPrompt: raw.systemPrompt ?? undefined,
    temperature: raw.temperature ?? 0,
    topP: raw.topP ?? 1,
    maxOutputTokens: raw.maxOutputTokensOverride ?? undefined,
    createdAt: raw.createdAt ?? '',
    updatedAt: raw.updatedAt ?? '',
  };
}

function mapBinding(raw: any): LibraryBinding {
  return {
    id: raw.id ?? undefined,
    purpose: raw.bindingPurpose as AIPurpose,
    credentialId: raw.providerCredentialId ?? undefined,
    presetId: raw.modelPresetId ?? undefined,
    state: (raw.bindingState === 'configured' || raw.bindingState === 'active') ? 'configured' : raw.bindingState === 'invalid' ? 'invalid' : 'unconfigured',
  };
}

function mapPricing(raw: any, providers: AIProvider[], models: any[]): PricingRule {
  const model = models.find((m: any) => m.id === raw.modelCatalogId);
  const provider = model ? providers.find(p => p.id === model.providerCatalogId) : undefined;
  return {
    id: raw.id,
    provider: provider?.displayName ?? '',
    model: model?.modelName ?? raw.modelCatalogId ?? '',
    billingUnit: raw.billingUnit ?? '',
    unitPrice: parseFloat(raw.unitPrice) || 0,
    currency: raw.currencyCode ?? 'USD',
    effectiveFrom: raw.effectiveFrom ? new Date(raw.effectiveFrom).toISOString().slice(0, 10) : '',
    effectiveTo: raw.effectiveTo ? new Date(raw.effectiveTo).toISOString().slice(0, 10) : undefined,
    sourceOrigin: raw.catalogScope ?? 'catalog',
  };
}

function mapOps(raw: any): OperationsSnapshot {
  const state = raw.state ?? raw;
  return {
    queueDepth: state.queueDepth ?? 0,
    runningAttempts: state.runningAttempts ?? 0,
    readableDocCount: state.readableDocumentCount ?? 0,
    failedDocCount: state.failedDocumentCount ?? 0,
    healthState: state.degradedState === 'healthy' ? 'healthy' : state.degradedState === 'critical' ? 'critical' : 'degraded',
    knowledgeGenerationState: state.knowledgeGenerationState ?? 'unknown',
    lastRecomputedAt: state.lastRecomputedAt ?? '',
    warnings: (raw.warnings ?? []).map((w: any) => w.warningKind ?? String(w)),
  };
}

function mapAudit(raw: any): AuditEvent {
  return {
    id: raw.id,
    action: raw.actionKind ?? '',
    result: raw.resultKind === 'success' ? 'success' : 'failure',
    timestamp: raw.createdAt ?? '',
    message: raw.redactedMessage ?? raw.actionKind ?? '',
    subjectSummary: (raw.subjects ?? []).map((s: any) => `${s.subjectKind}:${s.subjectId}`).join(', ') || '',
    actor: raw.actorPrincipalId ?? 'system',
  };
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
  const { activeWorkspace, activeLibrary, locale, setLocale } = useApp();
  const [activeTab, setActiveTab] = useState('access');

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

  // AI tab state
  const [providers, setProviders] = useState<AIProvider[]>([]);
  const [credentials, setCredentials] = useState<AICredential[]>([]);
  const [presets, setPresets] = useState<ModelPreset[]>([]);
  const [bindings, setBindings] = useState<LibraryBinding[]>([]);
  const [aiLoading, setAiLoading] = useState(false);
  const [aiError, setAiError] = useState<string | null>(null);

  const [createCredOpen, setCreateCredOpen] = useState(false);
  const [credProvider, setCredProvider] = useState('');
  const [credLabel, setCredLabel] = useState('');
  const [credApiKey, setCredApiKey] = useState('');

  const [createPresetOpen, setCreatePresetOpen] = useState(false);
  const [presetName, setPresetName] = useState('');
  const [presetModelId, setPresetModelId] = useState('');
  const [presetSystemPrompt, setPresetSystemPrompt] = useState('');
  const [presetTemperature, setPresetTemperature] = useState('0.3');
  const [presetTopP, setPresetTopP] = useState('0.9');
  const [presetMaxTokens, setPresetMaxTokens] = useState('');
  const [presetSaving, setPresetSaving] = useState(false);

  const [createPricingOpen, setCreatePricingOpen] = useState(false);
  const [pricingModelId, setPricingModelId] = useState('');
  const [pricingBillingUnit, setPricingBillingUnit] = useState('');
  const [pricingUnitPrice, setPricingUnitPrice] = useState('');
  const [pricingCurrency, setPricingCurrency] = useState('USD');
  const [pricingFrom, setPricingFrom] = useState('');
  const [pricingTo, setPricingTo] = useState('');
  const [pricingSaving, setPricingSaving] = useState(false);

  // Binding editor state
  const [editingBinding, setEditingBinding] = useState<string | null>(null);
  const [bindingCredId, setBindingCredId] = useState('');
  const [bindingPresetId, setBindingPresetId] = useState('');
  const [bindingSaving, setBindingSaving] = useState(false);

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
  const [audit, setAudit] = useState<AuditEvent[]>([]);
  const [auditLoading, setAuditLoading] = useState(false);

  // Raw model catalog for pricing resolution
  const [rawModels, setRawModels] = useState<any[]>([]);

  // ── Data fetchers ──

  const loadTokens = useCallback(() => {
    setTokensLoading(true);
    setTokensError(null);
    adminApi.listTokens()
      .then((data: any) => {
        const list = Array.isArray(data) ? data : [];
        setTokens(list.map(mapToken));
      })
      .catch(err => setTokensError(err?.message ?? 'Failed to load tokens'))
      .finally(() => setTokensLoading(false));
  }, []);

  const loadAiData = useCallback(() => {
    setAiLoading(true);
    setAiError(null);
    Promise.all([
      adminApi.listProviders(),
      adminApi.listCredentials(),
      adminApi.listModelPresets(),
      adminApi.listModels(),
      activeLibrary ? adminApi.listLibraryBindings(activeLibrary.id) : Promise.resolve([]),
    ])
      .then(([provRaw, credRaw, presetRaw, modelRaw, bindRaw]) => {
        const provList = (Array.isArray(provRaw) ? provRaw : []).map(mapProvider);
        setProviders(provList);
        setCredentials((Array.isArray(credRaw) ? credRaw : []).map((c: any) => mapCredential(c, provList)));
        setPresets((Array.isArray(presetRaw) ? presetRaw : []).map(mapPreset));
        setRawModels(Array.isArray(modelRaw) ? modelRaw : []);
        setBindings((Array.isArray(bindRaw) ? bindRaw : []).map(mapBinding));
      })
      .catch(err => setAiError(err?.message ?? 'Failed to load AI configuration'))
      .finally(() => setAiLoading(false));
  }, [activeLibrary]);

  const loadPricing = useCallback(() => {
    setPricingLoading(true);
    adminApi.listPrices()
      .then((data: any) => {
        const list = Array.isArray(data) ? data : [];
        setPricing(list.map((p: any) => mapPricing(p, providers, rawModels)));
      })
      .catch((err: any) => toast.error(err?.message || "Failed to load pricing"))
      .finally(() => setPricingLoading(false));
  }, [providers, rawModels]);

  const loadOps = useCallback(() => {
    if (!activeLibrary) return;
    setOpsLoading(true);
    setOpsError(null);
    dashboardApi.getLibraryState(activeLibrary.id)
      .then((data: any) => setOps(mapOps(data)))
      .catch(err => setOpsError(err?.message ?? 'Failed to load operations'))
      .finally(() => setOpsLoading(false));
  }, [activeLibrary]);

  const loadAudit = useCallback(() => {
    setAuditLoading(true);
    adminApi.listAuditEvents()
      .then((data: any) => {
        const list = Array.isArray(data) ? data : [];
        setAudit(list.map(mapAudit));
      })
      .catch((err: any) => toast.error(err?.message || "Failed to load audit events"))
      .finally(() => setAuditLoading(false));
  }, []);

  // ── Load data per tab ──

  useEffect(() => {
    if (activeTab === 'access') loadTokens();
  }, [activeTab, loadTokens]);

  useEffect(() => {
    if (activeTab === 'ai') loadAiData();
  }, [activeTab, loadAiData]);

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

  // ── Actions ──

  const handleCreateToken = () => {
    setMintingToken(true);
    adminApi.mintToken(tokenLabel)
      .then((data: any) => {
        setCreatedToken(data.token ?? '');
        setCreateTokenOpen(false);
        setShowToken(true);
        loadTokens();
      })
      .catch((err: any) => toast.error(err?.message || "Failed to create token"))
      .finally(() => setMintingToken(false));
  };

  const handleRevokeToken = (token: APIToken) => {
    adminApi.revokeToken(token.id)
      .then(() => {
        loadTokens();
        setSelectedToken(null);
      })
      .catch((err: any) => toast.error(err?.message || "Failed to revoke token"));
  };

  const handleCreateCredential = () => {
    adminApi.createCredential({ workspaceId: activeWorkspace?.id, providerCatalogId: credProvider, label: credLabel, apiKey: credApiKey })
      .then(() => {
        setCreateCredOpen(false);
        setCredProvider('');
        setCredLabel('');
        setCredApiKey('');
        loadAiData();
      })
      .catch((err: any) => toast.error(err?.message || "Failed to create credential"));
  };

  const handleSaveBinding = (binding: LibraryBinding) => {
    if (!activeWorkspace || !activeLibrary || !bindingCredId || !bindingPresetId) return;
    setBindingSaving(true);
    const isUpdate = binding.id && binding.state !== 'unconfigured';
    const request = isUpdate
      ? adminApi.updateLibraryBinding(binding.id!, {
          providerCredentialId: bindingCredId,
          modelPresetId: bindingPresetId,
          bindingState: 'active',
        })
      : adminApi.createLibraryBinding({
          workspaceId: activeWorkspace.id,
          libraryId: activeLibrary.id,
          bindingPurpose: binding.purpose,
          providerCredentialId: bindingCredId,
          modelPresetId: bindingPresetId,
        });
    request
      .then(() => {
        setEditingBinding(null);
        setBindingCredId('');
        setBindingPresetId('');
        loadAiData();
      })
      .catch((err: any) => toast.error(err?.message || "Failed to save binding"))
      .finally(() => setBindingSaving(false));
  };

  const handleCreatePreset = () => {
    if (!activeWorkspace || !presetName.trim() || !presetModelId) return;
    setPresetSaving(true);
    adminApi.createModelPreset({
      workspaceId: activeWorkspace.id,
      modelCatalogId: presetModelId,
      presetName: presetName.trim(),
      systemPrompt: presetSystemPrompt.trim() || null,
      temperature: parseFloat(presetTemperature) || null,
      topP: parseFloat(presetTopP) || null,
      maxOutputTokensOverride: presetMaxTokens ? parseInt(presetMaxTokens, 10) : null,
      extraParametersJson: {},
    })
      .then(() => {
        toast.success("Model preset created");
        setCreatePresetOpen(false);
        setPresetName(''); setPresetModelId(''); setPresetSystemPrompt('');
        setPresetTemperature('0.3'); setPresetTopP('0.9'); setPresetMaxTokens('');
        loadAiData();
      })
      .catch((err: any) => toast.error(err?.message || "Failed to create model preset"))
      .finally(() => setPresetSaving(false));
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
        toast.success("Pricing override created");
        setCreatePricingOpen(false);
        setPricingModelId(''); setPricingBillingUnit(''); setPricingUnitPrice('');
        setPricingCurrency('USD'); setPricingFrom(''); setPricingTo('');
        loadPricing();
      })
      .catch((err: any) => toast.error(err?.message || "Failed to create pricing override"))
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

  return (
    <div className="flex-1 flex flex-col overflow-hidden">
      <div className="page-header">
        <h1 className="text-lg font-bold tracking-tight">{t('admin.title')}</h1>
        <p className="text-sm text-muted-foreground">
          {activeWorkspace?.name}{activeLibrary ? <><span className="mx-2 text-border">&middot;</span>{activeLibrary.name}</> : ''}
        </p>
      </div>

      <Tabs value={activeTab} onValueChange={setActiveTab} className="flex-1 flex flex-col overflow-hidden">
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
                  <span className="text-muted-foreground flex items-center gap-1.5"><Loader2 className="h-3 w-3 animate-spin" /> Loading...</span>
                ) : tokensError ? (
                  <span className="text-status-failed">{tokensError}</span>
                ) : (
                  <>
                    <span className="text-muted-foreground">{tokens.length} total</span>
                    <span className="text-status-ready">{tokens.filter(t => t.status === 'active').length} active</span>
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
                    <span className={`status-badge ${tokenStatusCls(token.status)}`}>{token.status}</span>
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
                    <span className={`status-badge ${tokenStatusCls(selectedToken.status)}`}>{selectedToken.status}</span>
                  </div>
                  <div className="space-y-2.5 text-sm">
                    {[
                      ['Prefix', selectedToken.tokenPrefix + '...'],
                      ['Scope', selectedToken.scopeSummary],
                      ['Principal', selectedToken.principalLabel],
                      ['Issued by', selectedToken.issuedBy],
                      ['Expires', selectedToken.expiresAt ? new Date(selectedToken.expiresAt).toLocaleDateString() : 'Never'],
                      ['Last used', selectedToken.lastUsedAt ? new Date(selectedToken.lastUsedAt).toLocaleDateString() : 'Never'],
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
            <div className="mb-5 flex items-center justify-between">
              <h2 className="text-base font-bold tracking-tight">{t('admin.operations')}</h2>
              {opsLoading ? (
                <span className="text-xs text-muted-foreground flex items-center gap-1.5"><Loader2 className="h-3 w-3 animate-spin" /> Loading...</span>
              ) : opsError ? (
                <span className="text-xs text-status-failed">{opsError}</span>
              ) : ops ? (
                <span className={`status-badge ${ops.healthState === 'healthy' ? 'status-ready' : ops.healthState === 'degraded' ? 'status-warning' : 'status-failed'}`}>
                  {ops.healthState}
                </span>
              ) : !activeLibrary ? (
                <span className="text-xs text-muted-foreground">Select a library</span>
              ) : null}
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
                <div className="workbench-surface p-5 mb-6 text-sm space-y-2.5">
                  <div className="flex justify-between"><span className="text-muted-foreground">Knowledge Generation</span><span className="font-bold">{ops.knowledgeGenerationState}</span></div>
                  <div className="flex justify-between"><span className="text-muted-foreground">Last Recomputed</span><span className="font-bold">{ops.lastRecomputedAt ? new Date(ops.lastRecomputedAt).toLocaleString() : 'Never'}</span></div>
                </div>
              </>
            ) : !opsLoading && !opsError && (
              <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">
                {activeLibrary ? 'No operations data available.' : 'Select a library to view operations.'}
              </div>
            )}

            <h3 className="text-sm font-bold tracking-tight mb-3">{t('admin.auditLog')}</h3>
            {auditLoading ? (
              <div className="flex items-center gap-2 text-sm text-muted-foreground p-4"><Loader2 className="h-4 w-4 animate-spin" /> Loading audit events...</div>
            ) : audit.length === 0 ? (
              <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">No audit events found.</div>
            ) : (
              <div className="workbench-surface divide-y">
                {audit.slice(0, 20).map(evt => (
                  <div key={evt.id} className="p-4 flex items-start gap-3 transition-colors hover:bg-accent/30">
                    <div className={`mt-0.5 ${evt.result === 'success' ? 'text-status-ready' : 'text-status-failed'}`}>
                      {evt.result === 'success' ? <CheckCircle2 className="h-4 w-4" /> : <XCircle className="h-4 w-4" />}
                    </div>
                    <div className="flex-1 min-w-0">
                      <div className="text-sm font-semibold">{evt.message}</div>
                      <div className="text-xs text-muted-foreground mt-0.5 font-medium">{evt.action} · {evt.actor} · {new Date(evt.timestamp).toLocaleString()}</div>
                    </div>
                  </div>
                ))}
              </div>
            )}
          </TabsContent>

          {/* AI TAB */}
          <TabsContent value="ai" className="mt-0 p-6 space-y-6 animate-fade-in">
            {aiLoading ? (
              <div className="flex items-center gap-2 text-sm text-muted-foreground p-4"><Loader2 className="h-4 w-4 animate-spin" /> Loading AI configuration...</div>
            ) : aiError ? (
              <div className="text-sm text-status-failed p-4">{aiError}</div>
            ) : (
              <>
                <div>
                  <h2 className="text-sm font-bold tracking-tight mb-3">{t('admin.providers')}</h2>
                  <div className="grid md:grid-cols-3 gap-3">
                    {providers.map(p => (
                      <div key={p.id} className="workbench-surface p-5 transition-shadow duration-200 hover:shadow-lifted">
                        <div className="flex items-center justify-between mb-3">
                          <span className="text-sm font-bold">{p.displayName}</span>
                          <span className={`status-badge ${p.lifecycleState === 'active' ? 'status-ready' : 'status-processing'}`}>{p.lifecycleState}</span>
                        </div>
                        <div className="text-xs text-muted-foreground space-y-0.5 font-medium">
                          <div>{p.kind} · {p.apiStyle}</div>
                          <div>{p.modelCount} models · {p.credentialCount} credentials</div>
                        </div>
                      </div>
                    ))}
                    {providers.length === 0 && (
                      <div className="text-sm text-muted-foreground col-span-3 text-center p-8 border rounded-xl bg-surface-sunken">No providers found.</div>
                    )}
                  </div>
                </div>

                <div>
                  <div className="flex items-center justify-between mb-3">
                    <h2 className="text-sm font-bold tracking-tight">{t('admin.credentials')}</h2>
                    <Button size="sm" variant="outline" onClick={() => setCreateCredOpen(true)}><Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.add')}</Button>
                  </div>
                  <div className="space-y-1.5">
                    {credentials.map(c => (
                      <div key={c.id} className="workbench-surface p-4 flex items-center gap-3 transition-shadow duration-200 hover:shadow-lifted">
                        <div className="w-9 h-9 rounded-xl bg-surface-sunken flex items-center justify-center shrink-0">
                          <Shield className="h-4 w-4 text-muted-foreground" />
                        </div>
                        <div className="flex-1">
                          <div className="text-sm font-bold">{c.label}</div>
                          <div className="text-xs text-muted-foreground mt-0.5 font-medium">{c.providerName} · <span className="font-mono">{c.apiKeySummary}</span></div>
                        </div>
                        <span className={`status-badge ${c.state === 'valid' ? 'status-ready' : c.state === 'invalid' ? 'status-failed' : 'status-warning'}`}>{c.state}</span>
                      </div>
                    ))}
                    {credentials.length === 0 && (
                      <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">No credentials configured.</div>
                    )}
                  </div>
                </div>

                <div>
                  <div className="flex items-center justify-between mb-3">
                    <h2 className="text-sm font-bold tracking-tight">{t('admin.modelPresets')}</h2>
                    <Button size="sm" variant="outline" onClick={() => setCreatePresetOpen(true)}><Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.add')}</Button>
                  </div>
                  <div className="space-y-1.5">
                    {presets.map(p => (
                      <div key={p.id} className="workbench-surface p-4 flex items-center gap-3 transition-shadow duration-200 hover:shadow-lifted">
                        <div className="w-9 h-9 rounded-xl bg-surface-sunken flex items-center justify-center shrink-0">
                          <Brain className="h-4 w-4 text-muted-foreground" />
                        </div>
                        <div className="flex-1">
                          <div className="text-sm font-bold">{p.presetName}</div>
                          <div className="text-xs text-muted-foreground mt-0.5 font-medium"><span className="font-mono">{p.model}</span> · temp={p.temperature} · topP={p.topP}</div>
                        </div>
                      </div>
                    ))}
                    {presets.length === 0 && (
                      <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">No presets configured.</div>
                    )}
                  </div>
                </div>

                <div>
                  <h2 className="text-sm font-bold tracking-tight mb-3">{t('admin.libraryBindings')}</h2>
                  {!activeLibrary ? (
                    <div className="text-sm text-muted-foreground p-5 border rounded-xl bg-surface-sunken text-center font-medium">Select a library to configure bindings.</div>
                  ) : (
                    <div className="space-y-2">
                      {bindings.map(b => {
                        const isEditing = editingBinding === b.purpose;
                        return (
                          <div key={b.purpose} className="workbench-surface p-5 transition-shadow duration-200 hover:shadow-lifted">
                            <div className="flex items-center justify-between mb-2">
                              <div className="flex items-center gap-2.5">
                                <span className="text-sm font-bold font-mono">{b.purpose}</span>
                                <span className={`status-badge ${b.state === 'configured' ? 'status-ready' : b.state === 'unconfigured' ? 'status-warning' : 'status-failed'}`}>{b.state}</span>
                              </div>
                              {!isEditing && (
                                <Button
                                  size="sm"
                                  variant="outline"
                                  onClick={() => {
                                    setEditingBinding(b.purpose);
                                    setBindingCredId(b.credentialId ?? '');
                                    setBindingPresetId(b.presetId ?? '');
                                  }}
                                >
                                  <Settings className="h-3 w-3 mr-1.5" /> {b.state === 'configured' ? t('admin.edit') : t('admin.configure')}
                                </Button>
                              )}
                            </div>
                            {!isEditing && b.state === 'configured' && (
                              <div className="text-xs text-muted-foreground font-medium">
                                Credential: <span className="font-bold text-foreground">{credentials.find(c => c.id === b.credentialId)?.label ?? '...'}</span> ·
                                Preset: <span className="font-bold text-foreground">{presets.find(p => p.id === b.presetId)?.presetName ?? '...'}</span>
                              </div>
                            )}
                            {!isEditing && b.state === 'unconfigured' && (
                              <div className="text-xs flex items-center gap-1.5 font-bold" style={{ color: 'hsl(var(--status-warning))' }}>
                                <AlertTriangle className="h-3 w-3" /> Not configured — click Configure to select a credential and model preset
                              </div>
                            )}
                            {isEditing && (
                              <div className="mt-3 space-y-3 p-4 rounded-xl bg-surface-sunken border border-border/50">
                                <div>
                                  <Label className="text-xs font-semibold">{t('admin.credential')}</Label>
                                  <Select value={bindingCredId} onValueChange={setBindingCredId}>
                                    <SelectTrigger className="mt-1.5 h-9 text-sm">
                                      <SelectValue placeholder={t('admin.selectCredential')} />
                                    </SelectTrigger>
                                    <SelectContent>
                                      {credentials.map(c => (
                                        <SelectItem key={c.id} value={c.id}>
                                          {c.label} ({c.providerName})
                                        </SelectItem>
                                      ))}
                                    </SelectContent>
                                  </Select>
                                </div>
                                <div>
                                  <Label className="text-xs font-semibold">{t('admin.modelPreset')}</Label>
                                  <Select value={bindingPresetId} onValueChange={setBindingPresetId}>
                                    <SelectTrigger className="mt-1.5 h-9 text-sm">
                                      <SelectValue placeholder={t('admin.selectPreset')} />
                                    </SelectTrigger>
                                    <SelectContent>
                                      {presets.map(p => (
                                        <SelectItem key={p.id} value={p.id}>
                                          {p.presetName}
                                        </SelectItem>
                                      ))}
                                    </SelectContent>
                                  </Select>
                                </div>
                                <div className="flex gap-2 pt-1">
                                  <Button
                                    size="sm"
                                    disabled={!bindingCredId || !bindingPresetId || bindingSaving}
                                    onClick={() => handleSaveBinding(b)}
                                  >
                                    {bindingSaving ? <><Loader2 className="h-3 w-3 animate-spin mr-1.5" /> {t('admin.saving')}</> : t('admin.save')}
                                  </Button>
                                  <Button
                                    size="sm"
                                    variant="outline"
                                    onClick={() => {
                                      setEditingBinding(null);
                                      setBindingCredId('');
                                      setBindingPresetId('');
                                    }}
                                  >
                                    Cancel
                                  </Button>
                                </div>
                              </div>
                            )}
                          </div>
                        );
                      })}
                      {bindings.length === 0 && (
                        <div className="text-sm text-muted-foreground text-center p-8 border rounded-xl bg-surface-sunken">No bindings found for this library.</div>
                      )}
                    </div>
                  )}
                </div>
              </>
            )}
          </TabsContent>

          {/* PRICING TAB */}
          <TabsContent value="pricing" className="mt-0 p-6 animate-fade-in">
            <div className="flex items-center justify-between mb-5">
              <h2 className="text-base font-bold tracking-tight">{t('admin.pricing')}</h2>
              <div className="flex gap-2">
                <Select value={pricingProvider} onValueChange={setPricingProvider}>
                  <SelectTrigger className="h-9 w-36 text-sm"><SelectValue placeholder="Provider" /></SelectTrigger>
                  <SelectContent>
                    <SelectItem value="all">All Providers</SelectItem>
                    {providers.map(p => <SelectItem key={p.id} value={p.displayName}>{p.displayName}</SelectItem>)}
                  </SelectContent>
                </Select>
                <div className="relative">
                  <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
                  <Input className="h-9 pl-9 w-48 text-sm" placeholder="Search models..." value={pricingSearch} onChange={e => setPricingSearch(e.target.value)} />
                </div>
                <Button size="sm" variant="outline" onClick={() => setCreatePricingOpen(true)}>
                  <Plus className="h-3.5 w-3.5 mr-1.5" /> Override
                </Button>
              </div>
            </div>
            {pricingLoading ? (
              <div className="flex items-center gap-2 text-sm text-muted-foreground p-4"><Loader2 className="h-4 w-4 animate-spin" /> Loading pricing...</div>
            ) : (
              <div className="workbench-surface overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="border-b text-left">
                      <th className="px-4 py-3 section-label">Provider</th>
                      <th className="px-4 py-3 section-label">Model</th>
                      <th className="px-4 py-3 section-label">Billing Unit</th>
                      <th className="px-4 py-3 section-label">Price</th>
                      <th className="px-4 py-3 section-label">Effective From</th>
                      <th className="px-4 py-3 section-label">Source</th>
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
                      <tr><td colSpan={6} className="text-center p-8 text-sm text-muted-foreground">No pricing data found.</td></tr>
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
            <div><Label>Label</Label><Input value={tokenLabel} onChange={e => setTokenLabel(e.target.value)} placeholder="Production API" className="mt-2" /></div>
            <div><Label>Expiry</Label><Select value={tokenExpiry} onValueChange={setTokenExpiry}><SelectTrigger className="mt-2"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="30">30 days</SelectItem><SelectItem value="90">90 days</SelectItem><SelectItem value="365">365 days</SelectItem><SelectItem value="never">Never</SelectItem></SelectContent></Select></div>
            <div><Label>Scope</Label><Select value={tokenScope} onValueChange={v => { setTokenScope(v as typeof tokenScope); setSelectedPermissions([]); }}><SelectTrigger className="mt-2"><SelectValue /></SelectTrigger><SelectContent><SelectItem value="workspace">Workspace</SelectItem><SelectItem value="library">Library</SelectItem></SelectContent></Select></div>
            <div>
              <Label>Permissions</Label>
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
          <DialogFooter><Button variant="outline" onClick={() => setCreateTokenOpen(false)}>Cancel</Button><Button onClick={handleCreateToken} disabled={!tokenLabel.trim() || mintingToken}>{mintingToken ? 'Creating...' : 'Create'}</Button></DialogFooter>
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

      <Dialog open={createCredOpen} onOpenChange={setCreateCredOpen}>
        <DialogContent>
          <DialogHeader><DialogTitle>{t('admin.addCredential')}</DialogTitle><DialogDescription>{t('admin.addCredentialDesc')}</DialogDescription></DialogHeader>
          <div className="space-y-4">
            <div><Label>Provider</Label><Select value={credProvider} onValueChange={setCredProvider}><SelectTrigger className="mt-2"><SelectValue placeholder="Select provider" /></SelectTrigger><SelectContent>{providers.map(p => <SelectItem key={p.id} value={p.id}>{p.displayName}</SelectItem>)}</SelectContent></Select></div>
            <div><Label>Label</Label><Input value={credLabel} onChange={e => setCredLabel(e.target.value)} placeholder="My API Key" className="mt-2" /></div>
            <div><Label>API Key</Label><Input type="password" value={credApiKey} onChange={e => setCredApiKey(e.target.value)} placeholder="sk-..." className="mt-2" /></div>
          </div>
          <DialogFooter><Button variant="outline" onClick={() => setCreateCredOpen(false)}>Cancel</Button><Button disabled={!credProvider || !credLabel.trim() || !credApiKey.trim()} onClick={handleCreateCredential}>Save</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={createPresetOpen} onOpenChange={v => { setCreatePresetOpen(v); if (!v) { setPresetName(''); setPresetModelId(''); setPresetSystemPrompt(''); setPresetTemperature('0.3'); setPresetTopP('0.9'); setPresetMaxTokens(''); } }}>
        <DialogContent className="max-w-md">
          <DialogHeader><DialogTitle>{t('admin.addPreset')}</DialogTitle></DialogHeader>
          <div className="space-y-4">
            <div><Label>Preset Name</Label><Input placeholder="My Preset" className="mt-2" value={presetName} onChange={e => setPresetName(e.target.value)} /></div>
            <div><Label>Model</Label><Select value={presetModelId} onValueChange={setPresetModelId}><SelectTrigger className="mt-2"><SelectValue placeholder="Select model" /></SelectTrigger><SelectContent>{rawModels.map((m: any) => <SelectItem key={m.id} value={m.id}>{m.modelName ?? m.id}</SelectItem>)}</SelectContent></Select></div>
            <div><Label>System Prompt</Label><Textarea placeholder="Optional system prompt..." rows={3} className="mt-2" value={presetSystemPrompt} onChange={e => setPresetSystemPrompt(e.target.value)} /></div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>Temperature</Label><Input type="number" step="0.1" min="0" max="2" className="mt-2" value={presetTemperature} onChange={e => setPresetTemperature(e.target.value)} /></div>
              <div><Label>Top P</Label><Input type="number" step="0.1" min="0" max="1" className="mt-2" value={presetTopP} onChange={e => setPresetTopP(e.target.value)} /></div>
            </div>
            <div><Label>Max Output Tokens</Label><Input type="number" placeholder="Optional" className="mt-2" value={presetMaxTokens} onChange={e => setPresetMaxTokens(e.target.value)} /></div>
          </div>
          <DialogFooter><Button variant="outline" onClick={() => setCreatePresetOpen(false)}>Cancel</Button><Button disabled={!presetName.trim() || !presetModelId || presetSaving} onClick={handleCreatePreset}>{presetSaving ? 'Saving...' : 'Save'}</Button></DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={createPricingOpen} onOpenChange={v => { setCreatePricingOpen(v); if (!v) { setPricingModelId(''); setPricingBillingUnit(''); setPricingUnitPrice(''); setPricingCurrency('USD'); setPricingFrom(''); setPricingTo(''); } }}>
        <DialogContent className="max-w-md">
          <DialogHeader><DialogTitle>{t('admin.addPricingOverride')}</DialogTitle></DialogHeader>
          <div className="space-y-4">
            <div><Label>Model</Label><Select value={pricingModelId} onValueChange={setPricingModelId}><SelectTrigger className="mt-2"><SelectValue placeholder="Select model" /></SelectTrigger><SelectContent>{rawModels.map((m: any) => <SelectItem key={m.id} value={m.id}>{m.modelName ?? m.id}</SelectItem>)}</SelectContent></Select></div>
            <div><Label>Billing Unit</Label><Select value={pricingBillingUnit} onValueChange={setPricingBillingUnit}><SelectTrigger className="mt-2"><SelectValue placeholder="Select unit" /></SelectTrigger><SelectContent><SelectItem value="per_1m_input_tokens">Per 1M Input Tokens</SelectItem><SelectItem value="per_1m_cached_input_tokens">Per 1M Cached Input Tokens</SelectItem><SelectItem value="per_1m_output_tokens">Per 1M Output Tokens</SelectItem></SelectContent></Select></div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>Unit Price</Label><Input type="number" step="0.01" placeholder="0.00" className="mt-2" value={pricingUnitPrice} onChange={e => setPricingUnitPrice(e.target.value)} /></div>
              <div><Label>Currency</Label><Input className="mt-2" value={pricingCurrency} onChange={e => setPricingCurrency(e.target.value)} /></div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div><Label>Effective From</Label><Input type="date" className="mt-2" value={pricingFrom} onChange={e => setPricingFrom(e.target.value)} /></div>
              <div><Label>Effective To</Label><Input type="date" className="mt-2" value={pricingTo} onChange={e => setPricingTo(e.target.value)} /></div>
            </div>
          </div>
          <DialogFooter><Button variant="outline" onClick={() => setCreatePricingOpen(false)}>Cancel</Button><Button disabled={!pricingModelId || !pricingBillingUnit || !pricingUnitPrice || !pricingFrom || pricingSaving} onClick={handleCreatePricing}>{pricingSaving ? 'Saving...' : 'Save'}</Button></DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
