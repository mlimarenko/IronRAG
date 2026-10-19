import { useQuery } from '@tanstack/react-query'
import {
  AlertTriangle,
  CheckCircle2,
  Database,
  ExternalLink,
  HardDrive,
  LoaderCircle,
  RefreshCw,
  Server,
  Terminal,
} from 'lucide-react'
import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'
import { queries } from '@/shared/api'
import type { DependencyStatus, StorageStatus } from '@/shared/api/generated'
import { versionApi } from '@/shared/api/version'
import { useApp } from '@/shared/contexts/app-context'
import { formatDate, formatRelative } from '@/shared/format'
import { healthStatusLabel, systemDetailLabel, tokenKindLabel } from '@/shared/lib/enumLabels'
import { screenPhase } from '@/shared/lib/screenPhase'
import { Badge, Button, CopyButton, PageHeader, Panel } from '@/shared/ui'
import { getMcpConfigs } from './model/mcpConfigs'
import { useAdminBreadcrumb } from './useAdminBreadcrumb'

type HealthItem = Readonly<{
  detail: string
  icon: typeof Database
  name: string
  status: string
  technicalDetail: string
}>

function dependencyItem(
  name: string,
  icon: typeof Database,
  dependency: DependencyStatus,
  t: TFunction,
): HealthItem {
  return {
    name,
    icon,
    status: dependency.status,
    detail: dependency.message ?? systemDetailLabel(dependency.mode, t),
    technicalDetail: dependency.mode,
  }
}

function storageItem(storage: StorageStatus, t: TFunction): HealthItem {
  return {
    name: t('admin.systemPage.storage'),
    icon: HardDrive,
    status: storage.status,
    detail:
      storage.message ??
      `${systemDetailLabel(storage.provider, t)}, ${systemDetailLabel(storage.topology, t)}`,
    technicalDetail: `${storage.provider} · ${storage.topology}`,
  }
}

function QueryFailure({ label, retry }: Readonly<{ label: string; retry: () => void }>) {
  const { t } = useTranslation()
  return (
    <div className="state-card state-card--error">
      <AlertTriangle size={22} />
      <div>
        <strong>{label}</strong>
        <Button size="sm" onClick={retry}>
          {t('common.tryAgain')}
        </Button>
      </div>
    </div>
  )
}

// A dependency is green only when it reports ok; "down" and "misconfigured"
// are hard failures, and everything else (degraded, unknown) is a warning.
function healthTone(status: DependencyStatus | StorageStatus | string): string {
  if (status === 'ok') return 'ready'
  if (status === 'down' || status === 'misconfigured') return 'failed'
  return 'warning'
}

