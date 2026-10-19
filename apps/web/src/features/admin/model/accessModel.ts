import type { IamPermissionKind, TokenResponse } from '@/shared/api/generated'
import type { APIToken } from '@/shared/types'
import { mapToken } from './adminAdapter'

export type TokenScope = 'workspace' | 'library'
export type TokenExpiry = '30' | '90' | '365' | 'never'

const IAM_PERMISSIONS: readonly IamPermissionKind[] = [
  'iam_admin',
  'workspace_admin',
  'workspace_read',
  'library_read',
  'library_write',
  'document_read',
  'document_write',
  'connector_admin',
  'credential_admin',
  'binding_admin',
  'query_run',
  'ops_read',
  'audit_read',
]

export function permissionsForScope(scope: TokenScope): readonly IamPermissionKind[] {
  return scope === 'workspace'
    ? IAM_PERMISSIONS.filter((permission) => permission !== 'iam_admin')
    : IAM_PERMISSIONS
}

export function mapAdminToken(raw: TokenResponse, now = Date.now()): APIToken {
  const token = mapToken(raw)
  if (
    token.status === 'active' &&
    token.expiresAt !== undefined &&
    new Date(token.expiresAt).getTime() < now
  ) {
    return { ...token, status: 'expired' }
  }
  return token
}

export function expiresAtFromDateInput(value: string): string | null {
  return value ? new Date(`${value}T00:00:00.000Z`).toISOString() : null
}

export function dateInputFromExpiresAt(value: string | undefined): string {
  return value?.slice(0, 10) ?? ''
}
