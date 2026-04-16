import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { TFunction } from 'i18next';
import { toast } from 'sonner';
import { ArrowDown, ArrowUp, ArrowUpDown, Loader2, Plus, Search } from 'lucide-react';
import { adminApi } from '@/api';
import { Button } from '@/components/ui/button';
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@/components/ui/select';
import { mapModelList, mapProviderList } from '@/adapters/ai';
import { mapPricing } from '@/adapters/admin';
import { errorMessage } from '@/lib/errorMessage';
import type { AIModelOption, AIProvider, PricingRule } from '@/types';

type PricingTabProps = {
  t: TFunction;
  activeWorkspaceId: string | undefined;
  active: boolean;
};

export function PricingTab({ t, activeWorkspaceId, active }: PricingTabProps) {
  const [providers, setProviders] = useState<AIProvider[]>([]);
  const [modelOptions, setModelOptions] = useState<AIModelOption[]>([]);
  const [pricing, setPricing] = useState<PricingRule[]>([]);
  const [loading, setLoading] = useState(false);
  const [pricingSearch, setPricingSearch] = useState('');
  const [pricingProvider, setPricingProvider] = useState('all');
  // Track whether the provider/model catalog has been fetched at least once
  // so the tab-activation effect does not re-fetch forever when the backend
  // legitimately returns empty catalogs. `providers.length === 0` is not a
  // safe "cold" signal because the fetched catalog can also be empty.
  const catalogLoadedRef = useRef(false);

  const [createOpen, setCreateOpen] = useState(false);
  const [pricingModelId, setPricingModelId] = useState('');
  const [pricingBillingUnit, setPricingBillingUnit] = useState('');
  const [pricingUnitPrice, setPricingUnitPrice] = useState('');
  const [pricingCurrency, setPricingCurrency] = useState('USD');
  const [pricingFrom, setPricingFrom] = useState('');
  const [pricingTo, setPricingTo] = useState('');
  const [saving, setSaving] = useState(false);

  type PricingSortKey = 'provider' | 'model' | 'billingUnit' | 'unitPrice' | 'effectiveFrom' | 'sourceOrigin';
  const [sortKey, setSortKey] = useState<PricingSortKey>('provider');
  const [sortAsc, setSortAsc] = useState(true);

  const toggleSort = (key: PricingSortKey) => {
    if (sortKey === key) {
      setSortAsc((prev) => !prev);
    } else {
      setSortKey(key);
      setSortAsc(true);
    }
  };

  const loadPricing = useCallback(
    (providerList: AIProvider[], modelList: AIModelOption[]) => {
      setLoading(true);
      adminApi
        .listPrices()
        .then((data) => {
          const list = Array.isArray(data) ? data : [];
          setPricing(list.map((p) => mapPricing(p, providerList, modelList)));
        })
        .catch((err: unknown) =>
          toast.error(errorMessage(err, t('admin.loadPricingFailed'))),
        )
        .finally(() => setLoading(false));
    },
    [t],
  );

  useEffect(() => {
    if (!active) return;
    let cancelled = false;

    void (async () => {
      if (catalogLoadedRef.current) {
        loadPricing(providers, modelOptions);
        return;
      }
      try {
        const [provRaw, modelRaw] = await Promise.all([
          adminApi.listProviders(),
          adminApi.listModels(),
        ]);
        if (cancelled) return;
        const provList = mapProviderList(provRaw);
        const modelList = mapModelList(modelRaw);
        catalogLoadedRef.current = true;
        setProviders(provList);
        setModelOptions(modelList);
        loadPricing(provList, modelList);
      } catch {
        if (cancelled) return;
        // Fetch failed — still mark the catalog as "attempted" so the
        // tab does not retry in a tight loop. The operator can hit
        // Refresh (via re-activating the tab) to retry.
        catalogLoadedRef.current = true;
        loadPricing(providers, modelOptions);
      }
    })();

    return () => {
      cancelled = true;
    };
    // `providers` and `modelOptions` intentionally excluded — they are used
    // via the `catalogLoadedRef` guard, not as fetch triggers. Including
    // them would re-fire the effect after the initial fetch's setState calls.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active, loadPricing]);

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
        loadPricing(providers, modelOptions);
      })
      .catch((err: unknown) =>
        toast.error(errorMessage(err, t('admin.createPricingFailed'))),
      )
      .finally(() => setSaving(false));
  };

  const filteredPricing = useMemo(() => {
    const filtered = pricing.filter((p) => {
      if (pricingProvider !== 'all' && p.provider !== pricingProvider) return false;
      if (pricingSearch && !p.model.toLowerCase().includes(pricingSearch.toLowerCase())) {
        return false;
      }
      return true;
    });
    const dir = sortAsc ? 1 : -1;
    filtered.sort((a, b) => {
      let cmp: number;
      if (sortKey === 'unitPrice') {
        cmp = a.unitPrice - b.unitPrice;
      } else {
        const av = a[sortKey] ?? '';
        const bv = b[sortKey] ?? '';
        cmp = av.localeCompare(bv, undefined, { numeric: true, sensitivity: 'base' });
      }
      return cmp * dir;
    });
    return filtered;
  }, [pricing, pricingProvider, pricingSearch, sortKey, sortAsc]);

  return (
    <>
      <div className="flex items-center justify-between mb-5">
        <h2 className="text-base font-bold tracking-tight">{t('admin.pricing')}</h2>
        <div className="flex gap-2">
          <Select value={pricingProvider} onValueChange={setPricingProvider}>
            <SelectTrigger className="h-9 w-36 text-sm">
              <SelectValue placeholder={t('admin.provider')} />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="all">{t('admin.allProviders')}</SelectItem>
              {providers.map((p) => (
                <SelectItem key={p.id} value={p.displayName}>
                  {p.displayName}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
          <div className="relative">
            <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" />
            <Input
              className="h-9 pl-9 w-48 text-sm"
              placeholder={t('admin.searchModels')}
              value={pricingSearch}
              onChange={(e) => setPricingSearch(e.target.value)}
            />
          </div>
          <Button size="sm" variant="outline" onClick={() => setCreateOpen(true)}>
            <Plus className="h-3.5 w-3.5 mr-1.5" /> {t('admin.override')}
          </Button>
        </div>
      </div>
      {loading ? (
        <div className="flex items-center gap-2 text-sm text-muted-foreground p-4">
          <Loader2 className="h-4 w-4 animate-spin" /> {t('admin.loadingPricing')}
        </div>
      ) : (
        <div className="workbench-surface overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b text-left">
                {([
                  ['provider', t('admin.provider')],
                  ['model', t('admin.model')],
                  ['billingUnit', t('admin.billingUnit')],
                  ['unitPrice', t('admin.price')],
                  ['effectiveFrom', t('admin.effectiveFrom')],
                  ['sourceOrigin', t('admin.source')],
                ] as const).map(([key, label]) => {
                  const active = sortKey === key;
                  const Icon = active ? (sortAsc ? ArrowUp : ArrowDown) : ArrowUpDown;
                  return (
                    <th
                      key={key}
                      className="px-4 py-3 section-label cursor-pointer select-none hover:text-foreground transition-colors"
                      onClick={() => toggleSort(key)}
                    >
                      <span className="inline-flex items-center gap-1.5">
                        {label}
                        <Icon className={`h-3 w-3 ${active ? 'text-foreground' : 'text-muted-foreground/50'}`} />
                      </span>
                    </th>
                  );
                })}
              </tr>
            </thead>
            <tbody>
              {filteredPricing.map((p) => (
                <tr key={p.id} className="border-b hover:bg-accent/30 transition-colors">
                  <td className="px-4 py-3.5 font-semibold">{p.provider}</td>
                  <td className="px-4 py-3.5 font-mono text-xs font-bold">{p.model}</td>
                  <td className="px-4 py-3.5 text-xs text-muted-foreground font-medium">
                    {p.billingUnit.replace(/_/g, ' ')}
                  </td>
                  <td className="px-4 py-3.5 tabular-nums font-bold">
                    ${p.unitPrice.toFixed(2)} {p.currency}
                  </td>
                  <td className="px-4 py-3.5 text-muted-foreground text-xs">
                    {p.effectiveFrom}
                  </td>
                  <td className="px-4 py-3.5 text-xs text-muted-foreground font-medium">
                    {p.sourceOrigin}
                  </td>
                </tr>
              ))}
              {filteredPricing.length === 0 && (
                <tr>
                  <td colSpan={6} className="text-center p-8 text-sm text-muted-foreground">
                    {t('admin.noPricingData')}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
      )}

      <Dialog
        open={createOpen}
        onOpenChange={(v) => {
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
                  {modelOptions.map((m) => (
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
                  <SelectItem value="per_1m_input_tokens">
                    {t('admin.per1mInputTokens')}
                  </SelectItem>
                  <SelectItem value="per_1m_cached_input_tokens">
                    {t('admin.per1mCachedInputTokens')}
                  </SelectItem>
                  <SelectItem value="per_1m_output_tokens">
                    {t('admin.per1mOutputTokens')}
                  </SelectItem>
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
                  onChange={(e) => setPricingUnitPrice(e.target.value)}
                />
              </div>
              <div>
                <Label>{t('admin.currency')}</Label>
                <Input
                  className="mt-2"
                  value={pricingCurrency}
                  onChange={(e) => setPricingCurrency(e.target.value)}
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
                  onChange={(e) => setPricingFrom(e.target.value)}
                />
              </div>
              <div>
                <Label>{t('admin.effectiveTo')}</Label>
                <Input
                  type="date"
                  className="mt-2"
                  value={pricingTo}
                  onChange={(e) => setPricingTo(e.target.value)}
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
    </>
  );
}
