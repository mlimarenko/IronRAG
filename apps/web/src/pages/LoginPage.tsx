import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { useApp } from '@/contexts/AppContext';
import { authApi } from '@/api';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Loader2, FileText, Share2, Brain, Database, AlertCircle, CheckCircle2, Sparkles, Globe } from 'lucide-react';
import type { AIPurpose } from '@/types';
import { AVAILABLE_LOCALES } from '@/types';

const AI_PURPOSES: { purpose: AIPurpose; label: string; description: string }[] = [
  { purpose: 'extract_graph', label: 'Graph Extraction', description: 'Extract entities and relations from documents' },
  { purpose: 'embed_chunk', label: 'Chunk Embedding', description: 'Generate vector embeddings for document chunks' },
  { purpose: 'query_answer', label: 'Query Answering', description: 'Answer questions grounded in library content' },
  { purpose: 'vision', label: 'Vision', description: 'Process images and visual content in documents' },
];

interface BootstrapProvider {
  id: string;
  providerKind: string;
  displayName: string;
  credentialSource: string;
}

interface BootstrapModel {
  id: string;
  providerCatalogId: string;
  modelName: string;
  capabilityKind: string;
  allowedBindingPurposes: string[];
}

interface BootstrapBinding {
  provider: string;
  model: string;
  modelCatalogId: string;
  apiKey: string;
}

