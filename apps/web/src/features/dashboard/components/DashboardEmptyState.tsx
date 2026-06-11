import { useTranslation } from 'react-i18next';
import { BarChart3, Library as LibraryIcon, Plus } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { useCan } from '@/shared/auth/useCan';
import { emitShellIntent } from '@/shared/lib/shell-events';

/**
 * Actionable empty state shown when no library is active. Replaces the old
 * "discover the unlabelled selector yourself" dead end (DSH-03) with real
 * CTAs: open the library picker (everyone) and — gated to operator+ via
 * `library.create` — create a first library. Both drive shell-owned UI through
 * the `emitShellIntent` bridge so this stays decoupled from the shell.
 */
export function DashboardEmptyState() {
  const { t } = useTranslation();
  const { can } = useCan();

  return (
    <div className="flex-1 flex flex-col">
      <div className="page-header">
        <h1 className="text-lg font-bold tracking-tight">{t('dashboard.title')}</h1>
      </div>
      <div className="empty-state flex-1 ambient-bg relative">
        <div className="relative z-10 flex flex-col items-center">
          <div className="w-14 h-14 rounded-2xl bg-accent-subtle flex items-center justify-center mb-4 ring-1 ring-border/60">
            <BarChart3 className="h-7 w-7 text-primary" />
          </div>
          <h2 className="text-base font-bold tracking-tight">{t('dashboard.noLibrary')}</h2>
          <p className="text-sm text-muted-foreground mt-2 max-w-sm leading-relaxed">
            {can('library.create')
              ? t('dashboard.noLibraryDescOperator')
              : t('dashboard.noLibraryDescViewer')}
          </p>
          <div className="mt-6 flex flex-wrap items-center justify-center gap-2.5">
            <Button size="sm" onClick={() => emitShellIntent('open-library-picker')}>
              <LibraryIcon className="h-3.5 w-3.5 mr-1.5" />
              {t('dashboard.selectLibrary')}
            </Button>
            {can('library.create') && (
              <Button
                variant="outline"
                size="sm"
                onClick={() => emitShellIntent('create-library')}
              >
                <Plus className="h-3.5 w-3.5 mr-1.5" />
                {t('dashboard.createLibrary')}
              </Button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
