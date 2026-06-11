import { useMemo, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { Loader2, Pencil, Plus, Trash2 } from 'lucide-react';
import { adminApi, adminModelCatalogOptions, queries } from '@/shared/api';
import { Button } from '@/shared/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/shared/components/ui/dialog';
import { Input } from '@/shared/components/ui/input';
import { Label } from '@/shared/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/shared/components/ui/select';
import { mapModelList, mapProviderList } from '@/features/admin/model/aiAdapter';
import { mapPricing } from '@/features/admin/model/adminAdapter';
import { errorMessage } from '@/shared/lib/errorMessage';
import type { AIModelOption, AIProvider, PricingRule } from '@/shared/types';
import { matchesFilter } from '@/features/admin/model/aiConfig';

import {
  EntityWorkbench,
  InspectorField,
  InspectorSection,
} from '@/features/admin/components/ai-configuration/EntityWorkbench';

type PricingTabProps = {
  t: TFunction;
  activeWorkspaceId: string | undefined;
  active: boolean;
};

const ADMIN_PRICES_QUERY_KEY = ['admin', 'ai', 'prices'] as const;

export function PricingTab({ t, activeWorkspaceId, active }: PricingTabProps) {
  const queryClient = useQueryClient();

  const [pricingProvider, setPricingProvider] = useState('all');

  const [editorOpen, setEditorOpen] = useState(false);
  const [editingRule, setEditingRule] = useState<PricingRule | null>(null);
  const [deleteTarget, setDeleteTarget] = useState<PricingRule | null>(null);
  const [pricingModelId, setPricingModelId] = useState('');
  const [pricingBillingUnit, setPricingBillingUnit] = useState('');
  const [pricingUnitPrice, setPricingUnitPrice] = useState('');
  const [pricingCurrency, setPricingCurrency] = useState('USD');
  const [pricingFrom, setPricingFrom] = useState('');
  const [pricingTo, setPricingTo] = useState('');
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState(false);

  const providersQuery = useQuery({
    ...queries.listAiProvidersOptions(),
    enabled: active,
  });
  const modelsQuery = useQuery({
    ...adminModelCatalogOptions(),
    enabled: active,
  });
  const pricesQuery = useQuery({
    queryKey: [...ADMIN_PRICES_QUERY_KEY, activeWorkspaceId ?? null],
    queryFn: () => adminApi.listPrices(activeWorkspaceId ? { workspaceId: activeWorkspaceId } : {}),
    enabled: active,
  });

  const providers = useMemo<AIProvider[]>(
    () => mapProviderList(providersQuery.data),
    [providersQuery.data],
  );
  const modelOptions = useMemo<AIModelOption[]>(
    () => mapModelList(modelsQuery.data),
    [modelsQuery.data],
  );
  const pricing = useMemo<PricingRule[]>(() => {
    const list = Array.isArray(pricesQuery.data) ? pricesQuery.data : [];
    return list.map(p => mapPricing(p, providers, modelOptions));
  }, [pricesQuery.data, providers, modelOptions]);

  const isLoading =
    (providersQuery.isLoading || modelsQuery.isLoading || pricesQuery.isLoading) && active;
  const queryError =
    providersQuery.error ?? modelsQuery.error ?? pricesQuery.error ?? null;

  const filteredPricing = useMemo(() => {
    const filtered =
      pricingProvider === 'all'
        ? pricing
        : pricing.filter(p => p.provider === pricingProvider);
    return filtered.slice().sort((a, b) => {
      const providerDelta = a.provider.localeCompare(b.provider, undefined, {
        numeric: true,
        sensitivity: 'base',
      });
      if (providerDelta !== 0) return providerDelta;
      const modelDelta = a.model.localeCompare(b.model, undefined, {
        numeric: true,
        sensitivity: 'base',
      });
      if (modelDelta !== 0) return modelDelta;
      return a.billingUnit.localeCompare(b.billingUnit);
    });
  }, [pricing, pricingProvider]);

  const resetPricingEditor = () => {
    setEditorOpen(false);
    setEditingRule(null);
    setPricingModelId('');
    setPricingBillingUnit('');
    setPricingUnitPrice('');
    setPricingCurrency('USD');
    setPricingFrom('');
    setPricingTo('');
  };

  const openNewPricingEditor = () => {
    setEditingRule(null);
    setPricingModelId('');
    setPricingBillingUnit('');
    setPricingUnitPrice('');
    setPricingCurrency('USD');
    setPricingFrom('');
    setPricingTo('');
    setEditorOpen(true);
  };

  const openPricingEditor = (rule: PricingRule) => {
    setEditingRule(rule);
    setPricingModelId(rule.modelCatalogId);
    setPricingBillingUnit(rule.billingUnit);
    setPricingUnitPrice(String(rule.unitPrice));
    setPricingCurrency(rule.currency);
    setPricingFrom(rule.effectiveFrom);
    setPricingTo(rule.effectiveTo ?? '');
    setEditorOpen(true);
  };

  const handleSave = () => {
    if (
      (!editingRule && !activeWorkspaceId) ||
      !pricingModelId ||
      !pricingBillingUnit ||
      !pricingUnitPrice ||
      !pricingFrom
    ) {
      return;
    }
    setSaving(true);
    const body = {
      modelCatalogId: pricingModelId,
      billingUnit: pricingBillingUnit,
      unitPrice: pricingUnitPrice,
      currencyCode: pricingCurrency,
      effectiveFrom: new Date(pricingFrom).toISOString(),
      effectiveTo: pricingTo ? new Date(pricingTo).toISOString() : null,
    };
    const mutation = editingRule
      ? adminApi.updatePriceOverride(editingRule.id, body)
      : adminApi.createPriceOverride({
          workspaceId: activeWorkspaceId as string,
          ...body,
        });
    mutation
      .then(() => {
        toast.success(
          editingRule
            ? t('admin.pricingOverrideUpdated')
            : t('admin.pricingOverrideCreated'),
        );
        resetPricingEditor();
        void queryClient.invalidateQueries({ queryKey: ADMIN_PRICES_QUERY_KEY });
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.savePricingFailed'))))
      .finally(() => setSaving(false));
  };

  const deletePriceOverride = async () => {
    if (!deleteTarget || deleting) return;
    setDeleting(true);
    try {
      await adminApi.deletePriceOverride(deleteTarget.id);
      setDeleteTarget(null);
      toast.success(t('admin.pricingOverrideDeleted'));
      void queryClient.invalidateQueries({ queryKey: ADMIN_PRICES_QUERY_KEY });
    } catch (err) {
      toast.error(errorMessage(err, t('admin.deletePricingFailed')));
    } finally {
      setDeleting(false);
    }
  };

  const toolbar = (
    <>
      <Select value={pricingProvider} onValueChange={setPricingProvider}>
        <SelectTrigger className="h-9 w-[180px] text-sm">
          <SelectValue placeholder={t('admin.provider')} />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="all">{t('admin.allProviders')}</SelectItem>
          {providers.map(p => (
            <SelectItem key={p.id} value={p.displayName}>
              {p.displayName}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Button size="sm" onClick={openNewPricingEditor} disabled={!activeWorkspaceId}>
        <Plus className="mr-1.5 h-3.5 w-3.5" /> {t('admin.override')}
      </Button>
    </>
  );

  return (
    <div className="flex flex-1 min-h-0 flex-col">
      <EntityWorkbench<PricingRule>
        tableId="admin.pricing"
        title={t('admin.pricing')}
        count={filteredPricing.length}
        state={{ isLoading, error: queryError, data: filteredPricing }}
        rows={filteredPricing}
        rowKey={rule => rule.id}
        emptyMessage={t('admin.noPricingData')}
        searchPlaceholder={t('admin.searchModels')}
        toolbar={toolbar}
        rowActions={rule => {
          const editable = rule.sourceOrigin === 'workspace_override' && rule.workspaceId === activeWorkspaceId;
          return (
            <>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="h-8 w-8"
                disabled={!editable}
                aria-label={`${t('admin.edit')}: ${rule.model}`}
                onClick={() => openPricingEditor(rule)}
              >
                <Pencil className="h-3.5 w-3.5" />
              </Button>
              <Button
                type="button"
                size="icon"
                variant="ghost"
                className="h-8 w-8 text-status-failed hover:text-status-failed"
                disabled={!editable}
                aria-label={`${t('admin.delete')}: ${rule.model}`}
                onClick={() => setDeleteTarget(rule)}
              >
                <Trash2 className="h-3.5 w-3.5" />
              </Button>
            </>
          );
        }}
        matchesFilter={(rule, filter) =>
          matchesFilter([rule.provider, rule.model, rule.billingUnit, rule.sourceOrigin], filter)
        }
        columns={[
          {
            key: 'provider',
            header: t('admin.provider'),
            width: 'w-40',
            sortValue: rule => rule.provider,
            cell: rule => <span className="text-sm font-semibold">{rule.provider}</span>,
          },
          {
            key: 'model',
            header: t('admin.model'),
            sortValue: rule => rule.model,
            cell: rule => (
              <span className="block truncate font-mono text-xs font-bold" title={rule.model}>
                {rule.model}
              </span>
            ),
          },
          {
            key: 'billingUnit',
            header: t('admin.billingUnit'),
            width: 'w-56',
            sortValue: rule => rule.billingUnit,
            cell: rule => (
              <span className="text-xs text-muted-foreground">
                {rule.billingUnit.replace(/_/g, ' ')}
              </span>
            ),
          },
          {
            key: 'price',
            header: t('admin.price'),
            width: 'w-32',
            align: 'right',
            sortValue: rule => rule.unitPrice,
            cell: rule => (
              <span className="tabular-nums text-sm font-semibold">
                ${rule.unitPrice.toFixed(2)} {rule.currency}
              </span>
            ),
          },
          {
            key: 'effectiveFrom',
            header: t('admin.effectiveFrom'),
            width: 'w-32',
            sortValue: rule => rule.effectiveFrom,
            cell: rule => (
              <span className="text-xs text-muted-foreground">{rule.effectiveFrom}</span>
            ),
          },
          {
            key: 'source',
            header: t('admin.source'),
            width: 'w-24',
            sortValue: rule => rule.sourceOrigin,
            cell: rule => (
              <span className="text-xs text-muted-foreground">{rule.sourceOrigin}</span>
            ),
          },
        ]}
        renderInspector={rule => {
          const editable = rule.sourceOrigin === 'workspace_override' && rule.workspaceId === activeWorkspaceId;
          return {
            row: rule,
            title: `${rule.provider} · ${rule.model}`,
            subtitle: rule.billingUnit.replace(/_/g, ' '),
            body: (
              <>
                <InspectorSection title={t('admin.price')}>
                  <InspectorField
                    label={t('admin.unitPrice')}
                    value={`$${rule.unitPrice.toFixed(2)} ${rule.currency}`}
                    mono
                  />
                  <InspectorField
                    label={t('admin.billingUnit')}
                    value={rule.billingUnit.replace(/_/g, ' ')}
                  />
                </InspectorSection>
                <InspectorSection title={t('admin.effectiveFrom')}>
                  <InspectorField label={t('admin.effectiveFrom')} value={rule.effectiveFrom} />
                  <InspectorField label={t('admin.effectiveTo')} value={rule.effectiveTo ?? '—'} />
                </InspectorSection>
                <InspectorSection title={t('admin.source')}>
                  <InspectorField label={t('admin.source')} value={rule.sourceOrigin} />
                </InspectorSection>
                <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                  <InspectorField label={t('admin.aiPanel.fields.identifier')} value={rule.id} mono />
                </InspectorSection>
              </>
            ),
            actions: editable ? (
              <div className="grid gap-2 sm:grid-cols-2">
                <Button size="sm" variant="outline" onClick={() => openPricingEditor(rule)}>
                  <Pencil className="mr-1.5 h-3.5 w-3.5" /> {t('admin.edit')}
                </Button>
                <Button size="sm" variant="outline" onClick={() => setDeleteTarget(rule)}>
                  <Trash2 className="mr-1.5 h-3.5 w-3.5" /> {t('admin.delete')}
                </Button>
              </div>
            ) : undefined,
          };
        }}
      />

      <Dialog
        open={editorOpen}
        onOpenChange={v => {
          if (!v) resetPricingEditor();
          else setEditorOpen(true);
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>
              {editingRule ? t('admin.editPricingOverride') : t('admin.addPricingOverride')}
            </DialogTitle>
            <DialogDescription>{t('admin.pricingOverrideDescription')}</DialogDescription>
          </DialogHeader>
          <div className="space-y-4">
            <div>
              <Label>{t('admin.model')}</Label>
              <Select value={pricingModelId} onValueChange={setPricingModelId}>
                <SelectTrigger className="mt-2">
                  <SelectValue placeholder={t('admin.selectModel')} />
                </SelectTrigger>
                <SelectContent>
                  {modelOptions.map(m => (
                    <SelectItem key={m.id} value={m.id}>
                      {m.modelName || m.id}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div>
              <Label>{t('admin.billingUnit')}</Label>
              <Select value={pricingBillingUnit} onValueChange={setPricingBillingUnit}>
                <SelectTrigger className="mt-2">
                  <SelectValue placeholder={t('admin.selectBillingUnit')} />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="per_1m_input_tokens">{t('admin.per1mInputTokens')}</SelectItem>
                  <SelectItem value="per_1m_cached_input_tokens">
                    {t('admin.per1mCachedInputTokens')}
                  </SelectItem>
                  <SelectItem value="per_1m_output_tokens">{t('admin.per1mOutputTokens')}</SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <Label>{t('admin.unitPrice')}</Label>
                <Input
                  type="number"
                  step="0.01"
                  placeholder={t('admin.aiPanel.placeholders.priceAmount')}
                  className="mt-2"
                  value={pricingUnitPrice}
                  onChange={e => setPricingUnitPrice(e.target.value)}
                />
              </div>
              <div>
                <Label>{t('admin.currency')}</Label>
                <Input
                  className="mt-2"
                  value={pricingCurrency}
                  onChange={e => setPricingCurrency(e.target.value)}
                />
              </div>
            </div>
            <div className="grid grid-cols-2 gap-3">
              <div>
                <Label>{t('admin.effectiveFrom')}</Label>
                <Input
                  type="date"
                  className="mt-2"
                  value={pricingFrom}
                  onChange={e => setPricingFrom(e.target.value)}
                />
              </div>
              <div>
                <Label>{t('admin.effectiveTo')}</Label>
                <Input
                  type="date"
                  className="mt-2"
                  value={pricingTo}
                  onChange={e => setPricingTo(e.target.value)}
                />
              </div>
            </div>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={resetPricingEditor}>
              {t('admin.cancel')}
            </Button>
            <Button
              disabled={
                !pricingModelId ||
                !pricingBillingUnit ||
                !pricingUnitPrice ||
                !pricingFrom ||
                saving
              }
              onClick={handleSave}
            >
              {saving ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={Boolean(deleteTarget)} onOpenChange={open => { if (!open) setDeleteTarget(null); }}>
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.deletePricingOverrideTitle')}</DialogTitle>
            <DialogDescription>
              {t('admin.deletePricingOverrideDescription', {
                model: deleteTarget?.model ?? '',
              })}
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => setDeleteTarget(null)}>
              {t('admin.cancel')}
            </Button>
            <Button variant="destructive" disabled={deleting} onClick={() => void deletePriceOverride()}>
              {deleting ? <><Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" /> {t('admin.saving')}</> : t('admin.delete')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
