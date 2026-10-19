import { useQuery } from '@tanstack/react-query'
import { Bot, FileText, RefreshCw } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { useNavigate } from 'react-router-dom'

import { ChooseLibraryButton } from '@/app/ChooseLibraryButton'
import { queries } from '@/shared/api'
import { useApp } from '@/shared/contexts/app-context'
import { screenPhase } from '@/shared/lib/screenPhase'
import type { DemoScenario } from '@/shared/types'
import { Button, PageHeader, ScreenState } from '@/shared/ui'

import { DashboardContent } from './components/DashboardContent'
import { dashboardAdapter } from './model/dashboardAdapter'
import { useSettledPolling } from './model/useSettledPolling'

/// The gap between settle polls, and the app's only path back from a dashboard
/// query that stalled. Exported so a test can prove the screen loads without
/// waiting for it.
export const DASHBOARD_POLL_INTERVAL_MS = 2_500
function DashboardLoadingState({ label }: Readonly<{ label: string }>) {
  return (
    <output className="dashboard-skeleton" aria-label={label}>
      <div className="metric-grid dashboard-metrics" aria-hidden="true">
        {[0, 1, 2].map((item) => (
          <div className="metric-card dashboard-skeleton__metric" key={item}>
            <span className="skeleton-line short" />
            <span className="skeleton-line" />
            <span className="skeleton-line short" />
          </div>
        ))}
      </div>
      <div className="dashboard-grid" aria-hidden="true">
        <div className="panel dashboard-skeleton__panel">
          <span className="skeleton-line short" />
          <span className="skeleton-line" />
          <span className="skeleton-line" />
          <span className="skeleton-line short" />
        </div>
        <div className="panel dashboard-skeleton__panel">
          <span className="skeleton-line short" />
          <span className="skeleton-line" />
          <span className="skeleton-line short" />
        </div>
      </div>
    </output>
  )
}

export default function DashboardPage() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  const { activeLibrary, locale } = useApp()
  const libraryId = activeLibrary?.id ?? ''
  const query = useQuery({
    ...queries.getLibraryDashboardOptions({
      path: { libraryId },
    }),
    enabled: activeLibrary !== null,
    select: dashboardAdapter,
  })
  const data = query.data
  const refetch = query.refetch
  useSettledPolling(() => refetch(), DASHBOARD_POLL_INTERVAL_MS, activeLibrary !== null)
  // A poll that fails after the screen is already populated is a background
  // event, not a screen failure: the next poll usually succeeds, and replacing
  // rendered data with an error page loses everything the operator was reading.
  // The error screen is reserved for having nothing to show at all.
  const scenario: DemoScenario = activeLibrary
    ? screenPhase({
        loading: query.isLoading,
        failed: query.isError && !data,
        empty: data?.metrics.total === 0,
      })
    : 'empty'
  const emptyText = activeLibrary ? t('dashboard.noDocs') : t('dashboard.noLibrary')

  const refresh = () => {
    if (activeLibrary) void query.refetch()
  }

  return (
    <div>
      <PageHeader
        eyebrow={t('dashboard.eyebrow')}
        // The eyebrow already names the screen, and the sidebar entry now uses
        // that same wording. Falling back to the navigation label here would
        // print the section name twice in the same header and still not say
        // what is missing.
        title={activeLibrary?.name ?? t('dashboard.noLibrary')}
        actions={
          <>
            <Button onClick={refresh} disabled={!activeLibrary || query.isFetching}>
              <RefreshCw size={15} />
              {t('common.refresh')}
            </Button>
            <Button
              variant="primary"
              onClick={() => {
                void navigate('/assistant')
              }}
            >
              <Bot size={16} />
              {t('dashboard.ask')}
            </Button>
          </>
        }
      />
      {query.isError && data && (
        <output className="dashboard-refresh-warning">{t('dashboard.refreshFailed')}</output>
      )}
      {scenario === 'loading' ? (
        <DashboardLoadingState label={t('common.loading')} />
      ) : (
        <ScreenState
          scenario={scenario}
          onRetry={refresh}
          emptyText={emptyText}
          emptyHint={activeLibrary ? t('dashboard.emptyHint') : t('dashboard.noLibraryHint')}
          action={
            activeLibrary ? (
              <Button
                variant="primary"
                onClick={() => {
                  void navigate('/documents')
                }}
              >
                <FileText size={15} />
                {t('dashboard.emptyAction')}
              </Button>
            ) : (
              // Without a library the hint used to name the top-bar picker and
              // stop there, leaving the reader to hunt for it. The control that
              // resolves the state belongs in the state.
              <ChooseLibraryButton />
            )
          }
        >
          {data && <DashboardContent data={data} locale={locale} />}
        </ScreenState>
      )}
    </div>
  )
}
