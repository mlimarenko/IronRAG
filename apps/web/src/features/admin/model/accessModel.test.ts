import { describe, expect, it } from 'vitest'
import type { TokenResponse } from '@/shared/api/generated'
import {
  dateInputFromExpiresAt,
  expiresAtFromDateInput,
  mapAdminToken,
  permissionsForScope,
} from './accessModel'

const ACTIVE_TOKEN: TokenResponse = {
  principalId: 'token-1',
  label: 'Automation',
  tokenPrefix: 'inrg_example',
  status: 'active',
  expiresAt: '2026-08-08T00:00:00.000Z',
  scope: { kind: 'workspace', libraries: [] },
  grants: [],
}

describe('accessModel', () => {
  it('derives expiration on the client without changing the wire status', () => {
    expect(mapAdminToken(ACTIVE_TOKEN, Date.parse('2026-08-09T00:00:00.000Z')).status).toBe(
      'expired',
    )
    expect(ACTIVE_TOKEN.status).toBe('active')
  })

  it('never offers iam_admin for workspace-scoped tokens', () => {
    expect(permissionsForScope('workspace')).not.toContain('iam_admin')
    expect(permissionsForScope('library')).toContain('iam_admin')
  })

  it('converts editable expiry dates consistently', () => {
    expect(expiresAtFromDateInput('2027-01-02')).toBe('2027-01-02T00:00:00.000Z')
    expect(expiresAtFromDateInput('')).toBeNull()
    expect(dateInputFromExpiresAt('2027-01-02T11:00:00.000Z')).toBe('2027-01-02')
  })
})
