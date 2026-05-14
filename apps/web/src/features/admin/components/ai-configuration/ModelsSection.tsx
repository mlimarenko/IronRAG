import { useTranslation } from 'react-i18next';

import { Badge } from '@/shared/components/ui/badge';
import type { AIModelOption, AIProvider } from '@/shared/types';
import {
  formatModelLabel,
  matchesFilter,
  purposeLabel,
  type AiConfigDataState,
} from '@/features/admin/model/aiConfig';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';

type ModelsSectionProps = {
  modelsState: AiConfigDataState<AIModelOption[]>;
  providers: AIProvider[];
};

export function ModelsSection({ modelsState, providers }: ModelsSectionProps) {
  const { t } = useTranslation();
  const models = modelsState.data ?? [];
  const providerById = new Map(providers.map(p => [p.id, p]));

  return (
    <EntityWorkbench<AIModelOption>
      tableId="admin.ai.models"
      title={t('admin.aiPanel.metrics.visibleModels')}
      count={models.length}
      state={modelsState}
      rows={models}
      rowKey={model => model.id}
      emptyMessage={t('admin.aiPanel.empty.noSelectableModels')}
      searchPlaceholder={t('admin.searchModels')}
      matchesFilter={(model, filter) =>
        matchesFilter(
          [
            model.modelName,
            model.capabilityKind,
            model.modalityKind,
            formatModelLabel(model, providers),
            model.allowedBindingPurposes.map(p => purposeLabel(p, t)).join(', '),
          ],
          filter,
        )
      }
      columns={[
        {
          key: 'name',
          header: t('admin.model'),
          sortValue: model => model.modelName,
          cell: model => {
            const provider = providerById.get(model.providerCatalogId);
            const unavailable = model.availabilityState === 'unavailable';
            return (
              <div className="min-w-0">
                <div
                  className={`truncate text-sm font-semibold ${unavailable ? 'text-muted-foreground line-through' : ''}`}
                  title={model.modelName}
                >
                  {model.modelName}
                </div>
                <div className="truncate text-[11px] text-muted-foreground">
                  {provider?.displayName ?? '—'}
                  {model.capabilityKind ? ` · ${model.capabilityKind}` : ''}
                </div>
              </div>
            );
          },
        },
        {
          key: 'modality',
          header: t('admin.aiPanel.fields.modality'),
          width: 'w-32',
          sortValue: model => model.modalityKind,
          cell: model => (
            <span className="text-xs text-muted-foreground">{model.modalityKind || '—'}</span>
          ),
        },
        {
          key: 'purposes',
          header: t('admin.aiPanel.fields.purposes'),
          sortValue: model => model.allowedBindingPurposes.map(p => purposeLabel(p, t)).join(', '),
          cell: model => {
            const labels = model.allowedBindingPurposes.map(p => purposeLabel(p, t));
            const text = labels.length === 0 ? '—' : labels.join(', ');
            return (
              <span className="block truncate text-xs text-muted-foreground" title={text}>
                {text}
              </span>
            );
          },
        },
      ]}
      renderInspector={model => {
        const provider = providerById.get(model.providerCatalogId);
        const unavailable = model.availabilityState === 'unavailable';
        return {
          row: model,
          title: model.modelName,
          subtitle: provider?.displayName,
          body: (
            <>
              {unavailable && (
                <div className="rounded-md border border-status-warning/25 bg-status-warning/5 p-3 text-sm text-status-warning">
                  {t('admin.aiPanel.unavailableBadge')}
                </div>
              )}
              <InspectorSection title={t('admin.aiPanel.fields.profile')}>
                <InspectorField
                  label={t('admin.aiPanel.fields.capability')}
                  value={model.capabilityKind}
                  mono
                />
                <InspectorField
                  label={t('admin.aiPanel.fields.modality')}
                  value={model.modalityKind || '—'}
                />
                {model.contextWindow != null && (
                  <InspectorField
                    label={t('admin.aiPanel.fields.contextWindow')}
                    value={model.contextWindow.toLocaleString()}
                  />
                )}
                {model.maxOutputTokens != null && (
                  <InspectorField
                    label={t('admin.maxOutputTokens')}
                    value={model.maxOutputTokens.toLocaleString()}
                  />
                )}
              </InspectorSection>
              <InspectorSection title={t('admin.aiPanel.fields.purposes')}>
                <div className="flex flex-wrap gap-1.5">
                  {model.allowedBindingPurposes.length === 0 ? (
                    <span className="text-xs text-muted-foreground">—</span>
                  ) : (
                    model.allowedBindingPurposes.map(purpose => (
                      <Badge key={purpose} variant="outline">
                        {purposeLabel(purpose, t)}
                      </Badge>
                    ))
                  )}
                </div>
              </InspectorSection>
              <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                <InspectorField label="ID" value={model.id} mono />
                <InspectorField
                  label={t('admin.aiPanel.fields.providerId')}
                  value={model.providerCatalogId}
                  mono
                />
              </InspectorSection>
            </>
          ),
        };
      }}
    />
  );
}
