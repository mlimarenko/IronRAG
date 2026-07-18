import { useRef, useState } from 'react'
import type { TFunction } from 'i18next'
import {
  Atom,
  Blend,
  Boxes,
  ChevronDown,
  GitBranch,
  Group,
  PieChart,
  Radar,
  Rows3,
  Sparkles,
  Target,
  Waypoints,
  Workflow,
  type LucideIcon,
} from 'lucide-react'

import { GRAPH_LAYOUT_OPTIONS, type GraphLayoutType } from '@/features/graph/model/config'

// One icon per layout, chosen to hint at the arrangement the layout produces
// (hub-and-spoke, provenance branches, layered flow, concentric rings, …) so
// the picker reads visually instead of as a bare text list.
const LAYOUT_ICONS: Record<GraphLayoutType, LucideIcon> = {
  force: Atom,
  hubs: Waypoints,
  sources: GitBranch,
  flow: Workflow,
  radial: Radar,
  circlepack: Blend,
  sectors: PieChart,
  bands: Rows3,
  components: Boxes,
  rings: Target,
  clusters: Group,
}

type GraphLayoutPickerProps = Readonly<{
  value: GraphLayoutType
  recommended: GraphLayoutType | null
  onChange: (value: GraphLayoutType) => void
  t: TFunction
}>

/**
 * Icon-tile layout picker. A combobox trigger showing the current layout's icon
 * + name opens a compact popover grid of icon tiles (one per layout), each with
 * a hover description; the recommended layout is marked with a subtle sparkle.
 */
export function GraphLayoutPicker({ value, recommended, onChange, t }: GraphLayoutPickerProps) {
  const [open, setOpen] = useState(false)
  const triggerRef = useRef<HTMLButtonElement>(null)
  const CurrentIcon = LAYOUT_ICONS[value]

  const choose = (next: GraphLayoutType) => {
    onChange(next)
    setOpen(false)
    triggerRef.current?.focus()
  }

  return (
    <div className="relative w-full sm:w-auto">
      <button
        ref={triggerRef}
        type="button"
        aria-expanded={open}
        aria-label={t('graph.layoutControls')}
        onClick={() => setOpen((prev) => !prev)}
        onKeyDown={(event) => {
          if (event.key === 'ArrowDown') {
            event.preventDefault()
            setOpen(true)
          }
        }}
        className="flex h-8 w-full items-center gap-2 rounded-lg border bg-background px-2.5 text-xs font-medium shadow-soft transition-colors hover:bg-muted/60 sm:w-[11rem]"
      >
        <CurrentIcon className="h-3.5 w-3.5 shrink-0 text-primary" />
        <span className="min-w-0 flex-1 truncate text-left">{t(`graph.layouts.${value}`)}</span>
        <ChevronDown
          className={`h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform duration-200 ${open ? 'rotate-180' : ''}`}
        />
      </button>

      {open && (
        <>
          <button
            type="button"
            aria-hidden="true"
            tabIndex={-1}
            className="fixed inset-0 z-40 cursor-default"
            onClick={() => setOpen(false)}
          />
          <div
            aria-label={t('graph.layoutControls')}
            className="absolute left-0 top-full z-50 mt-1.5 w-[min(22rem,calc(100vw-2rem))] rounded-xl border bg-popover p-2 shadow-elevated animate-scale-in"
          >
            <div className="grid grid-cols-2 gap-1.5">
              {GRAPH_LAYOUT_OPTIONS.map((option) => {
                const Icon = LAYOUT_ICONS[option.id]
                const active = option.id === value
                const isRecommended = recommended === option.id
                return (
                  <button
                    key={option.id}
                    type="button"
                    aria-pressed={active}
                    title={t(option.descriptionKey)}
                    onClick={() => choose(option.id)}
                    className={`group flex items-start gap-2.5 rounded-lg border p-2.5 text-left transition-colors ${
                      active
                        ? 'border-primary/40 bg-accent-subtle ring-1 ring-primary/25'
                        : 'border-transparent hover:bg-muted/60'
                    }`}
                  >
                    <span
                      className={`mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md ${
                        active
                          ? 'bg-primary/10 text-primary'
                          : 'bg-muted text-muted-foreground group-hover:text-foreground'
                      }`}
                    >
                      <Icon className="h-4 w-4" />
                    </span>
                    <span className="flex min-w-0 items-center gap-1.5">
                      <span className="truncate text-xs font-semibold">{t(option.labelKey)}</span>
                      {isRecommended && (
                        <Sparkles
                          className="h-3.5 w-3.5 shrink-0 text-primary"
                          aria-label={t('graph.recommended')}
                        />
                      )}
                    </span>
                  </button>
                )
              })}
            </div>
          </div>
        </>
      )}
    </div>
  )
}
