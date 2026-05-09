import type { TFunction } from "i18next";
import { RotateCw, Upload } from "lucide-react";

import { Button } from "@/shared/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/shared/components/ui/dialog";

import type { UploadQueueController } from "./useUploadQueueController";

type UploadQueueSectionProps = {
  controller: UploadQueueController;
  t: TFunction;
};

export function UploadQueueSection({
  controller,
  t,
}: UploadQueueSectionProps) {
  const { duplicateConflict, resolveDuplicate } = controller;
  return (
    <Dialog
      open={Boolean(duplicateConflict)}
      onOpenChange={(open) => {
        if (!open) void resolveDuplicate("skip");
      }}
    >
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>{t("documents.duplicateTitle")}</DialogTitle>
          <DialogDescription className="break-all">
            {t("documents.duplicateDescription", {
              name: duplicateConflict?.candidate.name ?? "",
            })}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter className="flex-col gap-2 sm:flex-row">
          <Button
            variant="default"
            onClick={() => void resolveDuplicate("replace")}
          >
            <RotateCw className="mr-2 h-3.5 w-3.5" />
            {t("documents.duplicateReplace")}
          </Button>
          <Button variant="outline" onClick={() => void resolveDuplicate("add")}>
            <Upload className="mr-2 h-3.5 w-3.5" />
            {t("documents.duplicateAddNew")}
          </Button>
          <Button variant="ghost" onClick={() => void resolveDuplicate("skip")}>
            {t("documents.duplicateSkip")}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
