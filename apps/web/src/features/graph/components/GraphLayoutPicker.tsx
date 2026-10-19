import { useTranslation } from 'react-i18next'
import { GRAPH_LAYOUT_OPTIONS, type GraphLayoutType } from '../model/config'

export function GraphLayoutPicker({
  value,
  onChange,
}: Readonly<{
  value: GraphLayoutType
  onChange: (layout: GraphLayoutType) => void
}>) {
  const { t } = useTranslation()

  return (
    <label className="graph-layout-picker">
      <span>{t('graph.layout')}</span>
      <select
        className="input"
        aria-label={t('graph.layoutControls')}
        title={t(
          GRAPH_LAYOUT_OPTIONS.find((option) => option.id === value)?.descriptionKey ??
            'graph.layoutControls',
        )}
        value={value}
        onChange={(event) => onChange(event.target.value as GraphLayoutType)}
      >
        {GRAPH_LAYOUT_OPTIONS.map((option) => (
          <option key={option.id} value={option.id}>
            {t(option.labelKey)}
          </option>
        ))}
      </select>
    </label>
  )
}
