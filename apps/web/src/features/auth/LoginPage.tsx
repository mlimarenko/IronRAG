import { useMutation, useQuery } from '@tanstack/react-query'
import {
  AlertCircle,
  ArrowRight,
  Check,
  KeyRound,
  Languages,
  LoaderCircle,
  LockKeyhole,
  Server,
} from 'lucide-react'
import { useEffect, useMemo, useState, type FormEvent } from 'react'
import { useTranslation } from 'react-i18next'
import { useLocation, useNavigate, type Location } from 'react-router-dom'
import { z } from 'zod'

import { usePreferences } from '@/app/preferences-context'
import { Brand } from '@/app/Brand'
import { DEFAULT_SIGNED_IN_ROUTE, returnToFromLocation } from '@/app/returnTo'
import { authApi, getApiProblem, queries } from '@/shared/api'
import type { BootstrapProviderBindingBundle } from '@/shared/api/auth'
import type { AiBindingPurpose } from '@/shared/api/generated'
import { useApp } from '@/shared/contexts/app-context'
import { buildBootstrapAiSetup } from '@/shared/lib/ai-provider'
import { screenPhase } from '@/shared/lib/screenPhase'
import { Badge, Button } from '@/shared/ui'
import { loginNoticeKind, ssoErrorFromSearch } from './model/loginNotice'

type LoginValues = {
  login: string
  password: string
}

type BootstrapValues = {
  login: string
  email: string
  displayName: string
  password: string
  providerKey: string
  apiKey: string
  baseUrl: string
}

type BootstrapField = keyof BootstrapValues
type BootstrapErrors = Partial<Record<BootstrapField, string>>

const INITIAL_LOGIN: LoginValues = { login: '', password: '' }
const INITIAL_BOOTSTRAP: BootstrapValues = {
  login: '',
  email: '',
  displayName: '',
  password: '',
  providerKey: '',
  apiKey: '',
  baseUrl: '',
}

const PURPOSE_TRANSLATIONS = {
  extract_text: ['login.purposeExtractText', 'login.purposeExtractTextDesc'],
  extract_graph: ['login.purposeExtractGraph', 'login.purposeExtractGraphDesc'],
  graph_entity_type: ['login.purposeGraphEntityType', 'login.purposeGraphEntityTypeDesc'],
  embed_chunk: ['login.purposeEmbedChunk', 'login.purposeEmbedChunkDesc'],
  query_compile: ['login.purposeQueryCompile', 'login.purposeQueryCompileDesc'],
  query_rerank: ['login.purposeQueryRerank', 'login.purposeQueryRerankDesc'],
  query_answer: ['login.purposeQueryAnswer', 'login.purposeQueryAnswerDesc'],
  agent: ['login.purposeAgent', 'login.purposeAgentDesc'],
} as const satisfies Record<AiBindingPurpose, readonly [`login.${string}`, `login.${string}`]>

function bootstrapSchema(
  bundle: BootstrapProviderBindingBundle | null,
  translate: (key: `login.${string}`) => string,
) {
  return z
    .object({
      login: z
        .string()
        .trim()
        .min(1, translate('login.fillRequired'))
        .regex(/^[a-z0-9._-]+$/u, translate('login.loginPattern')),
      email: z.string().trim().email(translate('login.emailInvalid')),
      displayName: z.string().trim(),
      password: z.string().min(8, translate('login.passwordMin')),
      providerKey: z.string().trim().min(1, translate('login.bootstrapBundleRequired')),
      apiKey: z.string(),
      baseUrl: z.string(),
    })
    .superRefine((values, context) => {
      if (!bundle || values.providerKey !== bundle.providerCatalogId) {
        context.addIssue({
          code: 'custom',
          message: translate('login.bootstrapBundleRequired'),
          path: ['providerKey'],
        })
        return
      }
      if (bundle.credentialSource === 'env') return

      if (bundle.credentialPolicy.apiKeyRequired && !values.apiKey.trim()) {
        context.addIssue({
          code: 'custom',
          message: translate('login.providerTokenRequiredHint'),
          path: ['apiKey'],
        })
      }

      const baseUrl = values.baseUrl.trim()
      if (bundle.credentialPolicy.baseUrlMode === 'required' && !baseUrl) {
        context.addIssue({
          code: 'custom',
          message: translate('login.providerAddressRequiredHint'),
          path: ['baseUrl'],
        })
      }
      if (
        bundle.credentialPolicy.baseUrlMode === 'fixed' &&
        bundle.credentialPolicy.baseUrlRequired &&
        !bundle.defaultBaseUrl
      ) {
        context.addIssue({
          code: 'custom',
          message: translate('login.providerAddressRequiredHint'),
          path: ['baseUrl'],
        })
      }
      if (baseUrl && !z.url().safeParse(baseUrl).success) {
        context.addIssue({
          code: 'custom',
          message: translate('login.baseUrlInvalid'),
          path: ['baseUrl'],
        })
      }
    })
}

