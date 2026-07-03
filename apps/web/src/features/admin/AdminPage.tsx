import { Navigate, Route, Routes } from 'react-router-dom';

import { useCan } from '@/shared/auth/useCan';
import AdminLibrariesPage from '@/features/admin/components/AdminLibrariesPage';
import AdminAiPage from '@/features/admin/components/AdminAiPage';
import AdminAccessPage from '@/features/admin/components/AdminAccessPage';
import AdminAuditPage from '@/features/admin/components/AdminAuditPage';
import AdminQueuePage from '@/features/admin/components/AdminQueuePage';
import AdminUsersPage from '@/features/admin/components/AdminUsersPage';
import AdminSystemPage from '@/features/admin/components/AdminSystemPage';

/**
 * Admin router (§3.4 of the 0.5.0 UX plan). The flat eight-tab `?tab=` page is
 * dissolved into nested, bookmarkable routes under the role-gated `/admin`:
 *
 *   /admin                    → redirect to /admin/libraries
 *   /admin/libraries          → catalog (per-library backup/restore/AI/delete are row actions)
 *   /admin/queue              → global ingest queue
 *   /admin/audit              → global audit log (filter by workspace / library)
 *   /admin/ai                 → AI configuration (+ wizard, Pricing folded into Catalog)
 *   /admin/access             → API tokens
 *   /admin/users              → user management (gated users.manage)
 *   /admin/system             → instance settings, theme/locale, MCP connect guide, API explorer
 *
 * Every legacy admin capability stays reachable; only the organization changes.
 * Each section owns its own `PageShell` + `PageHeader`, the same page skeleton
 * every non-admin route uses, so the whole app reads as one product.
 */
export default function AdminPage() {
  const { can } = useCan();

  return (
    <Routes>
      <Route path="libraries" element={<AdminLibrariesPage />} />
      {can('queue.manage') && <Route path="queue" element={<AdminQueuePage />} />}
      <Route path="audit" element={<AdminAuditPage />} />
      {can('ai.configure') && <Route path="ai" element={<AdminAiPage />} />}
      <Route path="access" element={<AdminAccessPage />} />
      {can('users.manage') && <Route path="users" element={<AdminUsersPage />} />}
      {can('system.manage') && <Route path="system" element={<AdminSystemPage />} />}

      {/* Absolute targets — a relative `to="libraries"` from the catch-all at
          e.g. /admin/users would resolve to /admin/users/libraries, never
          match, and re-hit the catch-all in an infinite redirect loop. */}
      <Route index element={<Navigate to="/admin/libraries" replace />} />
      <Route path="*" element={<Navigate to="/admin/libraries" replace />} />
    </Routes>
  );
}
