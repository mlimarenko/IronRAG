import { memo, useCallback, useMemo } from 'react';
import type { TFunction } from 'i18next';
import { Eye, EyeOff, Layers, RotateCcw } from 'lucide-react';
import { GRAPH_NODE_COLORS } from '@/components/graph/config';
import { NO_SUBTYPE_KEY, type TypeLegendMap } from './typeLegend';

const SUBTYPE_PREVIEW_LIMIT = 12;

function subtypeLegendLabel(t: TFunction, subType: string): string {
  return subType === NO_SUBTYPE_KEY ? t('graph.noSubType') : subType;
}

type GraphLegendProps = {
  t: TFunction;
  legend: TypeLegendMap;
  legendOpen: boolean;
  setLegendOpen: (open: boolean) => void;
  hiddenTypes: Set<string>;
  setHiddenTypes: (updater: (prev: Set<string>) => Set<string>) => void;
  hiddenSubTypes: Set<string>;
  setHiddenSubTypes: (updater: (prev: Set<string>) => Set<string>) => void;
  expandedSubtypeGroups: Set<string>;
  setExpandedSubtypeGroups: (updater: (prev: Set<string>) => Set<string>) => void;
};

function GraphLegendImpl({
  t,
  legend,
  legendOpen,
  setLegendOpen,
  hiddenTypes,
  setHiddenTypes,
  hiddenSubTypes,
  setHiddenSubTypes,
  expandedSubtypeGroups,
  setExpandedSubtypeGroups,
}: GraphLegendProps) {
  const handleShowAll = useCallback(() => {
    setHiddenTypes(() => new Set());
    setHiddenSubTypes(() => new Set());
  }, [setHiddenTypes, setHiddenSubTypes]);

  const handleInvert = useCallback(() => {
    setHiddenTypes((prev) => {
      const allTypes = Object.keys(GRAPH_NODE_COLORS);
      const next = new Set<string>();
      for (const tp of allTypes) {
        if (!prev.has(tp)) next.add(tp);
      }
      return next;
    });
  }, [setHiddenTypes]);

  const handleTypeClick = useCallback(
    (type: string, meta: boolean) => {
      if (meta) {
        setHiddenTypes((prev) => {
          const next = new Set(prev);
          if (next.has(type)) next.delete(type);
          else next.add(type);
          return next;
        });
        return;
      }
      const allTypes = Object.keys(GRAPH_NODE_COLORS);
      const othersHidden = allTypes.filter((t2) => t2 !== type).every((t2) => hiddenTypes.has(t2));
      if (othersHidden && !hiddenTypes.has(type)) {
        setHiddenTypes(() => new Set());
        setHiddenSubTypes(() => new Set());
      } else {
        setHiddenTypes(() => new Set(allTypes.filter((t2) => t2 !== type)));
        setHiddenSubTypes(() => new Set());
      }
    },
    [hiddenTypes, setHiddenTypes, setHiddenSubTypes],
  );

  const handleSubtypeClick = useCallback(
    (type: string, sub: string, siblingKeys: string[], meta: boolean) => {
      const subKey = `${type}:${sub}`;
      if (meta) {
        setHiddenSubTypes((prev) => {
          const next = new Set(prev);
          if (next.has(subKey)) next.delete(subKey);
          else next.add(subKey);
          return next;
        });
        return;
      }
      if (siblingKeys.length === 1) {
        setHiddenSubTypes((prev) => {
          const next = new Set(prev);
          if (next.has(subKey)) next.delete(subKey);
          else next.add(subKey);
          return next;
        });
        return;
      }
      const othersHidden = siblingKeys
        .filter((k) => k !== subKey)
        .every((k) => hiddenSubTypes.has(k));
      if (othersHidden && !hiddenSubTypes.has(subKey)) {
        setHiddenSubTypes((prev) => {
          const next = new Set(prev);
          for (const k of siblingKeys) next.delete(k);
          return next;
        });
      } else {
        setHiddenSubTypes((prev) => {
          const next = new Set(prev);
          for (const k of siblingKeys) {
            if (k === subKey) next.delete(k);
            else next.add(k);
          }
          return next;
        });
      }
    },
    [hiddenSubTypes, setHiddenSubTypes],
  );

  const toggleGroupExpanded = useCallback(
    (type: string) => {
      setExpandedSubtypeGroups((prev) => {
        const next = new Set(prev);
        if (next.has(type)) next.delete(type);
        else next.add(type);
        return next;
      });
    },
    [setExpandedSubtypeGroups],
  );

  // Pre-compute the entries list once per legend change so the render pass
  // only walks it.
  const typeEntries = useMemo(() => Object.entries(GRAPH_NODE_COLORS), []);

  if (!legendOpen) {
    return (
      <button
        onClick={() => setLegendOpen(true)}
        className="absolute top-3 left-3 glass-panel rounded-xl p-2 shadow-lifted cursor-pointer hover:bg-white/10 transition-all"
        title={t('graph.showLegend')}
      >
        <Layers className="h-4 w-4 text-muted-foreground" />
      </button>
    );
  }

  return (
    <div className="absolute top-3 left-3 bottom-3 max-h-[calc(100%-24px)] overflow-y-auto text-xs glass-panel rounded-xl shadow-lifted min-w-[150px] max-w-[250px] flex flex-col">
      <div className="flex items-center gap-1 px-3 py-2 border-b border-white/10">
        <span className="text-[11px] font-semibold text-muted-foreground uppercase tracking-wider flex-1">
          {t('graph.legend')}
        </span>
        <button
          onClick={handleShowAll}
          className="p-1 rounded hover:bg-white/10 cursor-pointer transition-colors"
          title={t('graph.showAll')}
        >
          <Eye className="h-3.5 w-3.5 text-muted-foreground" />
        </button>
        <button
          onClick={handleInvert}
          className="p-1 rounded hover:bg-white/10 cursor-pointer transition-colors"
          title={t('graph.invert')}
        >
          <RotateCcw className="h-3.5 w-3.5 text-muted-foreground" />
        </button>
        <button
          onClick={() => setLegendOpen(false)}
          className="p-1 rounded hover:bg-white/10 cursor-pointer transition-colors"
          title={t('graph.hideLegend')}
        >
          <EyeOff className="h-3.5 w-3.5 text-muted-foreground" />
        </button>
      </div>

      <div className="px-2 py-1.5 flex-1 overflow-y-auto">
        {typeEntries.map(([type, color]) => {
          const isHidden = hiddenTypes.has(type);
          const stats = legend.get(type);
          const count = stats?.count ?? 0;
          if (count === 0 && type !== 'document') return null;
          const realSubs = stats?.subs
            ? Array.from(stats.subs.entries()).sort((a, b) => b[1] - a[1])
            : [];
          const subs =
            stats && stats.noSubtypeCount > 0 && realSubs.length > 0
              ? [...realSubs, [NO_SUBTYPE_KEY, stats.noSubtypeCount] as const]
              : realSubs;
          const isSubtypeGroupExpanded = expandedSubtypeGroups.has(type);
          const visibleSubs = isSubtypeGroupExpanded ? subs : subs.slice(0, SUBTYPE_PREVIEW_LIMIT);
          const hiddenSubtypeCount = Math.max(0, subs.length - SUBTYPE_PREVIEW_LIMIT);
          const siblingKeys = subs.map(([s]) => `${type}:${s}`);
          return (
            <div key={type} className={`mb-0.5 ${isHidden ? 'opacity-35' : ''}`}>
              <button
                className={`flex items-center gap-1.5 w-full px-2 py-1 rounded-md transition-all cursor-pointer ${
                  isHidden ? 'line-through' : 'hover:bg-white/10'
                }`}
                onClick={(e) => handleTypeClick(type, e.ctrlKey || e.metaKey)}
                title={t(`graph.nodeTypes.${type}`)}
              >
                <span
                  className="w-2.5 h-2.5 rounded-full shrink-0"
                  style={{ background: color }}
                />
                <span className="font-semibold truncate">{t(`graph.nodeTypes.${type}`)}</span>
                <span className="ml-auto tabular-nums text-muted-foreground">{count}</span>
              </button>
              {subs.length > 0 && !isHidden && (
                <div className="pl-6 pr-1 mt-0.5 mb-1">
                  <div className="flex flex-wrap gap-x-1.5 gap-y-0.5">
                    {visibleSubs.map(([sub, subCount]) => {
                      const subKey = `${type}:${sub}`;
                      const isSubHidden = hiddenSubTypes.has(subKey);
                      return (
                        <button
                          key={sub}
                          className={`text-[10px] whitespace-nowrap cursor-pointer rounded px-1 py-0.5 transition-colors ${
                            isSubHidden
                              ? 'opacity-35 line-through text-muted-foreground'
                              : 'text-muted-foreground hover:bg-white/10'
                          }`}
                          onClick={(e) => {
                            e.stopPropagation();
                            handleSubtypeClick(type, sub, siblingKeys, e.ctrlKey || e.metaKey);
                          }}
                          title={subtypeLegendLabel(t, sub)}
                        >
                          <span
                            className="inline-block w-1.5 h-1.5 rounded-full mr-0.5 align-middle"
                            style={{ background: color, opacity: 0.6 }}
                          />
                          {subtypeLegendLabel(t, sub)}{' '}
                          <span className="tabular-nums">{subCount}</span>
                        </button>
                      );
                    })}
                  </div>
                  {subs.length > SUBTYPE_PREVIEW_LIMIT && (
                    <button
                      type="button"
                      className="mt-1 inline-flex h-6 items-center rounded-md px-1.5 text-[10px] font-medium text-muted-foreground transition-colors hover:bg-white/10 hover:text-foreground"
                      onClick={(e) => {
                        e.stopPropagation();
                        toggleGroupExpanded(type);
                      }}
                    >
                      {isSubtypeGroupExpanded
                        ? t('graph.hideSubTypes')
                        : t('graph.showAllSubTypes', { count: hiddenSubtypeCount })}
                    </button>
                  )}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

export const GraphLegend = memo(GraphLegendImpl);
