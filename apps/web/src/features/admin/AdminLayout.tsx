import { ArrowLeft, Lock } from 'lucide-react'
import { useTranslation } from 'react-i18next'
import { Route, Routes, useNavigate } from 'react-router-dom'
import NotFoundPage from '@/app/NotFoundPage'
import { usePreferences } from '@/app/preferences-context'
import { Button, PageHeader } from '@/shared/ui'
import AccessAdmin from './AccessAdmin'
import AiAdmin from './AiAdmin'
import AuditAdmin from './AuditAdmin'
import LibrariesAdmin from './LibrariesAdmin'
import QueueAdmin from './QueueAdmin'
import SystemAdmin from './SystemAdmin'
import UsersAdmin from './UsersAdmin'
import WebhooksAdmin from './WebhooksAdmin'

/// A non-administrator used to be bounced straight back to the dashboard with
/// no explanation, so a shared link or a stale bookmark simply appeared not to
/// work. Saying what the section is for, and why it is closed, costs one screen
/// and leaves the reader with a next step instead of a silent redirect.
function AdminNoAccess() {
  const { t } = useTranslation()
  const navigate = useNavigate()
  return (
    <div>
      <PageHeader eyebrow={t('admin.title')} title={t('admin.noAccessTitle')} />
      <output className="state-card state-card--stacked">
        <Lock size={26} />
        <div>
          <strong>{t('admin.noAccessDetail')}</strong>
          <p>{t('admin.noAccessHint')}</p>
          <Button
            variant="primary"
            onClick={() => {
              void navigate('/dashboard')
            }}
          >
            <ArrowLeft size={15} />
            {t('nav.dashboard')}
          </Button>
        </div>
      </output>
    </div>
  )
}

export default function AdminLayout() {
  const { role } = usePreferences()
  if (role !== 'admin') return <AdminNoAccess />
  return (
    <Routes>
      <Route path="libraries" element={<LibrariesAdmin />} />
      <Route path="queue" element={<QueueAdmin />} />
      <Route path="audit" element={<AuditAdmin />} />
      <Route path="ai" element={<AiAdmin />} />
      <Route path="access" element={<AccessAdmin />} />
      <Route path="users" element={<UsersAdmin />} />
      <Route path="webhooks" element={<WebhooksAdmin />} />
      <Route path="system" element={<SystemAdmin />} />
      {/* Not a redirect. `Navigate to="libraries"` is relative, and this route
          sits under a splat, so an unknown path grew a segment per pass:
          /admin/tokens became /admin/tokens/libraries, matched here again, and
          climbed to a sixteen-kilobyte URL that never rendered anything. A
          stale link also deserves to be told it is stale rather than quietly
          landed somewhere else. */}
      <Route path="*" element={<NotFoundPage />} />
    </Routes>
  )
}
