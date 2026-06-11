import { useTranslation } from 'react-i18next';

import { IngestQueueTab } from './IngestQueueTab';

export default function AdminQueuePage() {
  const { t } = useTranslation();

  return (
    <div className="flex flex-1 min-h-0 flex-col overflow-hidden p-6">
      <IngestQueueTab t={t} active />
    </div>
  );
}
