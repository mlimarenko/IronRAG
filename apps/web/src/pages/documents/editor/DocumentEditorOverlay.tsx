import type { ReactNode } from 'react';

import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog';
import {
  Drawer,
  DrawerContent,
  DrawerDescription,
  DrawerFooter,
  DrawerHeader,
  DrawerTitle,
} from '@/components/ui/drawer';
import { useIsMobile } from '@/hooks/use-mobile';

type DocumentEditorOverlayProps = {
  actions: ReactNode;
  children: ReactNode;
  description: string;
  helperText: string;
  onOpenChange: (open: boolean) => void;
  open: boolean;
  title: string;
};

export function DocumentEditorOverlay({
  actions,
  children,
  description,
  helperText,
  onOpenChange,
  open,
  title,
}: DocumentEditorOverlayProps) {
  const isMobile = useIsMobile();

  if (isMobile) {
    return (
      <Drawer open={open} onOpenChange={onOpenChange}>
        <DrawerContent className="mt-0 h-[100dvh] rounded-none p-0">
          <DrawerHeader className="border-b px-4 py-4 text-left">
            <DrawerTitle>{title}</DrawerTitle>
            <DrawerDescription>{description}</DrawerDescription>
            <p className="text-xs text-muted-foreground">{helperText}</p>
          </DrawerHeader>
          {children}
          <DrawerFooter className="border-t bg-background/95 px-4 py-4">
            {actions}
          </DrawerFooter>
        </DrawerContent>
      </Drawer>
    );
  }

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="left-1/2 top-1/2 flex h-[min(96dvh,1120px)] w-[min(98vw,1600px)] max-w-none -translate-x-1/2 -translate-y-1/2 flex-col gap-0 overflow-hidden rounded-[28px] border border-border/70 bg-background p-0 shadow-[0_32px_120px_hsl(var(--foreground)/0.18)]">
        <DialogHeader className="border-b bg-background/95 px-6 py-5 text-left backdrop-blur supports-[backdrop-filter]:bg-background/90 sm:px-8 sm:py-6">
          <DialogTitle className="pr-10 text-[1.55rem] font-semibold tracking-tight">
            {title}
          </DialogTitle>
          <DialogDescription className="text-base">{description}</DialogDescription>
          <p className="text-xs text-muted-foreground">{helperText}</p>
        </DialogHeader>
        {children}
        <DialogFooter className="border-t bg-background/95 px-6 py-4 backdrop-blur supports-[backdrop-filter]:bg-background/90 sm:px-8 sm:py-5">
          {actions}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
