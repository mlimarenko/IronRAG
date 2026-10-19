import {
  BookOpen,
  Bot,
  FileText,
  KeyRound,
  LayoutDashboard,
  Network,
  Settings2,
  Shield,
  SlidersHorizontal,
  UserRound,
  Webhook,
  type LucideIcon,
} from 'lucide-react'

export type NavEntry = readonly [route: string, labelKey: string, icon: LucideIcon]
export type NavGroup = readonly [groupLabelKey: string, entries: readonly NavEntry[]]

export const mainNav: readonly NavEntry[] = [
  ['/dashboard', 'nav.dashboard', LayoutDashboard],
  ['/documents', 'nav.documents', FileText],
  ['/graph', 'nav.graph', Network],
  ['/assistant', 'nav.assistant', Bot],
]

/// Eight admin entries in one undifferentiated run told a first-time operator
/// nothing about which one answers their question. The groups name the job,
/// not the software layer: you go to "materials and processing" when a
/// document is missing, to "people and access" when someone cannot sign in,
/// and to "system settings" when the answers themselves need configuring.
export const adminNav: readonly NavGroup[] = [
  ['nav.adminContent', [['/admin/libraries', 'nav.libraries', BookOpen]]],
  [
    'nav.adminPeople',
    [
      ['/admin/users', 'nav.users', UserRound],
      ['/admin/access', 'nav.access', KeyRound],
    ],
  ],
  [
    'nav.adminSystem',
    [
      ['/admin/ai', 'nav.ai', Settings2],
      ['/admin/webhooks', 'nav.webhooks', Webhook],
      ['/admin/audit', 'nav.audit', Shield],
      ['/admin/system', 'nav.system', SlidersHorizontal],
    ],
  ],
]

/// The label key of the sidebar group a routed administration screen sits in,
/// or `null` for a path that is not in the sidebar. Screens use it for the
/// trail above their title, so that trail is the trail through the sidebar
/// rather than a section word the sidebar no longer shows.
export function adminGroupLabelKeyForPath(pathname: string): string | null {
  for (const [groupLabelKey, entries] of adminNav) {
    for (const [route] of entries) {
      if (pathname === route || pathname.startsWith(`${route}/`)) return groupLabelKey
    }
  }
  return null
}
