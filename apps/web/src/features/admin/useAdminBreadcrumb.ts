import { useTranslation } from 'react-i18next'
import { useLocation } from 'react-router-dom'

import { adminGroupLabelKeyForPath } from '@/app/navigation'

/// The trail shown above an administration screen's title. There is no
/// breadcrumb component in this shell — the line above the title plays that
/// role — and it used to read "Administration" on all eight screens, which
/// named a section the sidebar no longer shows and told the reader nothing
/// about where in it they were. Naming the sidebar group as well makes the
/// trail, the sidebar heading, and the command palette category one wording.
export function useAdminBreadcrumb(): string {
  const { t } = useTranslation()
  const { pathname } = useLocation()
  const groupLabelKey = adminGroupLabelKeyForPath(pathname)
  return groupLabelKey ? `${t('admin.title')} · ${t(groupLabelKey)}` : t('admin.title')
}
