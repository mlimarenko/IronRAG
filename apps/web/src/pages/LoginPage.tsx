import { useState, useEffect } from 'react';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { useApp } from '@/contexts/AppContext';
import { authApi } from '@/api';
import type { BootstrapProviderPresetBundle } from '@/api/auth';
import { baseUrlForProviderInput, buildBootstrapAiSetup } from '@/lib/ai-provider';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Loader2, FileText, Share2, Brain, Database, AlertCircle, CheckCircle2, Sparkles, Globe } from 'lucide-react';
import type { AIPurpose } from '@/types';
import { AVAILABLE_LOCALES } from '@/types';

const AI_PURPOSE_ORDER: AIPurpose[] = [
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_answer',
  'vision',
];

export default function LoginPage() {
  const { t } = useTranslation();
  const { login, bootstrapSetup, isBootstrapRequired, locale, setLocale } = useApp();
  const navigate = useNavigate();
  const aiPurposes: { purpose: AIPurpose; label: string; description: string }[] = [
    {
      purpose: 'extract_graph',
      label: t('login.purposeExtractGraph'),
      description: t('login.purposeExtractGraphDesc'),
    },
    {
      purpose: 'embed_chunk',
      label: t('login.purposeEmbedChunk'),
      description: t('login.purposeEmbedChunkDesc'),
    },
    {
      purpose: 'query_compile',
      label: t('login.purposeQueryCompile'),
      description: t('login.purposeQueryCompileDesc'),
    },
    {
      purpose: 'query_answer',
      label: t('login.purposeQueryAnswer'),
      description: t('login.purposeQueryAnswerDesc'),
    },
    {
      purpose: 'vision',
      label: t('login.purposeVision'),
      description: t('login.purposeVisionDesc'),
    },
  ];

  const [loginVal, setLoginVal] = useState('');
  const [password, setPassword] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');

  const [displayName, setDisplayName] = useState('');
  const [bootstrapError, setBootstrapError] = useState('');
  const [presetBundles, setPresetBundles] = useState<BootstrapProviderPresetBundle[]>([]);
  const [selectedProviderKind, setSelectedProviderKind] = useState('');
  const [bootstrapBaseUrl, setBootstrapBaseUrl] = useState('');
  const [bootstrapApiKey, setBootstrapApiKey] = useState('');

  useEffect(() => {
    if (isBootstrapRequired) {
      authApi.getBootstrapStatus().then(status => {
        const bundles = status.aiSetup?.presetBundles ?? [];
        setPresetBundles(bundles);
        setSelectedProviderKind(current => {
          if (current && bundles.some(bundle => bundle.providerKind === current)) {
            return current;
          }
          return bundles.find(bundle => bundle.providerKind === 'openai')?.providerKind ?? bundles[0]?.providerKind ?? '';
        });
      }).catch((err) => {
        setBootstrapError(err instanceof Error ? err.message : t('login.bootstrapStatusFetchFailed'));
      });
    }
  }, [isBootstrapRequired, t]);

  const selectedBundle =
    presetBundles.find(bundle => bundle.providerKind === selectedProviderKind) ?? null;

  useEffect(() => {
    setBootstrapBaseUrl('');
  }, [selectedBundle?.defaultBaseUrl, selectedBundle?.providerKind]);

  useEffect(() => {
    setBootstrapApiKey('');
  }, [selectedBundle?.providerKind]);

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
    if (!loginVal.trim() || !password.trim()) {
      setBootstrapError(t('login.fillRequired'));
      return;
    }
    const requiresApiKey =
      selectedBundle !== null
      && selectedBundle.credentialSource !== 'env'
      && selectedBundle.apiKeyRequired;
    const requiresBaseUrl =
      selectedBundle !== null
      && selectedBundle.credentialSource !== 'env'
      && selectedBundle.baseUrlRequired;
    if (selectedBundle && requiresApiKey && !bootstrapApiKey.trim()) {
      setBootstrapError(t('login.fillRequired'));
      return;
    }
    if (selectedBundle && requiresBaseUrl && !bootstrapBaseUrl.trim()) {
      setBootstrapError(t('login.fillRequired'));
      return;
    }

    setSubmitting(true);
    setBootstrapError('');
    try {
      const aiSetup = buildBootstrapAiSetup(
        selectedBundle,
        bootstrapApiKey,
        bootstrapBaseUrl,
      );
      await bootstrapSetup({
        login: loginVal,
        password,
        displayName: displayName.trim() || undefined,
        aiSetup,
      });
      navigate('/dashboard');
    } catch (err) {
      setBootstrapError(err instanceof Error ? err.message : t('login.setupFailed'));
    } finally {
      setSubmitting(false);
    }
  };
  const ollamaModels =
    selectedBundle?.providerKind === 'ollama'
      ? Array.from(new Set(selectedBundle.presets.map(preset => preset.modelName)))
      : [];

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
              <img
                src="/favicon.svg"
                alt=""
                aria-hidden="true"
                className="h-9 w-auto shrink-0"
              />
              <h1 className="text-2xl font-bold tracking-tight" style={{ color: 'hsl(var(--shell-foreground))' }}>IronRAG</h1>
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
              <img
                src="/favicon.svg"
                alt=""
                aria-hidden="true"
                className="h-8 w-auto shrink-0"
              />
              <h1 className="text-xl font-bold tracking-tight">IronRAG</h1>
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
                  <Input id="login" value={loginVal} onChange={e => setLoginVal(e.target.value)} placeholder={t('login.loginPlaceholder')} autoFocus className="mt-2" />
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
                    <Label htmlFor="admin-login" className="text-sm font-semibold">
                      {t('login.adminLogin')} <span className="text-destructive">*</span>
                    </Label>
                    <Input id="admin-login" value={loginVal} onChange={e => setLoginVal(e.target.value)} placeholder={t('login.loginPlaceholder')} className="mt-2" required />
                  </div>
                  <div>
                    <Label htmlFor="admin-name" className="text-sm font-semibold">
                      {t('login.displayName')} <span className="text-muted-foreground font-normal">({t('login.optional')})</span>
                    </Label>
                    <Input id="admin-name" value={displayName} onChange={e => setDisplayName(e.target.value)} placeholder={t('login.adminNamePlaceholder')} className="mt-2" />
                  </div>
                  <div>
                    <Label htmlFor="admin-password" className="text-sm font-semibold">
                      {t('login.password')} <span className="text-destructive">*</span>
                    </Label>
                    <Input id="admin-password" type="password" value={password} onChange={e => setPassword(e.target.value)} placeholder="••••••••" className="mt-2" required />
                  </div>
                </div>
              </div>

              {/* AI bootstrap section */}
              <div className="space-y-3">
                <div className="section-label px-1 flex items-center gap-2">
                  <Sparkles className="h-3 w-3" /> {t('login.aiConfig')}
                </div>
                <div className="p-4 border rounded-xl space-y-4 bg-card shadow-soft">
                  <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)]">
                    <div>
                      <Label className="text-sm font-semibold">{t('admin.provider')}</Label>
                      <Select value={selectedProviderKind} onValueChange={setSelectedProviderKind} disabled={presetBundles.length === 0}>
                        <SelectTrigger className="mt-2 h-10 text-sm">
                          <SelectValue placeholder={t('admin.selectProvider')} />
                        </SelectTrigger>
                        <SelectContent>
                          {presetBundles.map(bundle => (
                            <SelectItem key={bundle.providerKind} value={bundle.providerKind}>
                              {bundle.displayName}
                            </SelectItem>
                          ))}
                        </SelectContent>
                      </Select>
                    </div>
                    <div>
                      <Label htmlFor="bootstrap-api-key" className="text-sm font-semibold">
                        {selectedBundle?.apiKeyRequired ? t('login.apiKey') : t('login.providerTokenOptional')}
                      </Label>
                      <Input
                        id="bootstrap-api-key"
                        className="mt-2 h-10 text-sm"
                        type="password"
                        placeholder={selectedBundle?.apiKeyRequired ? t('login.apiKey') : t('login.providerTokenOptional')}
                        value={bootstrapApiKey}
                        onChange={e => setBootstrapApiKey(e.target.value)}
                        disabled={!selectedBundle || selectedBundle.credentialSource === 'env'}
                      />
                    </div>
                  </div>
                  {selectedBundle?.baseUrlRequired && (
                    <div>
                      <Label htmlFor="bootstrap-base-url" className="text-sm font-semibold">{t('login.providerAddress')}</Label>
                      <Input
                        id="bootstrap-base-url"
                        className="mt-2 h-10 text-sm font-mono"
                        type="text"
                        placeholder={baseUrlForProviderInput(selectedBundle.providerKind, selectedBundle.defaultBaseUrl)}
                        value={bootstrapBaseUrl}
                        onChange={e => setBootstrapBaseUrl(e.target.value)}
                        disabled={selectedBundle.credentialSource === 'env'}
                      />
                      {selectedBundle.providerKind === 'ollama' && (
                        <div className="mt-1.5 text-xs text-muted-foreground">
                          {t('login.ollamaAddressHint')}
                        </div>
                      )}
                    </div>
                  )}
                  {selectedBundle && (
                    <div className="rounded-xl border border-border/60 bg-surface-sunken p-4 space-y-3">
                      <div className="flex items-center justify-between gap-3">
                        <div>
                          <div className="text-sm font-semibold">{selectedBundle.displayName}</div>
                          <div className="text-xs text-muted-foreground mt-0.5">
                            {selectedBundle.credentialSource === 'env'
                              ? t('login.bundleConfiguredInEnv')
                              : t('login.bundleReadyPreview')}
                          </div>
                        </div>
                        <div className="w-6 h-6 rounded-full flex items-center justify-center" style={{
                          background: 'hsl(var(--status-ready-bg))',
                          boxShadow: 'inset 0 0 0 1px hsl(var(--status-ready-ring) / 0.5)',
                        }}>
                          <CheckCircle2 className="h-3.5 w-3.5 text-status-ready" />
                        </div>
                      </div>
                      <div className="space-y-2">
                        {selectedBundle.presets.map(preset => {
                          const purposeMeta = aiPurposes.find(entry => entry.purpose === preset.bindingPurpose as AIPurpose);
                          return (
                            <div key={preset.bindingPurpose} className="rounded-lg border border-border/50 bg-background/70 px-3 py-2">
                              <div className="flex flex-wrap items-center justify-between gap-2">
                                <div>
                                  <div className="text-sm font-medium">{purposeMeta?.label ?? preset.bindingPurpose}</div>
                                  <div className="text-xs text-muted-foreground">{purposeMeta?.description ?? preset.bindingPurpose}</div>
                                </div>
                                <div className="text-xs font-mono text-foreground">{preset.modelName}</div>
                              </div>
                              <div className="text-xs text-muted-foreground mt-1.5">
                                {preset.presetName}
                                {preset.temperature !== null && preset.temperature !== undefined ? ` · temp=${preset.temperature}` : ''}
                                {preset.topP !== null && preset.topP !== undefined ? ` · topP=${preset.topP}` : ''}
                              </div>
                            </div>
                          );
                        })}
                      </div>
                      {ollamaModels.length > 0 && (
                        <div className="rounded-lg border border-border/50 bg-background/70 px-3 py-2 text-xs text-muted-foreground space-y-1">
                          <div className="font-medium">{t('login.ollamaModelsHint')}</div>
                          {ollamaModels.map(model => (
                            <div key={model} className="font-mono">{`ollama pull ${model}`}</div>
                          ))}
                        </div>
                      )}
                    </div>
                  )}
                  {!selectedBundle && (
                    <div className="rounded-xl border border-dashed border-border/70 bg-surface-sunken p-4 text-sm text-muted-foreground">
                      {t('login.noBootstrapBundles')}
                    </div>
                  )}
                </div>
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
