// Which workspace and library a *person* last worked in.
//
// This is a preference of the signed-in principal, not of the browser, so the
// storage key carries the principal id. A device-wide key would hand the
// previous user's library to whoever signs in next on the same machine.

export interface ScopePreference {
  workspaceId?: string
  libraryId?: string
}

const SCOPE_STORAGE_PREFIX = 'ironrag.v1.scope.'

function keyFor(principalId: string): string {
  return `${SCOPE_STORAGE_PREFIX}${principalId}`
}

function asId(value: unknown): string | undefined {
  return typeof value === 'string' && value.length > 0 ? value : undefined
}

export function readScopePreference(principalId: string): ScopePreference {
  if (!principalId) return {}
  try {
    const raw = localStorage.getItem(keyFor(principalId))
    if (raw === null) return {}
    const parsed: unknown = JSON.parse(raw)
    if (typeof parsed !== 'object' || parsed === null) return {}
    const record = parsed as Record<string, unknown>
    const workspaceId = asId(record.workspaceId)
    const libraryId = asId(record.libraryId)
    return {
      ...(workspaceId ? { workspaceId } : {}),
      ...(libraryId ? { libraryId } : {}),
    }
  } catch {
    // Unreadable or corrupt storage must degrade to "no preference", never
    // block sign-in.
    return {}
  }
}

export function writeScopePreference(principalId: string, scope: ScopePreference): void {
  if (!principalId) return
  try {
    if (!scope.workspaceId && !scope.libraryId) {
      localStorage.removeItem(keyFor(principalId))
      return
    }
    localStorage.setItem(keyFor(principalId), JSON.stringify(scope))
  } catch {
    // A full or disabled storage costs the user a remembered selection, not
    // the session.
  }
}
