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
import { Input } from "@/shared/components/ui/input";
import { Label } from "@/shared/components/ui/label";

import type { UploadQueueController } from "./useUploadQueueController";

type UploadQueueSectionProps = {
  controller: UploadQueueController;
  t: TFunction;
};

export function UploadQueueSection({
  controller,
  t,
}: UploadQueueSectionProps) {
  const {
    cancelUploadDialog,
    confirmUploadDialog,
    duplicateConflict,
    resolveDuplicate,
    setUploadDialogHint,
    uploadDialogFileCount,
    uploadDialogHint,
    uploadDialogOpen,
  } = controller;
  return (
    <>
      <Dialog
        open={uploadDialogOpen}
        onOpenChange={(open) => {
          if (!open) cancelUploadDialog();
        }}
      >
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>{t("documents.uploadDialogTitle")}</DialogTitle>
            <DialogDescription>
              {t("documents.uploadDialogChosenFiles", {
                count: uploadDialogFileCount,
              })}
            </DialogDescription>
          </DialogHeader>
          <div className="space-y-3">
            <div>
              <Label htmlFor="document-upload-dialog-hint">
                {t("documents.documentHint")}
              </Label>
              <Input
                id="document-upload-dialog-hint"
                className="mt-2"
                maxLength={1024}
                value={uploadDialogHint}
                onChange={(event) => setUploadDialogHint(event.target.value)}
              />
              <p className="mt-2 text-xs text-muted-foreground">
                {t("documents.documentHintHelp")}
              </p>
            </div>
            <p className="rounded-md border border-border/70 bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
              {t("documents.uploadDialogHintAppliesToAll")}
            </p>
          </div>
          <DialogFooter className="flex-col gap-2 sm:flex-row">
            <Button variant="outline" onClick={cancelUploadDialog}>
              {t("common.cancel")}
            </Button>
            <Button onClick={() => void confirmUploadDialog()}>
              <Upload className="mr-2 h-3.5 w-3.5" />
              {t("documents.uploadDialogConfirm")}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

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
    </>
  );
}
