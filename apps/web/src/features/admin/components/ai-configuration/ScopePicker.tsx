import { useTranslation } from 'react-i18next'

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select'
import type { AIScopeKind } from '@/shared/types'

type ScopePickerProps = Readonly<{
  selectedScope: AIScopeKind
  activeWorkspaceName?: string | undefined
  activeLibraryName?: string | undefined
  onScopeChange: (scope: AIScopeKind) => void
}>

export function ScopePicker({
  selectedScope,
  activeWorkspaceName,
  activeLibraryName,
  onScopeChange,
}: ScopePickerProps) {
  const { t } = useTranslation()
  const scopes: Array<{ kind: AIScopeKind; title: string; disabled: boolean }> = [
    {
      kind: 'instance',
      title: t('admin.aiPanel.scopeCards.instanceTitle'),
      disabled: false,
    },
    {
      kind: 'workspace',
      title: activeWorkspaceName ?? t('admin.aiPanel.scopeCards.workspaceTitle'),
      disabled: !activeWorkspaceName,
    },
    {
      kind: 'library',
      title: activeLibraryName ?? t('admin.aiPanel.scopeCards.libraryTitle'),
      disabled: !activeLibraryName,
    },
  ]

  return (
    <div className="min-w-0 rounded-lg bg-surface-sunken p-2 sm:min-w-[22rem]">
      <div className="mb-1 section-label">{t('admin.scope')}</div>
      <Select value={selectedScope} onValueChange={(value) => onScopeChange(value as AIScopeKind)}>
        <SelectTrigger className="h-9 w-full min-w-0 bg-card text-sm font-semibold">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {scopes.map((scope) => (
            <SelectItem key={scope.kind} value={scope.kind} disabled={scope.disabled}>
              {scope.title}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  )
}
