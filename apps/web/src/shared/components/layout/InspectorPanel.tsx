import type { ReactNode } from "react";

import { cn } from "@/shared/lib/utils";

type InspectorMetric = {
  id?: string;
  label: ReactNode;
  value: ReactNode;
  title?: string;
  mono?: boolean;
};

type InspectorPanelProps = {
  title?: ReactNode;
  titleText?: string;
  subtitle?: ReactNode;
  eyebrow?: ReactNode;
  status?: ReactNode;
  metrics?: InspectorMetric[];
  actions?: ReactNode;
  children?: ReactNode;
  empty?: ReactNode;
  className?: string;
  contentClassName?: string;
};

function metricKey(metric: InspectorMetric, index: number) {
  if (metric.id) return metric.id;
  if (metric.title) return metric.title;
  if (typeof metric.label === "string" || typeof metric.label === "number") {
    return String(metric.label);
  }
  return `metric-${index}`;
}

export function InspectorPanel({
  title,
  titleText,
  subtitle,
  eyebrow,
  status,
  metrics,
  actions,
  children,
  empty,
  className,
  contentClassName,
}: InspectorPanelProps) {
  return (
    <aside className={cn("h-full min-h-0 overflow-y-auto bg-surface-sunken/40", className)}>
      {empty ? (
        <div className="flex min-h-64 items-center justify-center p-5 text-center text-sm text-muted-foreground">
          {empty}
        </div>
      ) : (
        <div className={cn("space-y-4 p-4", contentClassName)}>
          {(title || eyebrow || status || subtitle) && (
            <div className="border-b border-border/70 pb-3">
              <div className="flex items-start justify-between gap-3">
                <div className="min-w-0">
                  {eyebrow ? <div className="section-label">{eyebrow}</div> : null}
                  {title ? (
                    <h2 className="mt-1 truncate text-sm font-bold tracking-tight" title={titleText}>
                      {title}
                    </h2>
                  ) : null}
                  {subtitle ? (
                    <p className="mt-1 truncate text-xs text-muted-foreground">
                      {subtitle}
                    </p>
                  ) : null}
                </div>
                {status ? <div className="shrink-0">{status}</div> : null}
              </div>
            </div>
          )}
          {metrics && metrics.length > 0 ? (
            <dl className="grid grid-cols-2 gap-3 text-xs">
              {metrics.map((metric, index) => (
                <div className="min-w-0" key={metricKey(metric, index)}>
                  <dt className="text-muted-foreground">{metric.label}</dt>
                  <dd
                    className={cn(
                      "mt-0.5 truncate font-semibold",
                      metric.mono && "font-mono text-2xs",
                    )}
                    title={metric.title}
                  >
                    {metric.value}
                  </dd>
                </div>
              ))}
            </dl>
          ) : null}
          {actions ? <div className="flex flex-wrap gap-2">{actions}</div> : null}
          {children}
        </div>
      )}
    </aside>
  );
}