function errorMap(error: z.ZodError<BootstrapValues> | undefined): BootstrapErrors {
  if (!error) return {}
  const errors: BootstrapErrors = {}
  for (const issue of error.issues) {
    const field = issue.path[0]
    if (
      typeof field === 'string' &&
      field in INITIAL_BOOTSTRAP &&
      !errors[field as BootstrapField]
    ) {
      errors[field as BootstrapField] = issue.message
    }
  }
  return errors
}

function isBootstrapAccessFailure(error: unknown): 'forbidden' | 'claimed' | null {
  const problem = getApiProblem(error)
  if (problem?.status === 403) return 'forbidden'
  if (problem?.status === 409 && problem.code === 'bootstrap_already_claimed') return 'claimed'
  return null
}

function Field({
  id,
  label,
  error,
  hint,
  ...inputProps
}: Readonly<
  {
    id: string
    label: string
    error?: string | undefined
    hint?: string | undefined
  } & React.InputHTMLAttributes<HTMLInputElement>
>) {
  const descriptionId = error || hint ? `${id}-description` : undefined
  return (
    <label className="field" htmlFor={id}>
      <span>{label}</span>
      <input
        {...inputProps}
        aria-describedby={descriptionId}
        aria-invalid={Boolean(error)}
        className="input"
        id={id}
      />
      {(error || hint) && (
        <small className={error ? 'field-error' : 'field-hint'} id={descriptionId}>
          {error || hint}
        </small>
      )}
    </label>
  )
}

function LoginHeader() {
  const { t } = useTranslation()
  const { locale, setLocale } = usePreferences()
  const nextLocale = locale === 'en' ? 'ru' : 'en'

  return (
    <header className="login-header">
      <Brand />
      <Button
        aria-label={t('login.languageLabel')}
        onClick={() => setLocale(nextLocale)}
        size="sm"
        type="button"
        variant="ghost"
      >
        <Languages aria-hidden="true" size={16} />
        {locale === 'en' ? t('login.languageEnglish') : t('login.languageRussian')}
      </Button>
    </header>
  )
}

function oidcLoginHref(returnTo: string): string {
  return `/v1/iam/oidc/login?${new URLSearchParams({ returnTo }).toString()}`
}

function returnToForSso(location: Location): string {
  // When the app sent a visitor here from a protected page, or a rejected
  // single sign-on brought them back with the page they were heading for, that
  // is the most useful same-origin destination. A direct visit to /login falls
  // back to the canonical landing page instead of creating a /login loop.
  const candidate = returnToFromLocation(location)

  // A fragment never reaches the server and is not part of an HTTP redirect
  // target. Keep the path and query only, which is what the OIDC callback can
  // safely restore after the same-origin round trip.
  return candidate.split('#', 1)[0] || DEFAULT_SIGNED_IN_ROUTE
}

/**
 * The single sign-on verdict from the address, read once. The parameter is
 * then dropped from the address so a reload or a later password sign-in does
 * not keep repeating it, while `returnTo` stays for the retry.
 */