export default function LoginPage() {
  const { t } = useTranslation();
  const { login, bootstrapSetup, isBootstrapRequired, locale, setLocale } = useApp();
  const navigate = useNavigate();

  const [loginVal, setLoginVal] = useState('');
  const [password, setPassword] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');

  const [displayName, setDisplayName] = useState('');
  const [bindings, setBindings] = useState<Record<AIPurpose, BootstrapBinding>>({
    extract_graph: { provider: '', model: '', modelCatalogId: '', apiKey: '' },
    embed_chunk: { provider: '', model: '', modelCatalogId: '', apiKey: '' },
    query_answer: { provider: '', model: '', modelCatalogId: '', apiKey: '' },
    vision: { provider: '', model: '', modelCatalogId: '', apiKey: '' },
  });
  const [bootstrapError, setBootstrapError] = useState('');
  const [aiProviders, setAiProviders] = useState<BootstrapProvider[]>([]);
  const [aiModels, setAiModels] = useState<BootstrapModel[]>([]);

  useEffect(() => {
    if (isBootstrapRequired) {
      authApi.getBootstrapStatus().then(status => {
        if (status.aiSetup) {
          setAiProviders(status.aiSetup.providers ?? []);
          setAiModels(status.aiSetup.models ?? []);
        }
      }).catch(() => { /* bootstrap status fetch is best-effort */ });
    }
  }, [isBootstrapRequired]);

  const handleLogin = async () => {
    if (!loginVal.trim() || !password.trim()) { setError(t('login.fillAllFields')); return; }
    setSubmitting(true);
    setError('');
    try {
      await login(loginVal, password);
      navigate('/dashboard');
    } catch (err) {
      setError(err instanceof Error ? err.message : t('login.loginFailed'));
    } finally {
      setSubmitting(false);
    }
  };

  const handleBootstrap = async () => {
    if (!loginVal.trim() || !password.trim() || !displayName.trim()) {
      setBootstrapError(t('login.fillRequired'));
      return;
    }
    setSubmitting(true);
    setBootstrapError('');
    try {
      // Build AI setup payload from bindings
      const configuredBindings = Object.entries(bindings).filter(([, b]) => b.provider && b.modelCatalogId);
      const uniqueProviders = [...new Set(configuredBindings.map(([, b]) => b.provider))];
      const aiSetup = configuredBindings.length > 0 ? {
        credentials: uniqueProviders.map(pk => {
          const binding = configuredBindings.find(([, b]) => b.provider === pk);
          return { providerKind: pk, apiKey: binding?.[1].apiKey || undefined };
        }),
        bindingSelections: configuredBindings.map(([purpose, b]) => ({
          bindingPurpose: purpose,
          providerKind: b.provider,
          modelCatalogId: b.modelCatalogId,
        })),
      } : undefined;

      await bootstrapSetup({ login: loginVal, password, displayName, aiSetup });
      navigate('/dashboard');
    } catch (err) {
      setBootstrapError(err instanceof Error ? err.message : t('login.setupFailed'));
    } finally {
      setSubmitting(false);
    }
  };

  const updateBinding = (purpose: AIPurpose, field: keyof BootstrapBinding, value: string) => {
    setBindings(prev => {
      const updated = { ...prev[purpose], [field]: value };
      // When selecting a model by display name, also set its catalog ID
      if (field === 'model') {
        const modelEntry = aiModels.find(m => m.modelName === value);
        updated.modelCatalogId = modelEntry?.id ?? '';
      }
      return { ...prev, [purpose]: updated };
    });
  };

  return (
    <div className="min-h-screen flex bg-background">
      {/* Left: Product explainer — rich atmospheric panel */}
      <div className="hidden lg:flex lg:w-[460px] xl:w-[520px] flex-col justify-center p-12 relative overflow-hidden" style={{
        background: 'linear-gradient(170deg, hsl(225 32% 12%), hsl(225 32% 6%) 60%, hsl(224 40% 10%))',
      }}>
        {/* Ambient glow effects */}
        <div className="absolute inset-0 pointer-events-none">
          <div className="absolute top-0 left-0 w-full h-full" style={{
            background: 'radial-gradient(ellipse 60% 50% at 30% 20%, hsl(224 76% 48% / 0.08) 0%, transparent 60%)',
          }} />
          <div className="absolute bottom-0 right-0 w-full h-full" style={{
            background: 'radial-gradient(ellipse 50% 40% at 70% 90%, hsl(38 92% 50% / 0.04) 0%, transparent 50%)',
          }} />
        </div>

        <div className="space-y-10 relative z-10">
          <div>
            <div className="flex items-center gap-3 mb-5">
              <div className="w-9 h-9 rounded-xl flex items-center justify-center text-sm font-black" style={{
                background: 'linear-gradient(135deg, hsl(224 76% 52%), hsl(224 76% 40%))',
                color: 'white',
                boxShadow: '0 4px 16px -4px hsl(224 76% 48% / 0.5)',
              }}>R</div>
              <h1 className="text-2xl font-bold tracking-tight" style={{ color: 'hsl(var(--shell-foreground))' }}>RustRAG</h1>
            </div>
            <p className="text-sm leading-relaxed max-w-[320px]" style={{ color: 'hsl(224 14% 55%)' }}>
              {t('login.tagline')}
            </p>
          </div>
          <div className="space-y-5">
            {[
              { icon: FileText, title: t('login.featureDocs'), desc: t('login.featureDocsDesc'), color: '224 76% 52%' },
              { icon: Database, title: t('login.featureEntities'), desc: t('login.featureEntitiesDesc'), color: '152 62% 42%' },
              { icon: Share2, title: t('login.featureGraph'), desc: t('login.featureGraphDesc'), color: '38 92% 55%' },
              { icon: Brain, title: t('login.featureAi'), desc: t('login.featureAiDesc'), color: '270 60% 55%' },
            ].map(item => (
              <div key={item.title} className="flex gap-4 group">
                <div className="mt-0.5 p-2.5 rounded-xl shrink-0 transition-all duration-200 group-hover:scale-105" style={{
                  background: `hsl(${item.color} / 0.1)`,
                  boxShadow: `inset 0 0 0 1px hsl(${item.color} / 0.1)`,
                }}>
                  <item.icon className="h-4 w-4" style={{ color: `hsl(${item.color})` }} />
                </div>
                <div>
                  <div className="text-[13px] font-semibold" style={{ color: 'hsl(224 14% 88%)' }}>{item.title}</div>
                  <div className="text-xs leading-relaxed mt-1" style={{ color: 'hsl(224 14% 48%)' }}>{item.desc}</div>
                </div>
              </div>
            ))}
          </div>
        </div>
      </div>

      {/* Right: Login form */}
      <div className="flex-1 flex items-center justify-center p-6 ambient-bg">
        <div className="w-full max-w-md space-y-6 relative z-10">
          <div className="lg:hidden text-center mb-8">
            <div className="flex items-center justify-center gap-2.5 mb-2">
              <div className="w-8 h-8 rounded-xl flex items-center justify-center text-xs font-black bg-primary text-primary-foreground shadow-glow-primary">R</div>
              <h1 className="text-xl font-bold tracking-tight">RustRAG</h1>
            </div>
            <p className="text-sm text-muted-foreground">{t('login.knowledgeSystemLogin')}</p>
          </div>

          {/* Locale selector */}
          <div className="flex justify-end">
            <Select value={locale} onValueChange={setLocale}>
              <SelectTrigger className="h-8 w-auto min-w-[120px] text-xs gap-1.5">
                <Globe className="h-3 w-3 text-muted-foreground shrink-0" />
                <SelectValue>{AVAILABLE_LOCALES.find(l => l.code === locale)?.nativeLabel ?? locale}</SelectValue>
              </SelectTrigger>
              <SelectContent align="end">
                {AVAILABLE_LOCALES.map(l => (
                  <SelectItem key={l.code} value={l.code}>
                    <span className="font-medium">{l.nativeLabel}</span>
                    <span className="text-muted-foreground ml-1.5">({l.label})</span>
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>

          {!isBootstrapRequired ? (
            <div className="space-y-5 animate-fade-in">
              <div>
                <h2 className="text-xl font-bold tracking-tight">{t('login.signIn')}</h2>
                <p className="text-sm text-muted-foreground mt-1.5 leading-relaxed">{t('login.signInDesc')}</p>
              </div>
              {error && (
                <div className="flex items-center gap-2.5 p-4 rounded-xl text-sm text-destructive" style={{
                  background: 'hsl(var(--status-failed-bg))',
                  boxShadow: 'inset 0 0 0 1px hsl(var(--status-failed-ring) / 0.3)',
                }}>
                  <AlertCircle className="h-4 w-4 shrink-0" /> {error}
                </div>
              )}
              <div className="space-y-4">
                <div>
                  <Label htmlFor="login" className="text-sm font-semibold">{t('login.loginField')}</Label>
                  <Input id="login" value={loginVal} onChange={e => setLoginVal(e.target.value)} placeholder="admin" autoFocus className="mt-2" />
                </div>
                <div>
                  <Label htmlFor="password" className="text-sm font-semibold">{t('login.password')}</Label>
                  <Input id="password" type="password" value={password} onChange={e => setPassword(e.target.value)} placeholder="••••••••" onKeyDown={e => e.key === 'Enter' && handleLogin()} className="mt-2" />
                </div>
              </div>
              <Button className="w-full h-11" onClick={handleLogin} disabled={submitting}>
                {submitting && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
                {t('login.signInBtn')}
              </Button>
            </div>
          ) : (
            <div className="space-y-5 animate-fade-in">
              <div>
                <h2 className="text-xl font-bold tracking-tight">{t('login.initialSetup')}</h2>
                <p className="text-sm text-muted-foreground mt-1.5 leading-relaxed">{t('login.initialSetupDesc')}</p>
              </div>
              {bootstrapError && (
                <div className="flex items-center gap-2.5 p-4 rounded-xl text-sm text-destructive" style={{
                  background: 'hsl(var(--status-failed-bg))',
                  boxShadow: 'inset 0 0 0 1px hsl(var(--status-failed-ring) / 0.3)',
                }}>
                  <AlertCircle className="h-4 w-4 shrink-0" /> {bootstrapError}
                </div>
              )}

              {/* Admin credentials section */}
              <div className="space-y-4 p-5 rounded-xl border bg-card shadow-soft">
                <div className="section-label">{t('login.adminAccount')}</div>
                <div className="space-y-3">
                  <div>
                    <Label htmlFor="admin-login" className="text-sm font-semibold">{t('login.adminLogin')}</Label>
                    <Input id="admin-login" value={loginVal} onChange={e => setLoginVal(e.target.value)} placeholder="admin" className="mt-2" />
                  </div>
                  <div>
                    <Label htmlFor="admin-name" className="text-sm font-semibold">{t('login.displayName')}</Label>
                    <Input id="admin-name" value={displayName} onChange={e => setDisplayName(e.target.value)} placeholder="Admin User" className="mt-2" />
                  </div>
                  <div>
                    <Label htmlFor="admin-password" className="text-sm font-semibold">{t('login.password')}</Label>
                    <Input id="admin-password" type="password" value={password} onChange={e => setPassword(e.target.value)} placeholder="••••••••" className="mt-2" />
                  </div>
                </div>
              </div>

              {/* AI Bindings section */}
              <div className="space-y-3">
                <div className="section-label px-1 flex items-center gap-2">
                  <Sparkles className="h-3 w-3" /> {t('login.aiConfig')}
                </div>
                {AI_PURPOSES.map(({ purpose, label, description }) => (
                  <div key={purpose} className="p-4 border rounded-xl space-y-3 bg-card shadow-soft transition-all duration-200 hover:shadow-lifted">
                    <div className="flex items-center justify-between">
                      <div>
                        <div className="text-sm font-semibold">{label}</div>
                        <div className="text-xs text-muted-foreground mt-0.5">{description}</div>
                      </div>
                      {bindings[purpose].provider && bindings[purpose].model && (
                        <div className="w-6 h-6 rounded-full flex items-center justify-center" style={{
                          background: 'hsl(var(--status-ready-bg))',
                          boxShadow: 'inset 0 0 0 1px hsl(var(--status-ready-ring) / 0.5)',
                        }}>
                          <CheckCircle2 className="h-3.5 w-3.5 text-status-ready" />
                        </div>
                      )}
                    </div>
                    <div className="grid grid-cols-2 gap-2">
                      <Select value={bindings[purpose].provider} onValueChange={v => { updateBinding(purpose, 'provider', v); updateBinding(purpose, 'model', ''); }}>
                        <SelectTrigger className="h-9 text-xs">
                          <SelectValue placeholder="Provider" />
                        </SelectTrigger>
                        <SelectContent>
                          {aiProviders.map(p => <SelectItem key={p.providerKind} value={p.providerKind}>{p.displayName}</SelectItem>)}
                        </SelectContent>
                      </Select>
                      <Select value={bindings[purpose].model} onValueChange={v => updateBinding(purpose, 'model', v)} disabled={!bindings[purpose].provider}>
                        <SelectTrigger className="h-9 text-xs">
                          <SelectValue placeholder="Model" />
                        </SelectTrigger>
                        <SelectContent>
                          {aiModels
                            .filter(m => {
                              const prov = aiProviders.find(p => p.providerKind === bindings[purpose].provider);
                              return prov && m.providerCatalogId === prov.id && m.allowedBindingPurposes.includes(purpose);
                            })
                            .map(m => <SelectItem key={m.id} value={m.modelName}>{m.modelName}</SelectItem>)}
                        </SelectContent>
                      </Select>
                    </div>
                    {bindings[purpose].provider && (
                      <Input
                        className="h-9 text-xs"
                        type="password"
                        placeholder={t('login.apiKey')}
                        value={bindings[purpose].apiKey}
                        onChange={e => updateBinding(purpose, 'apiKey', e.target.value)}
                      />
                    )}
                  </div>
                ))}
              </div>

              <Button className="w-full h-11" onClick={handleBootstrap} disabled={submitting}>
                {submitting && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
                {t('login.completeSetup')}
              </Button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
