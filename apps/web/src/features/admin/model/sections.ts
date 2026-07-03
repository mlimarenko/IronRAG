import {
  Brain,
  Database,
  Key,
  ListOrdered,
  ScrollText,
  Settings,
  Users,
  type LucideIcon,
} from 'lucide-react';

import type { Capability } from '@/shared/auth/useCan';

/**
 * Admin sub-nav groups, rendered as captioned clusters in the sidebar (label
 * from `admin.nav.group.<group>`) so the seven sections read as a scannable
 * hierarchy instead of one flat list. Order here is the render order.
 */
export type AdminSectionGroup = 'content' | 'intelligence' | 'governance' | 'instance';

export const ADMIN_SECTION_GROUPS: AdminSectionGroup[] = [
  'content',
  'intelligence',
  'governance',
  'instance',
];

export type AdminSectionDescriptor = {
  /** Route segment under `/admin`, e.g. `libraries`. */
  segment: string;
  /** i18n key (relative to `admin.nav`) for the label. */
  labelKey: string;
  icon: LucideIcon;
  /** Capability that gates the entry. All sections require `admin.access`. */
  capability: Capability;
  /** Sidebar cluster this section belongs to. */
  group: AdminSectionGroup;
  /**
   * Other `/admin/*` segments whose detail pages belong to this section, so
   * the nav stays highlighted when the user drills into a child route.
   */
  alsoMatch?: string[];
};

export const ADMIN_SECTIONS: AdminSectionDescriptor[] = [
  { segment: 'libraries', labelKey: 'libraries', icon: Database, capability: 'admin.access', group: 'content' },
  { segment: 'queue', labelKey: 'queue', icon: ListOrdered, capability: 'queue.manage', group: 'content' },
  { segment: 'audit', labelKey: 'audit', icon: ScrollText, capability: 'admin.access', group: 'content' },
  { segment: 'ai', labelKey: 'ai', icon: Brain, capability: 'ai.configure', group: 'intelligence' },
  { segment: 'access', labelKey: 'access', icon: Key, capability: 'admin.access', group: 'governance' },
  { segment: 'users', labelKey: 'users', icon: Users, capability: 'users.manage', group: 'governance' },
  { segment: 'system', labelKey: 'system', icon: Settings, capability: 'system.manage', group: 'instance' },
];