function useSsoErrorFromAddress(location: Location) {
  const navigate = useNavigate()
  const [ssoError] = useState(() => ssoErrorFromSearch(location.search))
  useEffect(() => {
    if (ssoErrorFromSearch(location.search) === null) return
    const remaining = new URLSearchParams(location.search)
    remaining.delete('ssoError')
    const search = remaining.toString()
    void navigate(
      { pathname: location.pathname, search: search ? `?${search}` : '' },
      { replace: true, state: location.state },
    )
  }, [location.pathname, location.search, location.state, navigate])
  return ssoError
}

function LoginForm({
  notice,
  ssoAvailable,
  ssoHref,
}: Readonly<{
  notice: string
  ssoAvailable: boolean
  ssoHref: string
}>) {
  const { t } = useTranslation()
  const { login } = useApp()
  const [values, setValues] = useState(INITIAL_LOGIN)
  const [formError, setFormError] = useState('')
  const loginMutation = useMutation({
    mutationFn: ({ login: loginValue, password }: LoginValues) => login(loginValue, password),
    onError: (error: unknown) => {
      setFormError(
        getApiProblem(error)?.status === 401 ? t('login.invalid') : t('login.loginFailed'),
      )
    },
  })

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    setFormError('')
    if (!values.login.trim() || !values.password) {
      setFormError(t('login.fillAllFields'))
      return
    }
    loginMutation.mutate({ ...values, login: values.login.trim() })
  }

  return (
    <form className="login-form" onSubmit={handleSubmit}>
      <LoginHeader />
      <h1>{t('login.signIn')}</h1>
      {(formError || notice) && (
        <div className="login-error" role="alert">
          <AlertCircle aria-hidden="true" size={15} />
          {formError || notice}
        </div>
      )}
      <Field
        autoComplete="username"
        autoFocus
        id="login"
        label={t('login.loginField')}
        name="login"
        onChange={(event) => setValues((current) => ({ ...current, login: event.target.value }))}
        placeholder={t('login.loginPlaceholder')}
        value={values.login}
      />
      <Field
        autoComplete="current-password"
        id="password"
        label={t('login.password')}
        name="password"
        onChange={(event) => setValues((current) => ({ ...current, password: event.target.value }))}
        placeholder={t('login.passwordPlaceholder')}
        type="password"
        value={values.password}
      />
      <Button disabled={loginMutation.isPending} type="submit" variant="primary">
        {loginMutation.isPending ? (
          <LoaderCircle aria-hidden="true" className="spin" size={15} />
        ) : (
          <ArrowRight aria-hidden="true" size={15} />
        )}
        {t('login.submit')}
      </Button>
      {ssoAvailable && (
        <>
          <div aria-hidden="true" className="login-auth-separator">
            <span>{t('login.ssoSeparator')}</span>
          </div>
          <a className="button button--secondary button--md login-sso-button" href={ssoHref}>
            <KeyRound aria-hidden="true" size={15} />
            {t('login.ssoSignIn')}
          </a>
        </>
      )}
    </form>
  )
}

// Whether the endpoint half of a bootstrap credential is satisfied. `required`
// means the operator must type one; a `fixed` endpoint the provider declares
// required must have been shipped with a default; anything else is optional.
function endpointReadiness(bundle: BootstrapProviderBindingBundle, typedBaseUrl: string): boolean {
  const policy = bundle.credentialPolicy
  if (policy.baseUrlMode === 'required') return Boolean(typedBaseUrl.trim())
  if (policy.baseUrlMode === 'fixed' && policy.baseUrlRequired) {
    return Boolean(bundle.defaultBaseUrl)
  }
  return true
}

function providerReadiness(
  bundle: BootstrapProviderBindingBundle,
  values: BootstrapValues,
): boolean {
  if (bundle.credentialSource === 'env') return true
  const keyReady = !bundle.credentialPolicy.apiKeyRequired || Boolean(values.apiKey.trim())
  return keyReady && endpointReadiness(bundle, values.baseUrl)
}

