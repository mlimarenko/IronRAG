import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { DollarSign } from 'lucide-react';

import { Badge } from '@/shared/components/ui/badge';
import { Button } from '@/shared/components/ui/button';
import type { AIModelOption, AIProvider, PricingRule } from '@/shared/types';
import {
  formatModelLabel,
  formatModelPriceSuffix,
  matchesFilter,
  purposeLabel,
  resolveModelPriceSummary,
  type AiConfigDataState,
} from '@/features/admin/model/aiConfig';

import { EntityWorkbench, InspectorField, InspectorSection } from './EntityWorkbench';
import { PriceOverrideDialog } from './PriceOverrideDialog';

type ModelsSectionProps = {
  modelsState: AiConfigDataState<AIModelOption[]>;
  providers: AIProvider[];
  prices: PricingRule[];
  activeWorkspaceId: string | undefined;
  invalidateAll: () => void;
};

export function ModelsSection({ modelsState, providers, prices, activeWorkspaceId, invalidateAll }: ModelsSectionProps) {
  const { t } = useTranslation();
  const providerById = useMemo(() => new Map(providers.map(p => [p.id, p])), [providers]);
  const [priceDialogModel, setPriceDialogModel] = useState<AIModelOption | null>(null);
  const models = modelsState.data ?? [];

  const priceCell = (model: AIModelOption) => {
    const suffix = formatModelPriceSuffix(resolveModelPriceSummary(model.id, prices));
    return (
      <div className="flex items-center justify-end gap-2">
        <span className="tabular-nums text-xs text-muted-foreground">{suffix || '—'}</span>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="h-8 w-8"
          aria-label={`${t('admin.aiPanel.actions.overridePrice')}: ${model.modelName}`}
          onClick={() => setPriceDialogModel(model)}
        >
          <DollarSign className="h-3.5 w-3.5" />
        </Button>
      </div>
    );
  };

  return (
    <div className="flex flex-1 min-h-0 flex-col">
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
                  <div className="truncate text-2xs text-muted-foreground">
                    {provider?.displayName ?? '—'}
                    {model.capabilityKind ? ` · ${model.capabilityKind}` : ''}
                  </div>
                </div>
              );
            },
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
          {
            key: 'price',
            header: t('admin.aiPanel.fields.pricePerMillion'),
            width: 'w-44',
            align: 'right',
            cell: priceCell,
          },
        ]}
        renderInspector={model => {
          const provider = providerById.get(model.providerCatalogId);
          const unavailable = model.availabilityState === 'unavailable';
          const priceSuffix = formatModelPriceSuffix(resolveModelPriceSummary(model.id, prices));
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
                <InspectorSection title={t('admin.aiPanel.fields.pricePerMillion')}>
                  <InspectorField
                    label={t('admin.aiPanel.fields.pricePerMillion')}
                    value={priceSuffix || '—'}
                    mono
                  />
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
                  <InspectorField label={t('admin.aiPanel.fields.identifier')} value={model.id} mono />
                  <InspectorField
                    label={t('admin.aiPanel.fields.providerId')}
                    value={model.providerCatalogId}
                    mono
                  />
                </InspectorSection>
              </>
            ),
            actions: (
              <Button size="sm" variant="outline" onClick={() => setPriceDialogModel(model)}>
                <DollarSign className="mr-1.5 h-3.5 w-3.5" /> {t('admin.aiPanel.actions.overridePrice')}
              </Button>
            ),
          };
        }}
      />

      <PriceOverrideDialog
        open={priceDialogModel !== null}
        onOpenChange={open => { if (!open) setPriceDialogModel(null); }}
        model={priceDialogModel}
        prices={prices}
        activeWorkspaceId={activeWorkspaceId}
        invalidatePrices={invalidateAll}
      />
    </div>
  );
}
