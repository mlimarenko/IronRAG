import { useTranslation } from 'react-i18next';
import { BarChart3, Library as LibraryIcon, Plus } from 'lucide-react';

import { Button } from '@/shared/components/ui/button';
import { PageHeader } from '@/shared/components/layout/PageHeader';
import { PageShell } from '@/shared/components/layout/PageShell';
import { WorkbenchEmptyState } from '@/shared/components/layout/WorkbenchEmptyState';
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
    <PageShell
      header={<PageHeader title={t('dashboard.title')} />}
      bodyClassName="empty-state"
    >
        <WorkbenchEmptyState
          icon={<BarChart3 className="h-7 w-7 text-primary" />}
          title={t('dashboard.noLibrary')}
          description={
            can('library.create')
              ? t('dashboard.noLibraryDescOperator')
              : t('dashboard.noLibraryDescViewer')
          }
          action={
            <div className="flex flex-wrap items-center justify-center gap-2.5">
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
          }
        />
    </PageShell>
  );
}
