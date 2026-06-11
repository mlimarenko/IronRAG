import { Navigate, Route, Routes } from 'react-router-dom';

import { useCan } from '@/shared/auth/useCan';
import { AdminLayout } from '@/features/admin/components/AdminLayout';
import AdminLibrariesPage from '@/features/admin/components/AdminLibrariesPage';
import LibraryHubPage from '@/features/admin/components/LibraryHubPage';
import AdminAiPage from '@/features/admin/components/AdminAiPage';
import AdminAccessPage from '@/features/admin/components/AdminAccessPage';
import AdminQueuePage from '@/features/admin/components/AdminQueuePage';
import AdminUsersPage from '@/features/admin/components/AdminUsersPage';
import AdminSystemPage from '@/features/admin/components/AdminSystemPage';

/**
 * Admin router (§3.4 of the 0.5.0 UX plan). The flat eight-tab `?tab=` page is
 * dissolved into nested, bookmarkable routes under the role-gated `/admin`:
 *
 *   /admin                    → redirect to /admin/libraries
 *   /admin/libraries          → catalog (rows → Library Hub)
 *   /admin/library/:libraryId → Library Hub (Overview · Activity · Backup · MCP · Configure AI)
 *   /admin/queue              → global ingest queue
 *   /admin/ai                 → AI configuration (+ wizard, Pricing folded into Catalog)
 *   /admin/access             → API tokens
 *   /admin/users              → user management (gated users.manage)
 *   /admin/system             → instance settings, theme/locale, version, API explorer
 *
 * Every legacy admin capability stays reachable; only the organization changes.
 * The Library Hub renders full-bleed (its own header), so it sits outside the
 * shared `AdminLayout` chrome; every other section renders inside the layout's
 * section rail.
 */
export default function AdminPage() {
  const { can } = useCan();

  return (
    <Routes>
      {/* Library Hub is a full-bleed detail route — no shared section rail. */}
      <Route path="library/:libraryId" element={<LibraryHubPage />} />

      <Route
        path="libraries"
        element={
          <AdminLayout>
            <AdminLibrariesPage />
          </AdminLayout>
        }
      />
      {can('queue.manage') && (
        <Route
          path="queue"
          element={
            <AdminLayout>
              <AdminQueuePage />
            </AdminLayout>
          }
        />
      )}
      {can('ai.configure') && (
        <Route
          path="ai"
          element={
            <AdminLayout>
              <AdminAiPage />
            </AdminLayout>
          }
        />
      )}
      <Route
        path="access"
        element={
          <AdminLayout>
            <AdminAccessPage />
          </AdminLayout>
        }
      />
      {can('users.manage') && (
        <Route
          path="users"
          element={
            <AdminLayout>
              <AdminUsersPage />
            </AdminLayout>
          }
        />
      )}
      {can('system.manage') && (
        <Route
          path="system"
          element={
            <AdminLayout>
              <AdminSystemPage />
            </AdminLayout>
          }
        />
      )}

      {/* Absolute targets — a relative `to="libraries"` from the catch-all at
          e.g. /admin/users would resolve to /admin/users/libraries, never
          match, and re-hit the catch-all in an infinite redirect loop. */}
      <Route index element={<Navigate to="/admin/libraries" replace />} />
      <Route path="*" element={<Navigate to="/admin/libraries" replace />} />
    </Routes>
  );
}
