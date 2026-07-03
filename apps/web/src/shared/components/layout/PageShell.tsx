import type { ReactNode } from "react";

import { cn } from "@/shared/lib/utils";

type PageShellProps = {
  header?: ReactNode;
  children: ReactNode;
  className?: string;
  bodyClassName?: string;
  bodyScroll?: "auto" | "hidden" | "visible";
};

export function PageShell({
  header,
  children,
  className,
  bodyClassName,
  bodyScroll = "hidden",
}: PageShellProps) {
  const overflowClass =
    bodyScroll === "auto"
      ? "overflow-auto"
      : bodyScroll === "visible"
        ? "overflow-visible"
        : "overflow-hidden";

  return (
    <div className={cn("flex min-h-0 flex-1 flex-col bg-surface-sunken", className)}>
      {header}
      <div className={cn("min-h-0 flex-1", overflowClass, bodyClassName)}>{children}</div>
    </div>
  );
}
