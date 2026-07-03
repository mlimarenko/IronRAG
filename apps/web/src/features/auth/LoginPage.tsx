import { useEffect, useMemo, useRef, useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { useTranslation } from 'react-i18next';
import { useNavigate } from 'react-router-dom';
import { z } from 'zod';
import { useApp } from '@/shared/contexts/app-context';
import { queries } from '@/shared/api';
import type { BootstrapProviderBindingBundle } from '@/shared/api/auth';
import {
  buildBootstrapAiSetup,
  canEditProviderBaseUrl,
  normalizeProviderBaseUrl,
  resolveProviderCredentialPolicy,
} from '@/shared/lib/ai-provider';
import { Button } from '@/shared/components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/shared/components/ui/select';
import { Loader2, FileText, Share2, Brain, Database, AlertCircle, Sparkles, Globe } from 'lucide-react';
import { ProviderCredentialFields } from '@/shared/components/ai-provider/ProviderCredentialFields';
import { ProviderSetupSummary } from '@/shared/components/ai-provider/ProviderSetupSummary';
import type { AiBindingPurpose, BootstrapBindingPurpose } from '@/shared/api/generated';
import { AVAILABLE_LOCALES } from '@/shared/types';
import {
  fieldErrorMessage,
  FormInputField,
  FormSelectField,
  nonEmptyString,
  useTypedForm,
} from '@/shared/forms';

type BootstrapPurposeMetadata = {
  labelKey: `login.${string}`;
  descriptionKey: `login.${string}`;
};

type BootstrapPurposeCoverage = Record<BootstrapBindingPurpose, BootstrapPurposeMetadata> &
  Record<AiBindingPurpose, BootstrapPurposeMetadata>;

const bootstrapPurposeMetadata = {
  extract_text: {
    labelKey: 'login.purposeExtractText',
    descriptionKey: 'login.purposeExtractTextDesc',
  },
  extract_graph: {
    labelKey: 'login.purposeExtractGraph',
    descriptionKey: 'login.purposeExtractGraphDesc',
  },
  embed_chunk: {
    labelKey: 'login.purposeEmbedChunk',
    descriptionKey: 'login.purposeEmbedChunkDesc',
  },
  query_compile: {
    labelKey: 'login.purposeQueryCompile',
    descriptionKey: 'login.purposeQueryCompileDesc',
  },
  query_retrieve: {
    labelKey: 'login.purposeQueryRetrieve',
    descriptionKey: 'login.purposeQueryRetrieveDesc',
  },
  query_answer: {
    labelKey: 'login.purposeQueryAnswer',
    descriptionKey: 'login.purposeQueryAnswerDesc',
  },
  vision: {
    labelKey: 'login.purposeVision',
    descriptionKey: 'login.purposeVisionDesc',
  },
  agent: {
    labelKey: 'login.purposeAgent',
    descriptionKey: 'login.purposeAgentDesc',
  },
} satisfies BootstrapPurposeCoverage;

function providerBundleKey(bundle: BootstrapProviderBindingBundle) {
  return bundle.providerCatalogId;
}

export default function LoginPage() {
  const { t } = useTranslation();
  const { login, bootstrapSetup, isBootstrapRequired, locale, setLocale } = useApp();
  const navigate = useNavigate();

  const [error, setError] = useState('');
  const [bootstrapFormError, setBootstrapFormError] = useState('');

  const bootstrapQuery = useQuery({
    ...queries.getBootstrapStatusOptions(),
    enabled: isBootstrapRequired,
  });

  const bootstrapStatusError = bootstrapQuery.error
    ? t('login.bootstrapStatusFetchFailed')
    : '';
  const bindingBundles = useMemo<BootstrapProviderBindingBundle[]>(() => {
    if (!bootstrapQuery.data) return [];
    return bootstrapQuery.data.aiSetup?.bindingBundles ?? [];
  }, [bootstrapQuery.data]);
  const defaultProviderKey = bindingBundles[0] ? providerBundleKey(bindingBundles[0]) : '';
  const loginSchema = useMemo(
    () =>
      z.object({
        login: nonEmptyString(t('login.fillAllFields')),
        password: nonEmptyString(t('login.fillAllFields')),
      }),
    [t],
  );
  const bootstrapSchema = useMemo(
    () =>
      z.object({
        login: nonEmptyString(t('login.fillRequired')),
        password: nonEmptyString(t('login.fillRequired')),
        displayName: z.string(),
        providerKey: nonEmptyString(t('login.bootstrapBundleRequired')),
        apiKey: z.string(),
        baseUrl: z.string(),
      }).superRefine((values, context) => {
        const providerKey = values.providerKey || defaultProviderKey;
        const bundle =
          bindingBundles.find(entry => providerBundleKey(entry) === providerKey) ?? null;
        if (!bundle) {
          context.addIssue({
            code: 'custom',
            message: t('login.bootstrapBundleRequired'),
            path: ['providerKey'],
          });
          return;
        }
        const policy = resolveProviderCredentialPolicy(bundle);
        if (bundle.credentialSource === 'env') {
          return;
        }
        if (policy.apiKeyRequired && !values.apiKey.trim()) {
          context.addIssue({
            code: 'custom',
            message: t('login.providerTokenRequiredHint'),
            path: ['apiKey'],
          });
        }
        if (policy.baseUrlRequired && canEditProviderBaseUrl(bundle) && !values.baseUrl.trim()) {
          context.addIssue({
            code: 'custom',
            message: t('login.providerAddressRequiredHint'),
            path: ['baseUrl'],
          });
        }
        if (policy.baseUrlRequired && !canEditProviderBaseUrl(bundle)) {
          const defaultBaseUrl = normalizeProviderBaseUrl(bundle, bundle.defaultBaseUrl);
          if (!defaultBaseUrl) {
            context.addIssue({
              code: 'custom',
              message: t('login.providerAddressRequiredHint'),
              path: ['baseUrl'],
            });
          }
        }
      }),
    [defaultProviderKey, bindingBundles, t],
  );
  const loginForm = useTypedForm({
    schema: loginSchema,
    defaultValues: { login: '', password: '' },
  });
  const bootstrapForm = useTypedForm({
    schema: bootstrapSchema,
    defaultValues: {
      login: '',
      password: '',
      displayName: '',
      providerKey: '',
      apiKey: '',
      baseUrl: '',
    },
    mode: 'onChange',
  });
  const bootstrapProviderKey = bootstrapForm.watch('providerKey');
  const bootstrapApiKey = bootstrapForm.watch('apiKey');
  const bootstrapBaseUrl = bootstrapForm.watch('baseUrl');
  const { getValues: getBootstrapValues, setValue: setBootstrapValue } = bootstrapForm;
  const lastBootstrapProviderKey = useRef('');

  useEffect(() => {
    if (!isBootstrapRequired || !defaultProviderKey || getBootstrapValues('providerKey')) {
      return;
    }
    setBootstrapValue('providerKey', defaultProviderKey, {
      shouldDirty: false,
      shouldValidate: true,
    });
  }, [defaultProviderKey, getBootstrapValues, isBootstrapRequired, setBootstrapValue]);

  const effectiveProviderKey =
    bootstrapProviderKey &&
    bindingBundles.some(bundle => providerBundleKey(bundle) === bootstrapProviderKey)
      ? bootstrapProviderKey
      : defaultProviderKey;

  useEffect(() => {
    if (!effectiveProviderKey || lastBootstrapProviderKey.current === effectiveProviderKey) {
      return;
    }
    lastBootstrapProviderKey.current = effectiveProviderKey;
    setBootstrapValue('apiKey', '', { shouldDirty: false, shouldValidate: true });
    setBootstrapValue('baseUrl', '', { shouldDirty: false, shouldValidate: true });
  }, [effectiveProviderKey, setBootstrapValue]);

  const selectedBundle =
    bindingBundles.find(bundle => providerBundleKey(bundle) === effectiveProviderKey) ?? null;
  const selectedBundleCredentialPolicy = selectedBundle
    ? resolveProviderCredentialPolicy(selectedBundle)
    : null;
  const selectedBundleBaseUrlEditable = canEditProviderBaseUrl(selectedBundle);
  const selectedBundleDefaultBaseUrl = selectedBundle
    ? normalizeProviderBaseUrl(selectedBundle, selectedBundle.defaultBaseUrl)
    : '';
  const selectedBundleRequiresApiKey = Boolean(
    selectedBundle
      && selectedBundle.credentialSource !== 'env'
      && selectedBundleCredentialPolicy?.apiKeyRequired,
  );
  const selectedBundleRequiresBaseUrl = Boolean(
    selectedBundle
      && selectedBundle.credentialSource !== 'env'
      && selectedBundleCredentialPolicy?.baseUrlRequired,
  );
  const selectedBundleApiKeyReady =
    !selectedBundleRequiresApiKey || Boolean(bootstrapApiKey.trim());
  const selectedBundleBaseUrlReady =
    !selectedBundleRequiresBaseUrl
    || (selectedBundleBaseUrlEditable
      ? Boolean(bootstrapBaseUrl.trim())
      : Boolean(selectedBundleDefaultBaseUrl));
  const selectedBundleReady = Boolean(
    selectedBundle
      && (
        selectedBundle.credentialSource === 'env'
        || (selectedBundleApiKeyReady && selectedBundleBaseUrlReady)
      ),
  );
  const bootstrapConfigLoading = bootstrapQuery.isLoading && !bootstrapQuery.data;
  const bootstrapSubmitDisabled =
    bootstrapForm.formState.isSubmitting
    || bootstrapConfigLoading
    || !bootstrapForm.formState.isValid;
  const selectedBundleSummaryRows = selectedBundle ? [
    {
      label: t('login.summaryCredential'),
      value: selectedBundle.credentialSource === 'env'
        ? t('login.summaryCredentialEnv')
        : selectedBundleRequiresApiKey
          ? (bootstrapApiKey.trim() ? t('login.summaryCredentialProvided') : t('login.summaryCredentialRequired'))
          : t('login.summaryCredentialOptional'),
    },
    {
      label: t('login.summaryEndpoint'),
      value: selectedBundleRequiresBaseUrl
        ? (selectedBundleBaseUrlEditable
          ? (bootstrapBaseUrl.trim() || t('login.summaryEndpointRequired'))
          : (selectedBundleDefaultBaseUrl || t('login.summaryEndpointRequired')))
        : (selectedBundleDefaultBaseUrl || t('login.summaryEndpointHosted')),
    },
    {
      label: t('login.summaryDiscovery'),
      value: selectedBundle.modelDiscovery?.mode ?? t('login.summaryUnknown'),
    },
    {
      label: t('login.summaryBindings'),
      value: String(selectedBundle.bindings.length),
    },
  ] : [];
  const bootstrapError = bootstrapFormError || bootstrapStatusError;
  const bootstrapApiKeyError = fieldErrorMessage(bootstrapForm.formState.errors, 'apiKey');
  const bootstrapBaseUrlError = fieldErrorMessage(bootstrapForm.formState.errors, 'baseUrl');

  const handleLogin = loginForm.submitWithMutation(
    {
      mutateAsync: async (values) => {
        setError('');
        await login(values.login, values.password);
        void navigate('/dashboard');
      },
    },
    {
      errorMessage: t('login.loginFailed'),
      onError: () => setError(t('login.loginFailed')),
    },
  );

  const handleBootstrap = bootstrapForm.submitWithMutation(
    {
      mutateAsync: async (values) => {
        if (bootstrapConfigLoading) {
          throw new Error(t('login.bootstrapStatusLoading'));
        }
        const bundle =
          bindingBundles.find(entry => providerBundleKey(entry) === values.providerKey)
          ?? bindingBundles.find(entry => providerBundleKey(entry) === defaultProviderKey)
          ?? null;
        if (!bundle) {
          throw new Error(t('login.bootstrapBundleRequired'));
        }
        setBootstrapFormError('');
        const aiSetup = buildBootstrapAiSetup(
          bundle,
          values.apiKey,
          values.baseUrl,
        );
        await bootstrapSetup({
          login: values.login,
          password: values.password,
          displayName: values.displayName.trim(),
          ...(aiSetup ? { aiSetup } : {}),
        });
        void navigate('/dashboard');
      },
    },
    {
      errorMessage: t('login.setupFailed'),
      onError: () => setBootstrapFormError(t('login.setupFailed')),
    },
  );
  return (
    <div className="min-h-screen flex bg-background">
      {/* Left: Product explainer — same dark shell chrome as the app sidebar */}
      <div
        className="hidden lg:flex lg:w-[460px] xl:w-[520px] flex-col justify-center p-12 relative overflow-hidden ambient-bg"
        style={{ background: 'hsl(var(--shell-bg))' }}
      >
        <div className="space-y-10 relative z-10">
          <div>
            <div className="flex items-center gap-3 mb-5">
              <img
                src="/favicon.svg"
                alt=""
                aria-hidden="true"
                className="h-9 w-auto shrink-0"
              />
              <h1 className="text-2xl font-bold tracking-tight text-shell-foreground">IronRAG</h1>
            </div>
            <p className="text-sm leading-relaxed max-w-[320px] text-shell-muted">
              {t('login.tagline')}
            </p>
          </div>
          <div className="space-y-5">
            {[
              { icon: FileText, title: t('login.featureDocs'), desc: t('login.featureDocsDesc') },
              { icon: Database, title: t('login.featureEntities'), desc: t('login.featureEntitiesDesc') },
              { icon: Share2, title: t('login.featureGraph'), desc: t('login.featureGraphDesc') },
              { icon: Brain, title: t('login.featureAi'), desc: t('login.featureAiDesc') },
            ].map(item => (
              <div key={item.title} className="flex gap-4 group">
                <div className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-shell-hover text-shell-active ring-1 ring-shell-border transition-transform duration-200 group-hover:scale-105">
                  <item.icon className="h-4 w-4" />
                </div>
                <div>
                  <div className="text-[13px] font-semibold text-shell-foreground">{item.title}</div>
                  <div className="text-xs leading-relaxed mt-1 text-shell-muted">{item.desc}</div>
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
                <div role="alert" aria-live="polite" className="inline-error flex items-center gap-2.5 text-destructive">
                  <AlertCircle className="h-4 w-4 shrink-0" /> {error}
                </div>
              )}
              <div className="space-y-4">
                <FormInputField
                  formState={loginForm.formState}
                  id="login"
                  label={t('login.loginField')}
                  name="login"
                  registration={loginForm.register('login')}
                  placeholder={t('login.loginPlaceholder')}
                  autoFocus
                />
                <FormInputField
                  formState={loginForm.formState}
                  id="password"
                  label={t('login.password')}
                  name="password"
                  registration={loginForm.register('password')}
                  type="password"
                  placeholder="••••••••"
                  onKeyDown={event => {
                    if (event.key === 'Enter') void handleLogin();
                  }}
                />
              </div>
              <Button className="w-full h-11" onClick={handleLogin} disabled={loginForm.formState.isSubmitting}>
                {loginForm.formState.isSubmitting && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
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
                <div role="alert" aria-live="polite" className="inline-error flex items-center gap-2.5 text-destructive">
                  <AlertCircle className="h-4 w-4 shrink-0" /> {bootstrapError}
                </div>
              )}

              {/* Admin credentials section */}
              <div className="space-y-4 p-5 rounded-xl border bg-card shadow-soft">
                <div className="section-label">{t('login.adminAccount')}</div>
                <div className="space-y-3">
                  <FormInputField
                    formState={bootstrapForm.formState}
                    id="admin-login"
                    label={<>{t('login.adminLogin')} <span className="text-destructive">*</span></>}
                    name="login"
                    registration={bootstrapForm.register('login')}
                    placeholder={t('login.loginPlaceholder')}
                  />
                  <FormInputField
                    formState={bootstrapForm.formState}
                    id="admin-name"
                    label={<>{t('login.displayName')} <span className="text-muted-foreground font-normal">({t('login.optional')})</span></>}
                    name="displayName"
                    registration={bootstrapForm.register('displayName')}
                    placeholder={t('login.adminNamePlaceholder')}
                  />
                  <FormInputField
                    formState={bootstrapForm.formState}
                    id="admin-password"
                    label={<>{t('login.password')} <span className="text-destructive">*</span></>}
                    name="password"
                    registration={bootstrapForm.register('password')}
                    type="password"
                    placeholder="••••••••"
                  />
                </div>
              </div>

              {/* AI bootstrap section */}
              <div className="space-y-3">
                <div className="section-label px-1 flex items-center gap-2">
                  <Sparkles className="h-3 w-3" /> {t('login.aiConfig')}
                </div>
                <div className="p-4 border rounded-xl space-y-4 bg-card shadow-soft">
                  <FormSelectField
                    control={bootstrapForm.control}
                    disabled={bindingBundles.length === 0}
                    formState={bootstrapForm.formState}
                    id="bootstrap-provider"
                    label={t('admin.provider')}
                    name="providerKey"
                    placeholder={t('admin.selectProvider')}
                    triggerClassName="h-10 text-sm"
                  >
                    {bindingBundles.map(bundle => (
                      <SelectItem key={providerBundleKey(bundle)} value={providerBundleKey(bundle)}>
                        {bundle.displayName}
                      </SelectItem>
                    ))}
                  </FormSelectField>
                  <ProviderCredentialFields
                    provider={selectedBundle}
                    idPrefix="bootstrap-provider"
                    apiKey={bootstrapApiKey}
                    baseUrl={bootstrapBaseUrl}
                    labels={{
                      apiKeyRequired: t('login.apiKey'),
                      apiKeyOptional: t('login.providerTokenOptional'),
                      apiKeyPlaceholder: t('login.apiKey'),
                      apiKeyRequiredHint: t('login.providerTokenRequiredHint'),
                      baseUrlRequired: t('login.providerAddress'),
                      baseUrlOptional: t('login.providerAddressOptional'),
                      baseUrlRequiredHint: t('login.providerAddressRequiredHint'),
                      fixedBaseUrlHint: t('login.providerAddressFixedHint'),
                    }}
                    {...(bootstrapApiKeyError !== undefined ? { apiKeyError: bootstrapApiKeyError } : {})}
                    {...(bootstrapBaseUrlError !== undefined ? { baseUrlError: bootstrapBaseUrlError } : {})}
                    onApiKeyChange={value => setBootstrapValue('apiKey', value, { shouldDirty: true, shouldValidate: true })}
                    onBaseUrlChange={value => setBootstrapValue('baseUrl', value, { shouldDirty: true, shouldValidate: true })}
                  />
                  {selectedBundle && (
                    <div className="space-y-3">
                      <ProviderSetupSummary
                        title={selectedBundle.displayName}
                        description={selectedBundle.credentialSource === 'env'
                          ? t('login.bundleConfiguredInEnv')
                          : t('login.bundleReadyPreview')}
                        ready={selectedBundleReady}
                        readyLabel={t('login.summaryReady')}
                        attentionLabel={t('login.summaryNeedsInput')}
                        rows={selectedBundleSummaryRows}
                      />
                      <div className="space-y-2">
                        {selectedBundle.bindings.map(binding => {
                          const purposeMeta = bootstrapPurposeMetadata[binding.bindingPurpose];
                          return (
                            <div key={binding.bindingPurpose} className="rounded-lg border border-border/50 bg-background/70 px-3 py-2">
                              <div className="flex flex-wrap items-center justify-between gap-2">
                                <div className="min-w-0">
                                  <div className="text-sm font-medium [overflow-wrap:anywhere]">{t(purposeMeta.labelKey)}</div>
                                  <div className="text-xs text-muted-foreground [overflow-wrap:anywhere]">{t(purposeMeta.descriptionKey)}</div>
                                </div>
                                <div className="min-w-0 text-xs font-mono text-foreground [overflow-wrap:anywhere]">{binding.modelName}</div>
                              </div>
                              {(binding.temperature !== null && binding.temperature !== undefined) || (binding.topP !== null && binding.topP !== undefined) ? (
                                <div className="text-xs text-muted-foreground mt-1.5 [overflow-wrap:anywhere]">
                                  {binding.temperature !== null && binding.temperature !== undefined ? `temp=${binding.temperature}` : ''}
                                  {binding.topP !== null && binding.topP !== undefined ? ` · topP=${binding.topP}` : ''}
                                </div>
                              ) : null}
                            </div>
                          );
                        })}
                      </div>
                    </div>
                  )}
	                  {!selectedBundle && (
	                    <div className="rounded-xl border border-dashed border-border/70 bg-surface-sunken p-4 text-sm text-muted-foreground">
	                      {bootstrapConfigLoading
	                        ? t('login.bootstrapStatusLoading')
	                        : t('login.noBootstrapBundles')}
	                    </div>
	                  )}
	                </div>
	              </div>

	              <Button className="w-full h-11" onClick={handleBootstrap} disabled={bootstrapSubmitDisabled}>
	                {bootstrapForm.formState.isSubmitting && <Loader2 className="h-4 w-4 mr-2 animate-spin" />}
	                {t('login.completeSetup')}
	              </Button>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
