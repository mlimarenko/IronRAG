import { useCallback, useMemo } from 'react'

import { useApp } from '@/shared/contexts/app-context'
import type { User } from '@/shared/types'

/**
 * Role → capability gating for the UI shell and every feature surface.
 *
 * This is the single client-side source of truth for the owner-confirmed
 * role/capability matrix in the 0.5.0 UX plan (§8 Locked Decisions). The
 * rest of the app gates affordances off `useCan()` / `useRole()` rather than
 * re-deriving `role === 'admin'` checks inline, so the policy lives in one
 * place and stays consistent.
 *
 * IMPORTANT: these checks are presentational only — they decide what the UI
 * renders, not what the server allows. Every capability here must also be
 * enforced by the backend on the corresponding endpoint; the UI gate is a
 * usability layer, never a security boundary.
 */

type Role = User['role'] // 'admin' | 'operator' | 'viewer'

/**
 * Capability keys map 1:1 to rows of the §8 matrix. Grouped by domain:
 *   - `documents.*` / `graph.* ` / `assistant.*` — the read/ask surfaces
 *   - `library.*` — create/edit libraries and their content
 *   - `workspace.*` — workspace lifecycle
 *   - `admin.*` — the system/admin entry and its sub-surfaces
 *   - `users.*` — user management (admin → Users, a new surface)
 *   - `devmode.*` — who may flip developer mode
 */
export type Capability =
  // viewer+ (everyone authenticated)
  | 'documents.view'
  | 'graph.view'
  | 'assistant.ask'
  // operator+ — library authoring
  | 'library.create'
  | 'library.edit'
  | 'library.delete'
  | 'content.upload'
  | 'content.ingest'
  | 'content.edit'
  // admin only — system surfaces
  | 'workspace.manage'
  | 'admin.access'
  | 'system.manage'
  | 'ai.configure'
  | 'pricing.manage'
  | 'queue.manage'
  | 'users.manage'
  // developer mode is available to every signed-in user (per-user toggle)
  | 'devmode.toggle'

const VIEWER_CAPABILITIES: readonly Capability[] = [
  'documents.view',
  'graph.view',
  'assistant.ask',
  'devmode.toggle',
]

const OPERATOR_CAPABILITIES: readonly Capability[] = [
  ...VIEWER_CAPABILITIES,
  'library.create',
  'library.edit',
  'library.delete',
  'content.upload',
  'content.ingest',
  'content.edit',
]

const ADMIN_CAPABILITIES: readonly Capability[] = [
  ...OPERATOR_CAPABILITIES,
  'workspace.manage',
  'admin.access',
  'system.manage',
  'ai.configure',
  'pricing.manage',
  'queue.manage',
  'users.manage',
]

/**
 * The canonical role → capability sets. Adding a capability is a single-edit
 * change here; never branch on the raw role string elsewhere in the app.
 */
const ROLE_CAPABILITIES: Record<Role, ReadonlySet<Capability>> = {
  viewer: new Set(VIEWER_CAPABILITIES),
  operator: new Set(OPERATOR_CAPABILITIES),
  admin: new Set(ADMIN_CAPABILITIES),
}

/**
 * Returns the current user's role, defaulting to the least-privileged
 * `viewer` when there is no resolved session yet. Fail-closed: an unknown or
 * absent role never grants more than a viewer.
 */
function useRole(): Role {
  const { user } = useApp()
  return user?.role ?? 'viewer'
}

export interface UseCanResult {
  /** The resolved role for the current session (fail-closed to `viewer`). */
  role: Role
  /** True when the current role grants `capability`. */
  can: (capability: Capability) => boolean
  /** True when the role grants every listed capability. */
  canAll: (...capabilities: Capability[]) => boolean
  /** True when the role grants at least one listed capability. */
  canAny: (...capabilities: Capability[]) => boolean
  /** Convenience flags for the three tiers, for terse JSX guards. */
  isViewer: boolean
  isOperator: boolean
  isAdmin: boolean
}

/**
 * Primary capability hook. Example:
 *
 *   const { can, isAdmin } = useCan();
 *   if (can('library.edit')) { …render upload affordance… }
 *   {can('admin.access') && <AdminEntry />}
 */
export function useCan(): UseCanResult {
  const role = useRole()

  const granted = ROLE_CAPABILITIES[role] ?? ROLE_CAPABILITIES.viewer

  const can = useCallback((capability: Capability) => granted.has(capability), [granted])

  const canAll = useCallback(
    (...capabilities: Capability[]) => capabilities.every((c) => granted.has(c)),
    [granted],
  )

  const canAny = useCallback(
    (...capabilities: Capability[]) => capabilities.some((c) => granted.has(c)),
    [granted],
  )

  return useMemo(
    () => ({
      role,
      can,
      canAll,
      canAny,
      isViewer: role === 'viewer',
      isOperator: role === 'operator',
      isAdmin: role === 'admin',
    }),
    [role, can, canAll, canAny],
  )
}