// Which sentence the summary shows about the credential itself.
function credentialSummaryKey(
  bundle: BootstrapProviderBindingBundle,
  typedApiKey: string,
): 'env' | 'provided' | 'required' | 'optional' {
  if (bundle.credentialSource === 'env') return 'env'
  if (typedApiKey.trim()) return 'provided'
  return bundle.credentialPolicy.apiKeyRequired ? 'required' : 'optional'
}

function ProviderSummary({
  bundle,
  values,
}: Readonly<{ bundle: BootstrapProviderBindingBundle; values: BootstrapValues }>) {
  const { t } = useTranslation()
  const ready = providerReadiness(bundle, values)
  const credential = {
    env: t('login.summaryCredentialEnv'),
    provided: t('login.summaryCredentialProvided'),
    required: t('login.summaryCredentialRequired'),
    optional: t('login.summaryCredentialOptional'),
  }[credentialSummaryKey(bundle, values.apiKey)]
  const endpointFallback =
    bundle.credentialPolicy.baseUrlMode === 'required'
      ? t('login.summaryEndpointRequired')
      : t('login.summaryEndpointHosted')
  const endpoint =
    bundle.credentialPolicy.baseUrlMode === 'fixed'
      ? (bundle.defaultBaseUrl ?? t('login.summaryEndpointHosted'))
      : values.baseUrl.trim() || endpointFallback
  const discoveryKey = {
    shared: 'login.summaryDiscoveryShared',
    credential: 'login.summaryDiscoveryCredential',
    unsupported: 'login.summaryDiscoveryUnsupported',
  } as const

  return (
    <div className="bootstrap-summary">
      <div className="bootstrap-summary__heading">
        <div>
          <strong>{bundle.displayName}</strong>
          <span>
            {bundle.credentialSource === 'env'
              ? t('login.bundleConfiguredInEnv')
              : t('login.bundleReadyPreview')}
          </span>
        </div>
        <Badge tone={ready ? 'ready' : 'warning'}>
          {ready ? t('login.summaryReady') : t('login.summaryNeedsInput')}
        </Badge>
      </div>
      <dl>
        <div>
          <dt>{t('login.summaryCredential')}</dt>
          <dd>{credential}</dd>
        </div>
        <div>
          <dt>{t('login.summaryEndpoint')}</dt>
          <dd>{endpoint}</dd>
        </div>
        <div>
          <dt>{t('login.summaryDiscovery')}</dt>
          <dd>{t(discoveryKey[bundle.modelDiscovery.mode])}</dd>
        </div>
        <div>
          <dt>{t('login.summaryBindings')}</dt>
          <dd>{bundle.bindings.length}</dd>
        </div>
      </dl>
    </div>
  )
}

function ProviderFields({
  bundle,
  errors,
  touched,
  values,
  update,
}: Readonly<{
  bundle: BootstrapProviderBindingBundle
  errors: BootstrapErrors
  touched: ReadonlySet<BootstrapField>
  values: BootstrapValues
  update: (field: BootstrapField, value: string) => void
}>) {
  const { t } = useTranslation()
  if (bundle.credentialSource === 'env') {
    return (
      <div className="bootstrap-preconfigured">
        <Check aria-hidden="true" size={16} />
        <div>
          <strong>{t('login.preconfigured')}</strong>
          <span>{t('login.bundleConfiguredInEnv')}</span>
        </div>
      </div>
    )
  }

  const mode = bundle.credentialPolicy.baseUrlMode
  return (
    <div className="field-grid bootstrap-provider-fields">
      <Field
        autoComplete="off"
        error={touched.has('apiKey') ? errors.apiKey : undefined}
        id="bootstrap-api-key"
        label={
          bundle.credentialPolicy.apiKeyRequired
            ? t('login.apiKey')
            : t('login.providerTokenOptional')
        }
        name="apiKey"
        onBlur={() => update('apiKey', values.apiKey)}
        onChange={(event) => update('apiKey', event.target.value)}
        type="password"
        value={values.apiKey}
      />
      {mode === 'fixed' ? (
        <Field
          id="bootstrap-base-url"
          label={t('login.providerAddress')}
          readOnly
          value={bundle.defaultBaseUrl ?? ''}
          hint={t('login.providerAddressFixedHint')}
        />
      ) : (
        <Field
          error={touched.has('baseUrl') ? errors.baseUrl : undefined}
          id="bootstrap-base-url"
          label={
            mode === 'required' ? t('login.providerAddress') : t('login.providerAddressOptional')
          }
          name="baseUrl"
          onBlur={() => update('baseUrl', values.baseUrl)}
          onChange={(event) => update('baseUrl', event.target.value)}
          type="url"
          value={values.baseUrl}
        />
      )}
    </div>
  )
}

