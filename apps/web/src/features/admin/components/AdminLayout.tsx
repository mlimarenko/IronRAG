import type { ReactNode } from 'react';
import { useTranslation } from 'react-i18next';
import { NavLink, useLocation } from 'react-router-dom';
import {
  Brain,
  Database,
  Key,
  ListOrdered,
  Settings,
  Sparkles,
  Users,
  type LucideIcon,
} from 'lucide-react';

import { useApp } from '@/shared/contexts/app-context';
import { useCan, type Capability } from '@/shared/auth/useCan';

type AdminSectionDescriptor = {
  /** Route segment under `/admin`, e.g. `libraries`. */
  segment: string;
  /** i18n key (relative to `admin.nav`) for the label. */
  labelKey: string;
  icon: LucideIcon;
  /** Capability that gates the entry. All sections require `admin.access`. */
  capability: Capability;
  /**
   * Other `/admin/*` segments whose detail pages belong to this section, so
   * the nav stays highlighted when the user drills into a child route (e.g.
   * the Library Hub at `/admin/library/:id` keeps the Libraries entry active).
   */
  alsoMatch?: string[];
};

/**
 * Canonical admin section model (§3.4 of the 0.5.0 UX plan). The flat eight-tab
 * AdminPage is dissolved into these routed sections; every legacy capability is
 * preserved and reachable, just reorganized around the library-centric model.
 */
const ADMIN_SECTIONS: AdminSectionDescriptor[] = [
  {
    segment: 'libraries',
    labelKey: 'libraries',
    icon: Database,
    capability: 'admin.access',
    alsoMatch: ['library'],
  },
  { segment: 'queue', labelKey: 'queue', icon: ListOrdered, capability: 'queue.manage' },
  { segment: 'ai', labelKey: 'ai', icon: Brain, capability: 'ai.configure' },
  { segment: 'access', labelKey: 'access', icon: Key, capability: 'admin.access' },
  { segment: 'users', labelKey: 'users', icon: Users, capability: 'users.manage' },
  { segment: 'system', labelKey: 'system', icon: Settings, capability: 'system.manage' },
];

type AdminLayoutProps = {
  children: ReactNode;
};

/**
 * Shared chrome for every admin section: the page header plus a left section
 * rail (collapsing to a horizontal scroll strip on small screens). The active
 * surface renders through the router `<Outlet/>` passed in as `children`.
 */
export function AdminLayout({ children }: AdminLayoutProps) {
  const { t } = useTranslation();
  const { activeWorkspace } = useApp();
  const { can } = useCan();
  const location = useLocation();

  const sections = ADMIN_SECTIONS.filter((section) => can(section.capability));
  const currentSegment = location.pathname.split('/')[2] ?? 'libraries';

  return (
    <div className="flex flex-1 min-h-0 flex-col overflow-hidden">
      <div className="page-header flex items-center gap-3">
        <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-primary/10 text-primary">
          <Sparkles className="h-4 w-4" />
        </div>
        <div className="min-w-0">
          <h1 className="text-lg font-bold tracking-tight">{t('admin.title')}</h1>
          <p className="truncate text-sm text-muted-foreground">
            {activeWorkspace?.name ?? t('admin.nav.subtitle')}
          </p>
        </div>
      </div>

      <div className="flex flex-1 min-h-0 flex-col lg:flex-row">
        {/* Section rail — vertical on lg+, horizontal scroll strip below. */}
        <nav
          aria-label={t('admin.nav.label')}
          className="shrink-0 border-b lg:w-56 lg:border-b-0 lg:border-r"
        >
          <div className="flex gap-1 overflow-x-auto p-2 lg:flex-col lg:overflow-visible lg:p-3">
            {sections.map((section) => {
              const Icon = section.icon;
              const isActive =
                currentSegment === section.segment ||
                (section.alsoMatch?.includes(currentSegment) ?? false);
              return (
                <NavLink
                  key={section.segment}
                  to={`/admin/${section.segment}`}
                  aria-current={isActive ? 'page' : undefined}
                  className={`flex shrink-0 items-center gap-2.5 rounded-xl px-3 py-2 text-sm font-semibold transition-colors ${
                    isActive
                      ? 'bg-primary text-primary-foreground shadow-sm'
                      : 'text-muted-foreground hover:bg-accent/50 hover:text-foreground'
                  }`}
                >
                  <Icon className="h-4 w-4 shrink-0" />
                  <span className="whitespace-nowrap">{t(`admin.nav.${section.labelKey}`)}</span>
                </NavLink>
              );
            })}
          </div>
        </nav>

        <div className="flex flex-1 min-h-0 flex-col overflow-auto animate-fade-in">
          {children}
        </div>
      </div>
    </div>
  );
}
