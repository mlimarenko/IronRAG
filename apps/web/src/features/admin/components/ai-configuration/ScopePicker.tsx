import { useTranslation } from 'react-i18next';

import type { AIScopeKind } from '@/shared/types';

type ScopePickerProps = {
  selectedScope: AIScopeKind;
  activeWorkspaceName?: string | undefined;
  activeLibraryName?: string | undefined;
  onScopeChange: (scope: AIScopeKind) => void;
};

export function ScopePicker({
  selectedScope,
  activeWorkspaceName,
  activeLibraryName,
  onScopeChange,
}: ScopePickerProps) {
  const { t } = useTranslation();
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
  ];

  return (
    <div className="grid grid-cols-3 gap-1 rounded-md border border-border/70 bg-surface-sunken p-1 shadow-sm xl:min-w-[520px]">
      {scopes.map(scope => (
        <button
          key={scope.kind}
          type="button"
          disabled={scope.disabled}
          onClick={() => onScopeChange(scope.kind)}
          aria-pressed={selectedScope === scope.kind}
          className={`min-h-9 min-w-0 rounded-md px-2 py-1.5 text-center text-xs font-semibold transition sm:px-3 sm:text-left sm:text-sm ${
            selectedScope === scope.kind
              ? 'bg-primary text-primary-foreground shadow-sm'
              : 'text-muted-foreground hover:bg-muted/60'
          } ${scope.disabled ? 'cursor-not-allowed opacity-40' : ''}`}
          title={scope.title}
        >
          <span className="block truncate">{scope.title}</span>
        </button>
      ))}
    </div>
  );
}
