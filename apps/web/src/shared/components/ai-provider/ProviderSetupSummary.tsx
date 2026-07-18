import { AlertCircle, CheckCircle2 } from 'lucide-react'

type ProviderSetupSummaryRow = {
  label: string
  value: string
  state?: 'ready' | 'attention' | 'neutral'
}

type ProviderSetupSummaryProps = {
  title: string
  description: string
  ready: boolean
  readyLabel: string
  attentionLabel: string
  rows: ProviderSetupSummaryRow[]
}

export function ProviderSetupSummary({
  title,
  description,
  ready,
  readyLabel,
  attentionLabel,
  rows,
}: Readonly<ProviderSetupSummaryProps>) {
  const Icon = ready ? CheckCircle2 : AlertCircle

  return (
    <div className="rounded-xl border border-border/60 bg-surface-sunken p-4 space-y-3">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="text-sm font-semibold [overflow-wrap:anywhere]">{title}</div>
          <div className="text-xs text-muted-foreground mt-0.5 [overflow-wrap:anywhere]">
            {description}
          </div>
        </div>
        <div
          className="flex h-7 shrink-0 items-center gap-1.5 rounded-full px-2 text-xs font-medium"
          style={{
            background: ready ? 'hsl(var(--status-ready-bg))' : 'hsl(var(--status-warning-bg))',
            boxShadow: ready
              ? 'inset 0 0 0 1px hsl(var(--status-ready-ring) / 0.5)'
              : 'inset 0 0 0 1px hsl(var(--status-warning-ring) / 0.5)',
          }}
        >
          <Icon
            className={ready ? 'h-3.5 w-3.5 text-status-ready' : 'h-3.5 w-3.5 text-status-warning'}
          />
          <span>{ready ? readyLabel : attentionLabel}</span>
        </div>
      </div>
      <div className="grid gap-2 text-xs sm:grid-cols-2">
        {rows.map((row) => (
          <div
            key={row.label}
            className="min-w-0 rounded-lg border border-border/50 bg-background/70 px-3 py-2"
          >
            <div className="text-muted-foreground">{row.label}</div>
            <div className="mt-1 font-medium text-foreground [overflow-wrap:anywhere]">
              {row.value}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}
