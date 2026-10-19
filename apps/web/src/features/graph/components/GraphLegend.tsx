import { Eye, EyeOff } from 'lucide-react'
import { memo, useMemo, useState, type Dispatch, type MouseEvent, type SetStateAction } from 'react'
import { useTranslation } from 'react-i18next'
import { humanizeToken } from '@/shared/lib/enumLabels'
import { GRAPH_NODE_COLORS } from '../model/config'
import {
  NO_SUBTYPE_KEY,
  isolateLegendSubType,
  isolateLegendType,
  legendSubtypes,
  subtypeFilterKey,
  toggleLegendSubType,
  toggleLegendType,
  type LegendFilter,
  type TypeLegendMap,
} from '../model/typeLegend'

type GraphLegendProps = Readonly<{
  legend: TypeLegendMap
  filter: LegendFilter
  onFilterChange: Dispatch<SetStateAction<LegendFilter>>
  onShowAll: () => void
}>

const DEFAULT_VISIBLE_SUBTYPES = 6

/** Ctrl (or ⌘ on macOS) inverts a legend click into "show only this". */
function isIsolateClick(event: MouseEvent): boolean {
  return event.ctrlKey || event.metaKey
}

function GraphLegendImpl({ legend, filter, onFilterChange, onShowAll }: GraphLegendProps) {
  const { t } = useTranslation()
  const [expandedTypes, setExpandedTypes] = useState<Set<string>>(new Set())
  const types = useMemo(
    () =>
      Array.from(legend.entries())
        .filter(([, stats]) => stats.count > 0)
        .sort((left, right) => right[1].count - left[1].count)
        .map(([type]) => type),
    [legend],
  )

  return (
    <aside className="graph-legend" aria-label={t('graph.legend')}>
      <div className="legend-heading">
        <strong>{t('graph.legend')}</strong>
        <button type="button" onClick={onShowAll}>
          {t('graph.showAll')}
        </button>
      </div>
      <p className="graph-legend__hint">{t('graph.legendHint')}</p>
      <div className="graph-legend__list">
        {types.map((type) => {
          const stats = legend.get(type)
          const isHidden = filter.hiddenTypes.has(type)
          const subTypes = legendSubtypes(stats)
          const isExpanded = expandedTypes.has(type)
          const visibleSubTypes = isExpanded
            ? subTypes
            : subTypes.slice(0, DEFAULT_VISIBLE_SUBTYPES)
          const hiddenSubtypeCount = subTypes.length - visibleSubTypes.length

          return (
            <div className={`graph-legend__group ${isHidden ? 'is-hidden' : ''}`} key={type}>
              <button
                className="graph-legend__type"
                type="button"
                aria-pressed={!isHidden}
                onClick={(event) => {
                  const isolate = isIsolateClick(event)
                  onFilterChange((current) =>
                    isolate
                      ? isolateLegendType(current, legend, type)
                      : toggleLegendType(current, type),
                  )
                }}
              >
                <span
                  className="node-swatch"
                  style={{ backgroundColor: GRAPH_NODE_COLORS[type] ?? GRAPH_NODE_COLORS.entity }}
                />
                <span>{t(`graph.nodeTypes.${type}`)}</span>
                <small>{stats?.count ?? 0}</small>
                {isHidden ? <EyeOff size={12} /> : <Eye size={12} />}
              </button>
              {!isHidden && subTypes.length > 0 && (
                <div className="graph-legend__subtypes">
                  {visibleSubTypes.map(([subType, count]) => {
                    const key = subtypeFilterKey(type, subType)
                    const hidden = filter.hiddenSubTypes.has(key)
                    return (
                      <button
                        type="button"
                        className={hidden ? 'is-hidden' : ''}
                        aria-pressed={!hidden}
                        key={key}
                        onClick={(event) => {
                          const isolate = isIsolateClick(event)
                          onFilterChange((current) =>
                            isolate
                              ? isolateLegendSubType(current, legend, type, key)
                              : toggleLegendSubType(current, key),
                          )
                        }}
                      >
                        <span>
                          {subType === NO_SUBTYPE_KEY
                            ? t('graph.noSubType')
                            : humanizeToken(subType)}
                        </span>
                        <small>{count}</small>
                      </button>
                    )
                  })}
                  {hiddenSubtypeCount > 0 && (
                    <button
                      type="button"
                      className="graph-legend__more"
                      onClick={() => setExpandedTypes((current) => new Set(current).add(type))}
                    >
                      {t('graph.moreSubtypes', { count: hiddenSubtypeCount })}
                    </button>
                  )}
                  {isExpanded && subTypes.length > DEFAULT_VISIBLE_SUBTYPES && (
                    <button
                      type="button"
                      className="graph-legend__more"
                      onClick={() =>
                        setExpandedTypes((current) => {
                          const next = new Set(current)
                          next.delete(type)
                          return next
                        })
                      }
                    >
                      {t('graph.showLess')}
                    </button>
                  )}
                </div>
              )}
            </div>
          )
        })}
      </div>
    </aside>
  )
}

export const GraphLegend = memo(GraphLegendImpl)
