import { useTranslation } from 'react-i18next';

import { useApp } from '@/shared/contexts/app-context';
import { AccessTab } from './AccessTab';

/** `/admin/access` — API token management. */
export default function AdminAccessPage() {
  const { t } = useTranslation();
  const { activeWorkspace } = useApp();
  return (
    <div className="flex flex-1 min-h-0 flex-col overflow-auto p-6">
      <AccessTab t={t} activeWorkspaceId={activeWorkspace?.id} active />
    </div>
  );
}