function BootstrapForm({
  bundles,
  loading,
  statusError,
  onAccessFailure,
}: Readonly<{
  bundles: BootstrapProviderBindingBundle[]
  loading: boolean
  statusError: string
  onAccessFailure: (kind: 'forbidden' | 'claimed') => void
}>) {
  const { t } = useTranslation()
  const { bootstrapSetup } = useApp()
  const bundlesPhase = screenPhase({ loading, failed: false, empty: bundles.length === 0 })
  const [values, setValues] = useState(INITIAL_BOOTSTRAP)
  const [touched, setTouched] = useState<ReadonlySet<BootstrapField>>(new Set())
  const [formError, setFormError] = useState('')

  const effectiveProviderKey = values.providerKey || bundles[0]?.providerCatalogId || ''
  const selectedBundle =
    bundles.find((bundle) => bundle.providerCatalogId === effectiveProviderKey) ?? null
  const effectiveValues =
    values.providerKey === effectiveProviderKey
      ? values
      : { ...values, providerKey: effectiveProviderKey }
  const validation = bootstrapSchema(selectedBundle, t).safeParse(effectiveValues)
  const errors = errorMap(validation.error)
  const isReady = selectedBundle ? providerReadiness(selectedBundle, effectiveValues) : false

  const setupMutation = useMutation({
    mutationFn: async (validValues: BootstrapValues) => {
      if (!selectedBundle) throw new Error(t('login.bootstrapBundleRequired'))
      const aiSetup = buildBootstrapAiSetup(selectedBundle, validValues.apiKey, validValues.baseUrl)
      await bootstrapSetup({
        login: validValues.login.trim(),
        email: validValues.email.trim(),
        password: validValues.password,
        ...(validValues.displayName.trim() ? { displayName: validValues.displayName.trim() } : {}),
        ...(aiSetup ? { aiSetup } : {}),
      })
    },
    onError: (error: unknown) => {
      const accessFailure = isBootstrapAccessFailure(error)
      if (accessFailure) {
        onAccessFailure(accessFailure)
        return
      }
      setFormError(t('login.setupFailed'))
    },
  })

  const update = (field: BootstrapField, value: string) => {
    setValues((current) => ({ ...current, [field]: value }))
    setTouched((current) => new Set(current).add(field))
  }

  const selectProvider = (providerKey: string) => {
    setValues((current) => ({ ...current, providerKey, apiKey: '', baseUrl: '' }))
    setTouched((current) => new Set(current).add('providerKey'))
    setFormError('')
  }

  const handleSubmit = (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault()
    setTouched(new Set(Object.keys(INITIAL_BOOTSTRAP) as BootstrapField[]))
    setFormError('')
    if (!validation.success || !isReady) return
    setupMutation.mutate(validation.data)
  }

  return (
    <form className="bootstrap-flow" onSubmit={handleSubmit}>
      <LoginHeader />
      <h1>{t('login.initialSetup')}</h1>
      <p>{t('login.initialSetupDesc')}</p>
      {(formError || statusError) && (
        <div className="login-error" role="alert">
          <AlertCircle aria-hidden="true" size={15} />
          {formError || statusError}
        </div>
      )}

      <section className="bootstrap-section">
        <div className="bootstrap-section__heading">
          <LockKeyhole aria-hidden="true" size={16} />
          <strong>{t('login.adminAccount')}</strong>
        </div>
        <div className="field-grid">
          <Field
            autoComplete="username"
            error={touched.has('login') ? errors.login : undefined}
            id="admin-login"
            label={t('login.adminLogin')}
            name="login"
            onBlur={() => update('login', values.login)}
            onChange={(event) => update('login', event.target.value)}
            placeholder={t('login.loginPlaceholder')}
            value={values.login}
          />
          <Field
            autoComplete="email"
            error={touched.has('email') ? errors.email : undefined}
            id="admin-email"
            label={t('login.email')}
            name="email"
            onBlur={() => update('email', values.email)}
            onChange={(event) => update('email', event.target.value)}
            placeholder={t('login.emailPlaceholder')}
            type="email"
            value={values.email}
          />
          <Field
            autoComplete="name"
            error={touched.has('displayName') ? errors.displayName : undefined}
            id="admin-display-name"
            label={`${t('login.displayName')} (${t('login.optional')})`}
            name="displayName"
            onBlur={() => update('displayName', values.displayName)}
            onChange={(event) => update('displayName', event.target.value)}
            placeholder={t('login.adminNamePlaceholder')}
            value={values.displayName}
          />
          <Field
            autoComplete="new-password"
            error={touched.has('password') ? errors.password : undefined}
            id="admin-password"
            label={t('login.password')}
            name="password"
            onBlur={() => update('password', values.password)}
            onChange={(event) => update('password', event.target.value)}
            placeholder={t('login.passwordPlaceholder')}
            type="password"
            value={values.password}
          />
        </div>
      </section>

      <section className="bootstrap-section">
        <div className="bootstrap-section__heading">
          <Server aria-hidden="true" size={16} />
          <strong>{t('login.aiConfig')}</strong>
        </div>
        <p className="bootstrap-section__description">{t('login.providerHelp')}</p>
        {bundlesPhase === 'loading' && (
          <div className="bootstrap-placeholder">
            <LoaderCircle aria-hidden="true" className="spin" size={16} />
            {t('login.bootstrapStatusLoading')}
          </div>
        )}
        {bundlesPhase === 'empty' && (
          <div className="bootstrap-placeholder">{t('login.noBootstrapBundles')}</div>
        )}
        {bundlesPhase === 'ready' && (
          <>
            <div className="provider-cards">
              {bundles.map((bundle) => {
                const active = bundle.providerCatalogId === selectedBundle?.providerCatalogId
                const ready = providerReadiness(
                  bundle,
                  active ? effectiveValues : INITIAL_BOOTSTRAP,
                )
                return (
                  <button
                    aria-pressed={active}
                    className={active ? 'is-active' : ''}
                    key={bundle.providerCatalogId}
                    onClick={() => selectProvider(bundle.providerCatalogId)}
                    type="button"
                  >
                    <div>
                      <strong>{bundle.displayName}</strong>
                      <Badge tone={bundle.credentialSource === 'env' ? 'ready' : 'warning'}>
                        {bundle.credentialSource === 'env'
                          ? t('login.preconfigured')
                          : t('login.needsInput')}
                      </Badge>
                    </div>
                    <small>{ready ? t('login.summaryReady') : t('login.summaryNeedsInput')}</small>
                  </button>
                )
              })}
            </div>
            {selectedBundle && (
              <>
                <ProviderFields
                  bundle={selectedBundle}
                  errors={errors}
                  touched={touched}
                  update={update}
                  values={effectiveValues}
                />
                <ProviderSummary bundle={selectedBundle} values={effectiveValues} />
                <div className="bootstrap-bindings">
                  {selectedBundle.bindings.map((binding) => {
                    const [labelKey, descriptionKey] = PURPOSE_TRANSLATIONS[binding.bindingPurpose]
                    return (
                      <div key={binding.bindingPurpose}>
                        <Check aria-hidden="true" size={14} />
                        <div>
                          <strong>{t(labelKey)}</strong>
                          <span>{t(descriptionKey)}</span>
                        </div>
                        <code>{binding.modelName}</code>
                      </div>
                    )
                  })}
                </div>
              </>
            )}
          </>
        )}
      </section>

      <Button
        disabled={loading || setupMutation.isPending || !validation.success || !isReady}
        type="submit"
        variant="primary"
      >
        {setupMutation.isPending ? (
          <LoaderCircle aria-hidden="true" className="spin" size={15} />
        ) : (
          <ArrowRight aria-hidden="true" size={15} />
        )}
        {setupMutation.isPending ? t('login.completeSetupPending') : t('login.completeSetup')}
      </Button>
      {/* Validating the key and reading the provider catalogue can take half a
          minute; say so, otherwise the first run looks frozen. */}
      {setupMutation.isPending && (
        <p className="login-progress-note" aria-live="polite">
          {t('login.completeSetupNote')}
        </p>
      )}
    </form>
  )
}

