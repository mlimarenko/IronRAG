import { AlertTriangle, CheckCircle2, ChevronRight, Sparkles } from 'lucide-react'
import { useMemo, useState } from 'react'
import { useSearchParams } from 'react-router-dom'
import { useTranslation } from 'react-i18next'

import { useApp } from '@/shared/contexts/app-context'
import type { AIScopeKind } from '@/shared/types'
import { Badge, Button, PageHeader, Tabs } from '@/shared/ui'
import { AccountsSection } from './ai/AccountsSection'
import { AiWizard } from './ai/AiWizard'
import { BindingsSection } from './ai/BindingsSection'
import { DataBoundary } from './ai/common'
import { ModelsSection } from './ai/ModelsSection'
import { ProvidersSection } from './ai/ProvidersSection'
import { useAiAdminData } from './ai/api'
import {
  aiReadinessStage,
  reachableAiScope,
  recommendAiConfigSection,
  resolveBindingForPurpose,
  summarizeAiReadiness,
  type AiConfigSection,
} from './model/aiConfig'
import { useLibraryDeepLink } from '@/shared/hooks/useLibraryDeepLink'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

const SCOPES = new Set<AIScopeKind>(['instance', 'workspace', 'library'])
const SECTIONS = new Set<AiConfigSection>(['bindings', 'accounts', 'catalog'])
// Every binding purpose is required. Document understanding used to be
// exempt, which read as "you can skip this" while a deployment without it
// silently loses OCR and image content from every ingested document.
const REQUIRED_PURPOSES = [
  'extract_text',
  'extract_graph',
  'embed_chunk',
  'query_compile',
  'query_answer',
  'agent',
] as const

function parsedScope(value: string | null): AIScopeKind {
  return SCOPES.has(value as AIScopeKind) ? (value as AIScopeKind) : 'library'
}

function parsedSection(value: string | null): AiConfigSection {
  return SECTIONS.has(value as AiConfigSection) ? (value as AiConfigSection) : 'bindings'
}