export default function SystemAdmin() {
  const { t } = useTranslation()
  const adminBreadcrumb = useAdminBreadcrumb()
  const { locale } = useApp()
  const origin = window.location.origin
  const mcpUrl = `${origin}/v1/mcp`
  const capabilitiesUrl = `${origin}/v1/mcp/capabilities`
  const configs = getMcpConfigs(origin)
  const readinessQuery = useQuery(queries.getReadinessOptions())
  const capabilitiesQuery = useQuery(queries.getMcpCapabilitiesOptions())
  const promptQuery = useQuery(queries.getAssistantSystemPromptOptions())
  const versionQuery = useQuery({
    queryKey: queries.getLatestVersionQueryKey(),
    queryFn: versionApi.getLatestVersion,
  })
  const readiness = readinessQuery.data
  const capabilitiesPhase = screenPhase({
    loading: capabilitiesQuery.isLoading,
    failed: capabilitiesQuery.isError,
  })
  const promptPhase = screenPhase({ loading: promptQuery.isLoading, failed: promptQuery.isError })
  const healthItems: HealthItem[] = readiness
    ? [
        dependencyItem('PostgreSQL', Database, readiness.dependencies.postgres, t),
        dependencyItem('Redis', Server, readiness.dependencies.redis, t),
        storageItem(readiness.storage, t),
      ]
    : []
  const readyCount = healthItems.filter((item) => item.status === 'ok').length
  const prompt = promptQuery.data?.rendered ?? promptQuery.data?.template

  return (
    <div className="aicfg-readable-labels">
      <PageHeader
        eyebrow={adminBreadcrumb}
        title={t('admin.systemTitle')}
        description={t('admin.systemSubtitle')}
      />
      <div
        className={`system-status-banner system-status-banner--${readiness?.status ?? 'loading'}`}
      >
        {readinessQuery.isLoading && <LoaderCircle className="spin" size={18} />}
        {!readinessQuery.isLoading && readiness?.status === 'ready' && <CheckCircle2 size={18} />}
        {!readinessQuery.isLoading && readiness?.status !== 'ready' && <AlertTriangle size={18} />}
        <div>
          <strong>
            {readiness ? t(`admin.systemHealth.${readiness.status}`) : t('common.loading')}
          </strong>
          {readiness?.message && <small>{readiness.message}</small>}
        </div>
        {readiness?.checkedAt && (
          <time dateTime={readiness.checkedAt} title={formatDate(readiness.checkedAt, locale)}>
            {formatRelative(readiness.checkedAt, locale, t('common.justNow'))}
          </time>
        )}
      </div>
      <div className="aicfg-system-stack">
        <Panel className="system-health">
          <div className="panel-heading">
            <div>
              <h2>{t('admin.dependencies')}</h2>
            </div>
            {healthItems.length > 0 && readyCount < healthItems.length && (
              <span className="health-score">
                {t('admin.systemPage.healthyCount', {
                  ready: readyCount,
                  total: healthItems.length,
                })}
              </span>
            )}
          </div>
          {readinessQuery.isError ? (
            <QueryFailure
              label={t('admin.systemHealth.loadFailed')}
              retry={() => void readinessQuery.refetch()}
            />
          ) : (
            <div className="health-list">
              {healthItems.map((item) => {
                const Icon = item.icon
                return (
                  <div key={item.name} title={item.technicalDetail}>
                    <Icon size={17} />
                    <div>
                      <strong>{item.name}</strong>
                      <small>{item.detail}</small>
                    </div>
                    <Badge tone={healthTone(item.status)}>
                      {healthStatusLabel(item.status, t)}
                    </Badge>
                  </div>
                )
              })}
            </div>
          )}
          {versionQuery.data?.status === 'update_available' && (
            <div className="update-card">
              <div>
                <RefreshCw size={16} />
                <span>
                  <strong>
                    {t('admin.systemHealth.updateAvailable', {
                      version: versionQuery.data.latestVersion,
                    })}
                  </strong>
                  <small>
                    {t('admin.systemHealth.currentVersion', {
                      version: versionQuery.data.currentVersion,
                    })}
                  </small>
                </span>
              </div>
              {versionQuery.data.releaseUrl && (
                <a href={versionQuery.data.releaseUrl} target="_blank" rel="noreferrer">
                  {t('admin.systemHealth.releaseNotes')}
                  <ExternalLink size={12} />
                </a>
              )}
            </div>
          )}
        </Panel>
        <a className="swagger-link aicfg-developer-link" href="/swagger.html">
          <Terminal size={16} />
          <span>
            <strong>{t('shell.apiDocs')}</strong>
            <small>{t('admin.systemPage.developerDesc')}</small>
          </span>
          <ExternalLink size={13} />
        </a>
        {versionQuery.isError && <small>{t('admin.systemHealth.versionFailed')}</small>}
      </div>
      <Panel className="mcp-guide">
        <div className="panel-heading">
          <div>
            <h2>{t('admin.mcpGuide')}</h2>
            <p>{t('admin.mcpParityDesc')}</p>
          </div>
          <span className="protocol-chip aicfg-protocol-chip">Streamable HTTP</span>
        </div>
        <div className="mcp-endpoints">
          <div>
            <span>{t('admin.mcpServerUrl')}</span>
            <code>{mcpUrl}</code>
            <CopyButton value={mcpUrl} />
          </div>
          <div>
            <span>{t('admin.capabilitiesPolished')}</span>
            <code>{capabilitiesUrl}</code>
            <CopyButton value={capabilitiesUrl} />
          </div>
        </div>
        <p className="mcp-note">{t('admin.mcpScopeNote')}</p>
        <div className="mcp-capability-summary">
          {capabilitiesPhase === 'loading' && (
            <>
              <LoaderCircle className="spin" size={16} />
              {t('common.loading')}
            </>
          )}
          {capabilitiesPhase === 'error' && (
            <>
              <AlertTriangle size={16} />
              {t('admin.systemHealth.capabilitiesFailed')}
              <Button size="sm" onClick={() => void capabilitiesQuery.refetch()}>
                {t('common.tryAgain')}
              </Button>
            </>
          )}
          {capabilitiesPhase === 'ready' && capabilitiesQuery.data && (
            <>
              <Badge tone="active">{tokenKindLabel(capabilitiesQuery.data.tokenKind, t)}</Badge>
              <span>
                {t('admin.systemHealth.visibleWorkspaces', {
                  count: capabilitiesQuery.data.visibleWorkspaceCount,
                })}
              </span>
              <span>
                {t('admin.systemHealth.visibleLibraries', {
                  count: capabilitiesQuery.data.visibleLibraryCount,
                })}
              </span>
              <span>
                {t('admin.systemHealth.visibleTools', {
                  count: capabilitiesQuery.data.tools?.length ?? 0,
                })}
              </span>
            </>
          )}
        </div>
        <div className="client-configs">
          {configs.map(({ name, config }) => (
            <details key={name}>
              <summary>
                {name}
                <CopyButton value={config} label={t('admin.copyConfig')} />
              </summary>
              <pre>{config}</pre>
            </details>
          ))}
        </div>
        <div className="system-prompt">
          <div className="system-prompt-header">
            <span>{t('admin.systemPrompt')}</span>
            {prompt && <CopyButton value={prompt} />}
          </div>
          {promptPhase === 'loading' && (
            <div className="state-card aicfg-loading-state">
              <LoaderCircle className="spin" size={18} />
              <strong>{t('common.loading')}</strong>
            </div>
          )}
          {promptPhase === 'error' && (
            <QueryFailure
              label={t('admin.systemHealth.promptFailed')}
              retry={() => void promptQuery.refetch()}
            />
          )}
          {promptPhase === 'ready' && (
            <details className="aicfg-system-prompt">
              <summary>{t('common.showMore')}</summary>
              <pre>{prompt}</pre>
            </details>
          )}
        </div>
      </Panel>
    </div>
  )
}
