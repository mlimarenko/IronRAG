import { useTranslation } from 'react-i18next';

/**
 * Layout-matched loading skeleton for the dashboard. Mirrors the real grid
 * (4 summary tiles + a 1.55fr/1fr two-column panel layout) so the page paints
 * with zero layout shift when data resolves. Uses the token-driven `.shimmer`
 * utility and the same `workbench-surface` / `stat-tile` chrome as the loaded
 * state, so it reads as the same surface "filling in" rather than a generic
 * spinner. A visually-hidden status line keeps it announced to AT.
 */
function Block({ className = '' }: { className?: string }) {
  return <div className={`shimmer rounded-md ${className}`} />;
}

export function DashboardSkeleton() {
  const { t } = useTranslation();

  return (
    <div className="flex-1 flex flex-col overflow-auto ambient-bg">
      <div className="page-header flex items-center justify-between gap-4 flex-wrap relative z-10">
        <div className="space-y-2">
          <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
          <Block className="h-3.5 w-64" />
        </div>
        <div className="flex gap-2">
          <Block className="h-8 w-32 rounded-lg" />
          <Block className="h-8 w-24 rounded-lg" />
        </div>
      </div>

      <div
        className="flex-1 p-6 space-y-5 relative z-10"
        role="status"
        aria-live="polite"
        aria-busy="true"
      >
        <span className="sr-only">{t('dashboard.loadingDashboard')}</span>

        {/* Summary tiles */}
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          {Array.from({ length: 4 }).map((_, i) => (
            <div key={i} className="stat-tile">
              <Block className="h-10 w-10 rounded-xl" />
              <div className="mt-4 space-y-2">
                <Block className="h-2.5 w-16" />
                <Block className="h-7 w-20" />
                <Block className="h-3 w-28" />
              </div>
            </div>
          ))}
        </div>

        {/* Two-column panels */}
        <div className="grid items-start gap-4 xl:grid-cols-[minmax(0,1.55fr)_minmax(320px,1fr)]">
          <div className="grid gap-4">
            <div className="workbench-surface p-5 sm:p-6 space-y-4">
              <Block className="h-4 w-36" />
              {Array.from({ length: 4 }).map((_, i) => (
                <div key={i} className="space-y-2">
                  <Block className="h-3 w-full" />
                  <Block className="h-2 w-full rounded-full" />
                </div>
              ))}
              <div className="grid grid-cols-3 gap-3 pt-1">
                {Array.from({ length: 3 }).map((_, i) => (
                  <Block key={i} className="h-16 rounded-xl" />
                ))}
              </div>
            </div>
            <div className="workbench-surface p-5 sm:p-6 space-y-4">
              <Block className="h-4 w-40" />
              <div className="grid gap-3 xl:grid-cols-2">
                {Array.from({ length: 4 }).map((_, i) => (
                  <Block key={i} className="h-20 rounded-xl" />
                ))}
              </div>
            </div>
          </div>

          <div className="grid gap-4">
            <div className="workbench-surface p-5 sm:p-6 space-y-3">
              <Block className="h-4 w-32" />
              {Array.from({ length: 3 }).map((_, i) => (
                <Block key={i} className="h-16 rounded-xl" />
              ))}
            </div>
            <div className="workbench-surface p-5 sm:p-6 space-y-3">
              <Block className="h-4 w-28" />
              <Block className="h-12 rounded-xl" />
              <div className="grid grid-cols-3 gap-3">
                {Array.from({ length: 3 }).map((_, i) => (
                  <Block key={i} className="h-14 rounded-xl" />
                ))}
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
