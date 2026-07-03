import { useTranslation } from 'react-i18next';

import { PageHeader } from '@/shared/components/layout/PageHeader';
import { PageShell } from '@/shared/components/layout/PageShell';
import { IngestQueueTab } from './IngestQueueTab';

export default function AdminQueuePage() {
  const { t } = useTranslation();

  return (
    <PageShell
      header={<PageHeader title={t('admin.nav.queue')} description={t('admin.nav.queueDesc')} />}
      bodyClassName="flex flex-col overflow-hidden"
    >
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden animate-fade-in">
        <IngestQueueTab t={t} active />
      </div>
    </PageShell>
  );
}
