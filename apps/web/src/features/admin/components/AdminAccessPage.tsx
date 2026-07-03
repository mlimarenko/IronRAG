import { useTranslation } from 'react-i18next';

import { PageHeader } from '@/shared/components/layout/PageHeader';
import { PageShell } from '@/shared/components/layout/PageShell';
import { useApp } from '@/shared/contexts/app-context';
import { AccessTab } from './AccessTab';

/** `/admin/access` — API token management. */
export default function AdminAccessPage() {
  const { t } = useTranslation();
  const { activeWorkspace } = useApp();
  return (
    <PageShell
      header={
        <PageHeader title={t('admin.nav.access')} description={t('admin.nav.accessDesc')} />
      }
      bodyScroll="auto"
      bodyClassName="p-3 animate-fade-in sm:p-4"
    >
      <AccessTab t={t} activeWorkspaceId={activeWorkspace?.id} active />
    </PageShell>
  );
}
