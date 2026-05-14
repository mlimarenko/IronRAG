import { useMemo, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { Plus } from 'lucide-react';
import { adminApi, adminModelCatalogOptions, queries } from '@/shared/api';
import { Button } from '@/shared/components/ui/button';
import {
  Dialog,
  DialogContent,
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

  const [createOpen, setCreateOpen] = useState(false);
  const [pricingModelId, setPricingModelId] = useState('');
  const [pricingBillingUnit, setPricingBillingUnit] = useState('');
  const [pricingUnitPrice, setPricingUnitPrice] = useState('');
  const [pricingCurrency, setPricingCurrency] = useState('USD');
  const [pricingFrom, setPricingFrom] = useState('');
  const [pricingTo, setPricingTo] = useState('');
  const [saving, setSaving] = useState(false);

  const providersQuery = useQuery({
    ...queries.listAiProvidersOptions(),
    enabled: active,
  });
  const modelsQuery = useQuery({
    ...adminModelCatalogOptions(),
    enabled: active,
  });
  const pricesQuery = useQuery({
    queryKey: ADMIN_PRICES_QUERY_KEY,
    queryFn: () => adminApi.listPrices(),
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

  const handleCreate = () => {
    if (
      !activeWorkspaceId ||
      !pricingModelId ||
      !pricingBillingUnit ||
      !pricingUnitPrice ||
      !pricingFrom
    ) {
      return;
    }
    setSaving(true);
    adminApi
      .createPriceOverride({
        workspaceId: activeWorkspaceId,
        modelCatalogId: pricingModelId,
        billingUnit: pricingBillingUnit,
        unitPrice: pricingUnitPrice,
        currencyCode: pricingCurrency,
        effectiveFrom: new Date(pricingFrom).toISOString(),
        effectiveTo: pricingTo ? new Date(pricingTo).toISOString() : null,
      })
      .then(() => {
        toast.success(t('admin.pricingOverrideCreated'));
        setCreateOpen(false);
        setPricingModelId('');
        setPricingBillingUnit('');
        setPricingUnitPrice('');
        setPricingCurrency('USD');
        setPricingFrom('');
        setPricingTo('');
        void queryClient.invalidateQueries({ queryKey: ADMIN_PRICES_QUERY_KEY });
      })
      .catch((err: unknown) => toast.error(errorMessage(err, t('admin.createPricingFailed'))))
      .finally(() => setSaving(false));
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
      <Button size="sm" onClick={() => setCreateOpen(true)}>
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
        renderInspector={rule => ({
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
              </InspectorSection>
              <InspectorSection title={t('admin.source')}>
                <InspectorField label={t('admin.source')} value={rule.sourceOrigin} />
              </InspectorSection>
              <InspectorSection title={t('admin.aiPanel.fields.identifier')}>
                <InspectorField label="ID" value={rule.id} mono />
              </InspectorSection>
            </>
          ),
        })}
      />

      <Dialog
        open={createOpen}
        onOpenChange={v => {
          setCreateOpen(v);
          if (!v) {
            setPricingModelId('');
            setPricingBillingUnit('');
            setPricingUnitPrice('');
            setPricingCurrency('USD');
            setPricingFrom('');
            setPricingTo('');
          }
        }}
      >
        <DialogContent className="max-w-md">
          <DialogHeader>
            <DialogTitle>{t('admin.addPricingOverride')}</DialogTitle>
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
                  placeholder="0.00"
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
            <Button variant="outline" onClick={() => setCreateOpen(false)}>
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
              onClick={handleCreate}
            >
              {saving ? t('admin.saving') : t('admin.save')}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
