import type { ReactNode } from "react";
import { AlertTriangle } from "lucide-react";

import { Button } from "@/shared/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/components/ui/dialog";

type ConfirmDialogProps = {
  open: boolean;
  title: ReactNode;
  description?: ReactNode;
  cancelLabel: ReactNode;
  confirmLabel: ReactNode;
  onCancel: () => void;
  onConfirm: () => void;
  onOpenChange?: (open: boolean) => void;
  confirmDisabled?: boolean;
  destructive?: boolean;
  icon?: ReactNode;
};

export function ConfirmDialog({
  open,
  title,
  description,
  cancelLabel,
  confirmLabel,
  onCancel,
  onConfirm,
  onOpenChange,
  confirmDisabled = false,
  destructive = false,
  icon,
}: ConfirmDialogProps) {
  return (
    <Dialog
      open={open}
      onOpenChange={(nextOpen) => {
        onOpenChange?.(nextOpen);
        if (!nextOpen) onCancel();
      }}
    >
      <DialogContent>
        <DialogHeader>
          <div className="flex items-start gap-3">
            <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-lg bg-muted">
              {icon ?? <AlertTriangle className="h-4 w-4 text-muted-foreground" />}
            </div>
            <div className="min-w-0 text-left">
              <DialogTitle>{title}</DialogTitle>
              {description ? (
                <DialogDescription className="mt-2">{description}</DialogDescription>
              ) : null}
            </div>
          </div>
        </DialogHeader>
        <DialogFooter>
          <Button onClick={onCancel} variant="outline">
            {cancelLabel}
          </Button>
          <Button
            disabled={confirmDisabled}
            onClick={onConfirm}
            variant={destructive ? "destructive" : "default"}
          >
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