export default function AiAdmin() {
  const { t } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const { activeWorkspace, activeLibrary, selectLibrary } = useApp()
  const [params, setParams] = useSearchParams()
  const [accountCreateRequest, setAccountCreateRequest] = useState(0)
  const [consumedAccountCreateRequest, setConsumedAccountCreateRequest] = useState(0)
  const requestedScope = parsedScope(params.get('scope'))
  const selectedScope = reachableAiScope(
    requestedScope,
    Boolean(activeWorkspace),
    Boolean(activeLibrary),
  )
  const section = parsedSection(params.get('section'))
  const catalogSection = params.get('catalog') === 'providers' ? 'providers' : 'models'
  const wizardOpen = params.get('wizard') === '1'
  const scopeContext = useMemo(
    () => ({
      workspaceId: activeWorkspace?.id,
      libraryId: activeLibrary?.id,
    }),
    [activeLibrary?.id, activeWorkspace?.id],
  )
  const data = useAiAdminData(selectedScope, scopeContext)
  const readiness = useMemo(() => {
    const base = summarizeAiReadiness({
      selectedScope,
      availableAccounts: data.availableAccounts,
      localAccounts: data.localAccounts,
      bindingsForScope: data.bindingsForScope,
      instanceBindings: data.instanceBindings,
      workspaceBindings: data.workspaceBindings,
      models: data.models,
      providers: data.providers,
      priceRuleCount: data.models.length,
    })
    const missingPurposes = REQUIRED_PURPOSES.filter((purpose) => {
      const binding = resolveBindingForPurpose({
        purpose,
        selectedScope,
        bindingsForScope: data.bindingsForScope,
        instanceBindings: data.instanceBindings,
        workspaceBindings: data.workspaceBindings,
      }).effectiveBinding
      const account = data.availableAccounts.find((entry) => entry.id === binding?.accountId)
      const model = data.models.find((entry) => entry.id === binding?.modelCatalogId)
      return (
        binding?.state !== 'active' ||
        account?.state !== 'active' ||
        !model ||
        model.providerCatalogId !== account.providerId ||
        !model.allowedBindingPurposes.includes(purpose) ||
        !model.availableAccountIds.includes(account.id)
      )
    })
    return {
      ...base,
      totalPurposes: REQUIRED_PURPOSES.length,
      executableEffectiveBindings: REQUIRED_PURPOSES.length - missingPurposes.length,
      missingPurposes,
    }
  }, [
    data.availableAccounts,
    data.bindingsForScope,
    data.instanceBindings,
    data.localAccounts,
    data.models,
    data.providers,
    data.workspaceBindings,
    selectedScope,
  ])

  useLibraryDeepLink({
    params,
    setParams,
    activeLibraryId: activeLibrary?.id,
    selectLibrary,
  })
  const updateParam = (key: string, value: string | null) => {
    const next = new URLSearchParams(params)
    if (value === null) next.delete(key)
    else next.set(key, value)
    setParams(next)
  }
  const changeScope = (scope: AIScopeKind) => {
    if ((scope === 'workspace' && !activeWorkspace) || (scope === 'library' && !activeLibrary))
      return
    updateParam('scope', scope)
  }
  const changeSection = (nextSection: AiConfigSection) => {
    updateParam('section', nextSection)
  }
  const setWizard = (open: boolean) => {
    updateParam('wizard', open ? '1' : null)
  }
  const openRecommended = () => {
    const recommended = recommendAiConfigSection(readiness)
    changeSection(recommended)
    if (recommended === 'accounts') setAccountCreateRequest((value) => value + 1)
  }
  const ready = readiness.activeAccountCount > 0 && readiness.missingPurposes.length === 0
  // Title and detail live in one record keyed by the stage so a future stage
  // cannot gain a heading without gaining the sentence underneath it — they
  // used to be two independent ternary chains over the same two conditions.
  const { title: readinessTitle, detail: readinessDetail } = {
    missingAccounts: {
      title: t('admin.aiPanel.readiness.missingAccountsTitle'),
      detail: t('admin.aiPanel.readiness.missingAccountsDetail'),
    },
    ready: {
      title: t('admin.aiPanel.readiness.readyTitle'),
      detail: t('admin.aiPanel.readiness.readyDetail'),
    },
    missingBindings: {
      title: t('admin.aiPanel.readiness.missingBindingsTitle', {
        count: readiness.missingPurposes.length,
      }),
      detail: t('admin.aiPanel.readiness.missingBindingsDetail', {
        purposes: readiness.missingPurposes
          .map((purpose) => t(`admin.aiPanel.purposeLabels.${purpose}`))
          .join(', '),
      }),
    },
  }[aiReadinessStage(readiness.activeAccountCount, ready)]

  return (
    <div className="aicfg-readable-labels">
      <PageHeader
        eyebrow={adminBreadcrumb}
        title={t('admin.aiTitle')}
        description={t('admin.aiSubtitle')}
        actions={
          <Button variant="primary" onClick={() => setWizard(true)}>
            <Sparkles size={15} />
            {t('admin.aiWizard.launch')}
          </Button>
        }
      />
      <DataBoundary loading={data.isLoading} error={data.error} onRetry={() => void data.refetch()}>
        <div className="ai-scope-bar">
          <div
            className="inheritance-ladder aicfg-ladder"
            title={t('admin.aiPanel.inheritanceTooltip')}
            aria-label={t('common.scope')}
          >
            <button
              type="button"
              className={selectedScope === 'instance' ? 'is-active' : ''}
              onClick={() => changeScope('instance')}
            >
              {t('admin.aiPanel.scopeNames.instance')}
            </button>
            <ChevronRight size={13} />
            <button
              type="button"
              className={selectedScope === 'workspace' ? 'is-active' : ''}
              disabled={!activeWorkspace}
              onClick={() => changeScope('workspace')}
            >
              {activeWorkspace?.name ?? t('admin.aiPanel.scopeNames.workspace')}
            </button>
            <ChevronRight size={13} />
            <button
              type="button"
              className={selectedScope === 'library' ? 'is-active' : ''}
              disabled={!activeLibrary}
              onClick={() => changeScope('library')}
            >
              {activeLibrary?.name ?? t('admin.aiPanel.scopeNames.library')}
            </button>
          </div>
        </div>
        <div className={`ai-readiness ${ready ? '' : 'ai-readiness--warning'}`}>
          <div>
            {ready ? <CheckCircle2 size={20} /> : <AlertTriangle size={20} />}
            <div>
              <strong>{readinessTitle}</strong>
              <p>{readinessDetail}</p>
            </div>
          </div>
          <div className="ai-readiness__action">
            <span>
              {t('admin.aiPanel.readiness.configuredRequired', {
                configured: readiness.executableEffectiveBindings,
                total: readiness.totalPurposes,
              })}
            </span>
            {!ready && (
              <Button size="sm" onClick={openRecommended}>
                {t(`admin.aiPanel.readiness.actions.${recommendAiConfigSection(readiness)}`)}
              </Button>
            )}
          </div>
        </div>
        <Tabs
          id="ai-settings"
          label={t('admin.aiTitle')}
          value={section}
          onChange={changeSection}
          items={[
            { value: 'bindings', label: t('admin.bindings') },
            { value: 'accounts', label: t('admin.accounts') },
            { value: 'catalog', label: t('admin.catalog') },
          ]}
        >
          {section === 'bindings' && (
            <BindingsSection
              selectedScope={selectedScope}
              scopeContext={scopeContext}
              availableAccounts={data.availableAccounts}
              models={data.models}
              bindingsForScope={data.bindingsForScope}
              instanceBindings={data.instanceBindings}
              workspaceBindings={data.workspaceBindings}
            />
          )}
          {section === 'accounts' && (
            <AccountsSection
              key={`${selectedScope}:${scopeContext.workspaceId ?? ''}:${scopeContext.libraryId ?? ''}:${accountCreateRequest}`}
              selectedScope={selectedScope}
              scopeContext={scopeContext}
              accounts={data.availableAccounts}
              providers={data.providers}
              requestCreate={
                accountCreateRequest > consumedAccountCreateRequest ? accountCreateRequest : 0
              }
              onCreateRequestConsumed={() => setConsumedAccountCreateRequest(accountCreateRequest)}
            />
          )}
          {section === 'catalog' && (
            <Tabs
              id="ai-catalog"
              label={t('admin.catalog')}
              value={catalogSection}
              onChange={(value) => updateParam('catalog', value === 'models' ? null : value)}
              items={[
                {
                  value: 'models',
                  label: t('admin.aiPanel.metrics.visibleModels'),
                  suffix: <Badge>{data.models.length}</Badge>,
                },
                {
                  value: 'providers',
                  label: t('admin.providers'),
                  suffix: <Badge>{data.providers.length}</Badge>,
                },
              ]}
            >
              {catalogSection === 'providers' ? (
                <ProvidersSection
                  providers={data.providers}
                  models={data.models}
                  accounts={data.availableAccounts}
                />
              ) : (
                <ModelsSection models={data.models} providers={data.providers} />
              )}
            </Tabs>
          )}
        </Tabs>
      </DataBoundary>
      <AiWizard
        open={wizardOpen}
        onOpenChange={setWizard}
        selectedScope={selectedScope}
        onScopeChange={changeScope}
        scopeContext={scopeContext}
        accounts={data.availableAccounts}
        providers={data.providers}
        models={data.models}
      />
    </div>
  )
}
