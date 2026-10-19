import type { TFunction } from 'i18next'

import { getApiProblem } from '@/shared/api'
import type { AIScopeKind } from '@/shared/types'
import type { AiScopeContext } from '../model/aiConfig'

export function errorText(error: unknown, fallback: string, t: TFunction) {
  const problem = getApiProblem(error)
  if (problem?.status === 409) return t('admin.aiPanel.messages.resourceInUse')
  if (error instanceof Error && error.message) return error.message
  return fallback
}

export function scopeBody(scope: AIScopeKind, context: AiScopeContext) {
  return {
    scopeKind: scope,
    ...(scope !== 'instance' && context.workspaceId ? { workspaceId: context.workspaceId } : {}),
    ...(scope === 'library' && context.libraryId ? { libraryId: context.libraryId } : {}),
  }
}

export function parseJsonObject(value: string): Record<string, unknown> | undefined {
  if (!value.trim()) return undefined
  const parsed = JSON.parse(value) as unknown
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new Error('json')
  return parsed as Record<string, unknown>
}
