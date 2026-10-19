import type { TokenResponse } from '../../generated'

/** More than one table page, including status and non-name search targets. */
export default function iamTokens(): TokenResponse[] {
  const initialStatuses = ['active', 'revoked', 'expired'] as const
  return Array.from({ length: 31 }, (_, index) => ({
    principalId: `grid-token-${index}`,
    label: index === 30 ? 'Needle token' : `Row token ${String(index).padStart(2, '0')}`,
    tokenPrefix: 'mock',
    grants: [],
    scope: {
      kind: 'workspace',
      workspace: { id: 'workspace-alpha', displayName: 'Default Workspace' },
      libraries: [],
    },
    status: initialStatuses[index] ?? 'active',
    revokedAt: index === 1 ? '2026-09-29T12:00:00Z' : null,
    expiresAt: index === 30 ? '2037-11-12T12:00:00Z' : null,
    lastUsedAt: null,
  }))
}
