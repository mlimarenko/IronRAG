import { useMemo, useState } from 'react'
import type { TFunction } from 'i18next'
import { useTranslation } from 'react-i18next'

import { DataGrid, useDataGridState, type GridColumn } from '@/shared/data-grid'
import { Badge } from '@/shared/ui'
import type { ModelPickerOption } from '../model/modelPicker'

type ModelPickerFieldProps = Readonly<{
  options: ModelPickerOption[]
  value: string
  onChange: (key: string) => void
  loading: boolean
}>

/**
 * Which models a filter chip keeps. `serves` is the one an operator wants
 * nine times out of ten: a router lists hundreds of models and only some can
 * take the job being bound. These are capability predicates, not column values,
 * so they stay the grid's caller-owned quick filters.
 */
type Facet = 'all' | 'serves' | 'vision' | 'in_catalog'

const FACETS: readonly Facet[] = ['serves', 'all', 'vision', 'in_catalog']

function matchesFacet(option: ModelPickerOption, facet: Facet): boolean {
  switch (facet) {
    case 'serves':
      return option.selectable
    case 'vision':
      return option.inputModalities.includes('image')
    case 'in_catalog':
      return Boolean(option.modelCatalogId)
    default:
      return true
  }
}

function formatTokens(value: number | undefined, locale: string): string {
  if (value === undefined) return '—'
  if (value >= 1000) return `${new Intl.NumberFormat(locale).format(Math.round(value / 1000))}K`
  return new Intl.NumberFormat(locale).format(value)
}

/**
 * A rate per million tokens. Trailing zeros come from a decimal column and say
 * nothing, so they go; a genuine zero stays as `0`, which is an honest price
 * for a model running on the operator's own hardware.
 */
function formatRate(value: string): string {
  if (!value.includes('.')) return value
  let end = value.length
  while (end > 0 && value[end - 1] === '0') end -= 1
  if (end > 0 && value[end - 1] === '.') end -= 1
  const trimmed = value.slice(0, end)
  return trimmed === '' ? '0' : trimmed
}

const modelOptionKey = (option: ModelPickerOption) => option.key

function modelNameCell(option: ModelPickerOption, t: TFunction) {
  return (
    <span className="model-picker-field__name">
      <strong>{option.modelName}</strong>
      {option.note && option.note !== 'not_in_catalog' && (
        <Badge tone={option.selectable ? 'neutral' : 'warning'}>
          {t(`admin.aiPanel.modelNotes.${option.note}`)}
        </Badge>
      )}
      {option.modelCatalogId && (
        <span className="model-picker-field__mark" title={t('admin.aiPanel.facets.in_catalog')}>
          {t('admin.aiPanel.facets.in_catalog')}
        </span>
      )}
    </span>
  )
}

/**
 * The model chooser.
 *
 * A native `<select>` could not do this job: a router offers four hundred
 * models, and a flat list of names with a sentence appended to each is
 * unreadable and unsortable. It is the shared `DataGrid` — sortable and
 * filterable columns, one whole-table search, self-excluding value menus — the
 * same table every other screen uses. The capability chips above it narrow the
 * declared list before the grid sees it; an entry that cannot be chosen stays
 * visible, greyed, with the reason on its row.
 */
