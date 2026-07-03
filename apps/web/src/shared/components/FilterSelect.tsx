import type { ReactNode } from "react";

import {
  Select,
  SelectContent,
  SelectTrigger,
  SelectValue,
} from "@/shared/components/ui/select";
import { cn } from "@/shared/lib/utils";

/**
 * Canonical filter dropdown for every list-page toolbar (documents, libraries,
 * queue, audit). One height, one surface, one icon slot — so filters read the
 * same everywhere. The `icon` must be a *distinct, meaningful* glyph per filter;
 * do not repeat the same icon across sibling filters (that reads as decoration
 * noise). Width is left to the caller via `className` so the same control works
 * inside a fixed toolbar (`w-[220px]`) or a responsive grid cell (`w-full`).
 */
export function FilterSelect({
  ariaLabel,
  children,
  className,
  disabled,
  icon,
  onValueChange,
  value,
}: {
  ariaLabel?: string;
  children: ReactNode;
  className?: string;
  disabled?: boolean;
  icon?: ReactNode;
  onValueChange: (value: string) => void;
  value: string;
}) {
  return (
    <Select
      value={value}
      onValueChange={onValueChange}
      {...(disabled !== undefined ? { disabled } : {})}
    >
      <SelectTrigger
        aria-label={ariaLabel}
        className={cn(
          "h-9 gap-1.5 rounded-lg bg-card text-xs shadow-soft",
          className,
        )}
      >
        {icon ? (
          <span className="shrink-0 text-muted-foreground [&_svg]:h-3.5 [&_svg]:w-3.5">
            {icon}
          </span>
        ) : null}
        <SelectValue />
      </SelectTrigger>
      <SelectContent>{children}</SelectContent>
    </Select>
  );
}
