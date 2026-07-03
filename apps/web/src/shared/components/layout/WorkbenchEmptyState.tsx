import type { ReactNode } from "react";

import { cn } from "@/shared/lib/utils";

type WorkbenchEmptyStateProps = {
  icon?: ReactNode;
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  className?: string;
};

export function WorkbenchEmptyState({
  icon,
  title,
  description,
  action,
  className,
}: WorkbenchEmptyStateProps) {
  return (
    <div className={cn("empty-state px-6 py-12", className)}>
      {icon ? (
        <div className="mb-4 flex h-14 w-14 items-center justify-center rounded-lg bg-muted">
          {icon}
        </div>
      ) : null}
      <h2 className="text-base font-bold tracking-tight">{title}</h2>
      {description ? (
        <p className="mt-2 max-w-md text-sm text-muted-foreground">{description}</p>
      ) : null}
      {action ? <div className="mt-4">{action}</div> : null}
    </div>
  );
}
