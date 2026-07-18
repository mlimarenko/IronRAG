import { useTranslation } from 'react-i18next'
import { PageHeader } from '@/shared/components/layout/PageHeader'
import { PageShell } from '@/shared/components/layout/PageShell'

/**
 * Layout-matched loading skeleton for the dashboard. Mirrors the real grid
 * (3 summary rows + a two-column panel layout) so the page paints
 * with zero layout shift when data resolves. Uses the token-driven `.shimmer`
 * utility and the same `workbench-surface` / `stat-tile` chrome as the loaded
 * state, so it reads as the same surface "filling in" rather than a generic
 * spinner. A visually-hidden status line keeps it announced to AT.
 */

const SUMMARY_TILE_IDS = ['summary-primary', 'summary-secondary', 'summary-tertiary'] as const
const BREAKDOWN_ROW_IDS = [
  'breakdown-alpha',
  'breakdown-beta',
  'breakdown-gamma',
  'breakdown-delta',
] as const
const METRIC_TILE_IDS = ['metric-primary', 'metric-secondary', 'metric-tertiary'] as const
const ACTIVITY_TILE_IDS = [
  'activity-alpha',
  'activity-beta',
  'activity-gamma',
  'activity-delta',
] as const
const HEALTH_CARD_IDS = ['health-primary', 'health-secondary', 'health-tertiary'] as const
const FOOTER_TILE_IDS = ['footer-primary', 'footer-secondary', 'footer-tertiary'] as const

function Block({ className = '' }: Readonly<{ className?: string }>) {
  return <div className={`shimmer rounded-md ${className}`} />
}

export function DashboardSkeleton() {
  const { t } = useTranslation()

  return (
    <PageShell
      header={
        <PageHeader
          title={t('dashboard.title')}
          description={<Block className="h-3.5 w-64" />}
          actions={
            <div className="flex gap-2">
              <Block className="h-8 w-32 rounded-lg" />
              <Block className="h-8 w-24 rounded-lg" />
            </div>
          }
        />
      }
      bodyScroll="auto"
      bodyClassName="space-y-5 p-3 sm:p-4"
    >
      <div className="space-y-5" role="status" aria-live="polite" aria-busy="true">
        <span className="sr-only">{t('dashboard.loadingDashboard')}</span>

        {/* Summary tiles */}
        <div className="grid gap-2 sm:grid-cols-3">
          {SUMMARY_TILE_IDS.map((id) => (
            <div key={id} className="flex items-center gap-3 workbench-surface px-3 py-2.5">
              <Block className="h-8 w-8 rounded-md" />
              <div className="min-w-0 flex-1 space-y-2">
                <Block className="h-6 w-16" />
                <Block className="h-3 w-28" />
              </div>
            </div>
          ))}
        </div>

        {/* Two-column panels */}
        <div className="grid items-stretch gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(320px,1fr)]">
          <div className="flex flex-col gap-4">
            <div className="workbench-surface space-y-4 p-4">
              <Block className="h-4 w-36" />
              {BREAKDOWN_ROW_IDS.map((id) => (
                <div key={id} className="space-y-2">
                  <Block className="h-3 w-full" />
                  <Block className="h-2 w-full rounded-full" />
                </div>
              ))}
              <div className="grid grid-cols-3 gap-3 pt-1">
                {METRIC_TILE_IDS.map((id) => (
                  <Block key={id} className="h-16 rounded-xl" />
                ))}
              </div>
            </div>
            <div className="workbench-surface h-full flex-1 space-y-4 p-4">
              <Block className="h-4 w-40" />
              <div className="grid gap-3 xl:grid-cols-2">
                {ACTIVITY_TILE_IDS.map((id) => (
                  <Block key={id} className="h-20 rounded-xl" />
                ))}
              </div>
            </div>
          </div>

          <div className="flex flex-col gap-4">
            <div className="workbench-surface space-y-3 p-4">
              <Block className="h-4 w-32" />
              {HEALTH_CARD_IDS.map((id) => (
                <Block key={id} className="h-16 rounded-lg" />
              ))}
            </div>
            <div className="workbench-surface h-full flex-1 space-y-3 p-4">
              <Block className="h-4 w-28" />
              <Block className="h-12 rounded-lg" />
              <div className="grid grid-cols-3 gap-3">
                {FOOTER_TILE_IDS.map((id) => (
                  <Block key={id} className="h-14 rounded-xl" />
                ))}
              </div>
            </div>
          </div>
        </div>
      </div>
    </PageShell>
  )
}