export default function LoginPage() {
  const { t } = useTranslation()
  const location = useLocation()
  const { isBootstrapRequired, isLoading, isSessionExpired, sessionError, ssoAvailable } = useApp()
  const ssoError = useSsoErrorFromAddress(location)
  const [loginNotice, setLoginNotice] = useState('')
  const [forceLogin, setForceLogin] = useState(false)
  const bootstrapQueryOptions = queries.getIamBootstrapOptions()
  const bootstrapEnabled = isBootstrapRequired && !forceLogin
  const bootstrapQuery = useQuery({
    queryKey: bootstrapQueryOptions.queryKey,
    queryFn: authApi.getIamBootstrap,
    enabled: bootstrapEnabled,
    retry: false,
  })
  const queryAccessFailure = isBootstrapAccessFailure(bootstrapQuery.error)
  const showBootstrap = bootstrapEnabled && !queryAccessFailure

  const handleAccessFailure = (kind: 'forbidden' | 'claimed') => {
    setForceLogin(true)
    setLoginNotice(
      kind === 'claimed' ? t('login.bootstrapConflict') : t('login.bootstrapForbidden'),
    )
  }

  const bundles = useMemo(
    () => bootstrapQuery.data?.aiSetup?.bindingBundles ?? [],
    [bootstrapQuery.data],
  )
  const bootstrapStatusError =
    bootstrapQuery.error && !isBootstrapAccessFailure(bootstrapQuery.error)
      ? t('login.bootstrapStatusFetchFailed')
      : ''

  return (
    <main className="login-page">
      <section className="login-panel">
        {isLoading && (
          <div className="login-loading" aria-live="polite">
            <LoaderCircle aria-hidden="true" className="spin" size={20} />
            <span>{t('login.loading')}</span>
          </div>
        )}
        {!isLoading && showBootstrap && (
          <BootstrapForm
            bundles={bundles}
            loading={bootstrapQuery.isLoading}
            onAccessFailure={handleAccessFailure}
            statusError={bootstrapStatusError}
          />
        )}
        {!isLoading && !showBootstrap && (
          <LoginForm
            notice={
              loginNotice ||
              {
                // Same sentence for every rejected single sign-on of a kind: the
                // address says which of the two happened and nothing more.
                ssoDenied: t('login.ssoDenied'),
                ssoFailed: t('login.ssoFailed'),
                // One message for the whole app when the session runs out — the
                // reason the user is looking at this screen outranks whatever
                // individual request happened to fail first.
                expired: t('login.sessionExpired'),
                claimed: t('login.bootstrapConflict'),
                forbidden: t('login.bootstrapForbidden'),
                unavailable: t('login.sessionUnavailable'),
                none: '',
              }[loginNoticeKind({ isSessionExpired, queryAccessFailure, sessionError, ssoError })]
            }
            ssoAvailable={ssoAvailable}
            ssoHref={oidcLoginHref(returnToForSso(location))}
          />
        )}
      </section>
    </main>
  )
}
