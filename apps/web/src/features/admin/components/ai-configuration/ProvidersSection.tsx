import { useTranslation } from 'react-i18next';

import { Badge } from '@/shared/components/ui/badge';
import type { AIProvider } from '@/shared/types';
import { badgeClass, matchesFilter, type AiConfigDataState } from '@/features/admin/model/aiConfig';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

type ProvidersSectionProps = {
  providersState: AiConfigDataState<AIProvider[]>;
};

function lifecycleTone(state: AIProvider['lifecycleState']) {
  return state === 'active' ? 'ready' : state === 'deprecated' ? 'failed' : 'warning';
}

export function ProvidersSection({ providersState }: ProvidersSectionProps) {
  const { t } = useTranslation();
  const providers = providersState.data ?? [];

  return (
    <EntityWorkbench<AIProvider>
      tableId="admin.ai.providers"
      title={t('admin.providers')}
      count={providers.length}
      state={providersState}
      rows={providers}
      rowKey={provider => provider.id}
      emptyMessage={t('admin.noProviders')}
      searchPlaceholder={t('admin.searchModels')}
      matchesFilter={(provider, filter) =>
        matchesFilter([provider.displayName, provider.kind, provider.apiStyle], filter)
      }
      columns={[
        {
          key: 'name',
          header: t('admin.providers'),
          sortValue: provider => provider.displayName,
          cell: provider => (
            <div className="min-w-0">
              <div className="truncate text-sm font-semibold">{provider.displayName}</div>
              <div className="truncate text-[11px] text-muted-foreground">
                {provider.kind}
                {provider.apiStyle ? ` · ${provider.apiStyle}` : ''}
              </div>
            </div>
          ),
        },
        {
          key: 'models',
          header: t('admin.aiPanel.metrics.visibleModels'),
          width: 'w-28',
          align: 'right',
          sortValue: provider => provider.modelCount,
          cell: provider => (
            <span className="tabular-nums text-xs text-muted-foreground">{provider.modelCount}</span>
          ),
        },
        {
          key: 'credentials',
          header: t('admin.credentials'),
          width: 'w-28',
          align: 'right',
          sortValue: provider => provider.credentialCount,
          cell: provider => (
            <span className="tabular-nums text-xs text-muted-foreground">{provider.credentialCount}</span>
          ),
        },
        {
          key: 'state',
          header: t('admin.status'),
          width: 'w-32',
          sortValue: provider => provider.lifecycleState,
          cell: provider => {
            const tone = lifecycleTone(provider.lifecycleState);
            const cls = tone === 'ready'
              ? 'text-muted-foreground'
              : tone === 'failed'
                ? 'text-status-failed'
                : 'text-status-warning';
            return <span className={`text-xs ${cls}`}>{provider.lifecycleState}</span>;
          },
        },
      ]}
      renderInspector={provider => ({
        row: provider,
        title: provider.displayName,
        subtitle: provider.kind,
        body: (
          <>
            <div>
              <span className={`status-badge ${badgeClass(lifecycleTone(provider.lifecycleState))}`}>
                {provider.lifecycleState}
              </span>
            </div>
            <InspectorSection title={t('admin.aiPanel.fields.apiStyle')}>
              <InspectorField
                label={t('admin.aiPanel.fields.apiStyle')}
                value={provider.apiStyle || '—'}
                mono
              />
              <InspectorField
                label={t('admin.aiPanel.metrics.visibleModels')}
                value={provider.modelCount}
              />
              <InspectorField
                label={t('admin.credentials')}
                value={provider.credentialCount}
              />
            </InspectorSection>
            <InspectorSection title={t('admin.aiPanel.fields.requirements')}>
              <div className="flex flex-wrap gap-2">
                <Badge variant="outline">
                  {provider.apiKeyRequired
                    ? t('admin.apiKey')
                    : t('admin.aiPanel.fields.tokenOptional')}
                </Badge>
                {provider.baseUrlRequired && (
                  <Badge variant="outline">{t('admin.aiPanel.fields.baseUrl')}</Badge>
                )}
                <Badge variant="outline">
                  {t('admin.aiPanel.fields.discovery')}: {provider.modelDiscovery.mode}
                </Badge>
              </div>
            </InspectorSection>
            {provider.defaultBaseUrl && (
              <InspectorSection title={t('admin.aiPanel.fields.baseUrl')}>
                <div className="rounded-md border border-border/70 bg-surface-sunken p-3 font-mono text-xs [overflow-wrap:anywhere]">
                  {provider.defaultBaseUrl}
                </div>
              </InspectorSection>
            )}
            <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
              <InspectorField label="ID" value={provider.id} mono />
            </InspectorSection>
          </>
        ),
      })}
    />
  );
}