export function ModelPickerField({ options, value, onChange, loading }: ModelPickerFieldProps) {
  const { t, i18n } = useTranslation()
  const grid = useDataGridState({ pageSize: 10 })
  const [facet, setFacet] = useState<Facet>('serves')
  const selected = options.find((option) => option.key === value)

  const counts = useMemo(
    () =>
      Object.fromEntries(
        FACETS.map((entry) => [
          entry,
          options.filter((option) => matchesFacet(option, entry)).length,
        ]),
      ) as Record<Facet, number>,
    [options],
  )
  // An account with no compatible models still shows the reasons on its rows.
  const activeFacet = facet === 'serves' && counts.serves === 0 ? 'all' : facet
  const rows = useMemo(
    () => options.filter((option) => matchesFacet(option, activeFacet)),
    [options, activeFacet],
  )

  const columns = useMemo(
    (): GridColumn<ModelPickerOption>[] => [
      {
        id: 'model',
        label: t('admin.model'),
        value: (option) => option.modelName,
        searchText: (option) =>
          [
            option.modelName,
            option.note && option.note !== 'not_in_catalog'
              ? t(`admin.aiPanel.modelNotes.${option.note}`)
              : '',
            option.modelCatalogId ? t('admin.aiPanel.facets.in_catalog') : '',
          ].join(' '),
        cell: (option) => modelNameCell(option, t),
      },
      {
        id: 'context',
        label: t('admin.aiPanel.columns.context'),
        align: 'end',
        value: (option) => option.contextWindow ?? null,
        displayText: (option) => formatTokens(option.contextWindow, i18n.language),
      },
      {
        id: 'output',
        label: t('admin.aiPanel.columns.output'),
        align: 'end',
        value: (option) => option.maxOutputTokens ?? null,
        displayText: (option) => formatTokens(option.maxOutputTokens, i18n.language),
      },
      {
        id: 'inputPrice',
        label: t('admin.aiPanel.columns.inputPrice'),
        align: 'end',
        value: (option) => (option.price ? Number(option.price.input) : null),
        displayText: (option) => (option.price ? formatRate(option.price.input) : '—'),
      },
      {
        id: 'outputPrice',
        label: t('admin.aiPanel.columns.outputPrice'),
        align: 'end',
        value: (option) => (option.price ? Number(option.price.output) : null),
        displayText: (option) => (option.price ? formatRate(option.price.output) : '—'),
      },
      {
        id: 'currency',
        label: t('admin.aiPanel.columns.currency'),
        value: (option) => option.price?.currencyCode ?? null,
        displayText: (option) => option.price?.currencyCode ?? '—',
      },
    ],
    [t, i18n.language],
  )
  // Entirely absent metadata is not a useful comparison column. Keep every
  // populated column and its shared search/sort/filter behaviour intact.
  const visibleColumns = columns.filter(
    (column) => column.id === 'model' || options.some((option) => column.value(option) !== null),
  )

  const capabilityChips = (
    <fieldset className="model-picker-field__facets">
      <legend className="visually-hidden">{t('admin.aiPanel.facets.all')}</legend>
      {FACETS.filter((entry) => entry === 'all' || counts[entry] > 0).map((entry) => (
        <button
          key={entry}
          type="button"
          className={activeFacet === entry ? 'is-active' : ''}
          aria-pressed={activeFacet === entry}
          onClick={() => {
            setFacet(entry)
            grid.setState((current) => ({ ...current, page: 1 }))
          }}
        >
          {t(`admin.aiPanel.facets.${entry}`)}
          <span>{counts[entry]}</span>
        </button>
      ))}
    </fieldset>
  )

  return (
    <div className="model-picker-field">
      <output className="model-picker-field__selection" aria-label={t('admin.model')}>
        {selected ? modelNameCell(selected, t) : <strong>{t('admin.aiPanel.selectColumn')}</strong>}
      </output>
      <DataGrid<ModelPickerOption>
        rows={rows}
        rowId={modelOptionKey}
        columns={visibleColumns}
        selection={{
          activeRowId: value || null,
          onActivate: (option) => {
            if (option.selectable) onChange(option.key)
          },
          isSelectable: (option) => option.selectable,
          headerLabel: t('admin.aiPanel.selectColumn'),
          rowLabel: (option) => option.modelName,
        }}
        state={grid.state}
        onStateChange={grid.setState}
        ariaLabel={t('admin.model')}
        emptyText={
          loading
            ? t('admin.aiPanel.messages.loadingProviderModels')
            : t('admin.aiPanel.messages.noModelMatchesFilter')
        }
        noMatchText={t('admin.aiPanel.messages.noModelMatchesFilter')}
        pending={loading && options.length === 0}
        pageSizeOptions={[10, 25, 50]}
        toolbarExtras={capabilityChips}
      />
    </div>
  )
}
