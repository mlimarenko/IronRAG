import { useTranslation } from 'react-i18next';

import { PageHeader } from '@/shared/components/layout/PageHeader';
import { PageShell } from '@/shared/components/layout/PageShell';
import { LibrariesTab } from './LibrariesTab';

/** `/admin/libraries` — global cross-workspace library catalog. Per-library
 *  backup/restore/AI/delete are direct row + inspector actions. */
export default function AdminLibrariesPage() {
  const { t } = useTranslation();
  return (
    <PageShell
      header={
        <PageHeader
          title={t('admin.nav.libraries')}
          description={t('admin.nav.librariesDesc')}
        />
      }
      bodyClassName="flex flex-col overflow-hidden"
    >
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden animate-fade-in">
        <LibrariesTab active />
      </div>
    </PageShell>
  );
}
