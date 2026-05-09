import type { TFunction } from "i18next";
import { Loader2 } from "lucide-react";

import type {
  WebBoundaryPolicy,
  WebIngestMode,
  WebIngestUrlFilterMode,
} from "@/shared/api";
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
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/shared/components/ui/select";
import { Textarea } from "@/shared/components/ui/textarea";

import type { WebIngestController } from "./useWebIngestController";

const WEB_INGEST_MODES: readonly WebIngestMode[] = [
  "single_page",
  "recursive_crawl",
];
const WEB_BOUNDARY_POLICIES: readonly WebBoundaryPolicy[] = [
  "same_host",
  "allow_external",
];
const WEB_INGEST_URL_FILTER_MODES: readonly WebIngestUrlFilterMode[] = [
  "blocklist",
  "allowlist",
];

function isWebIngestMode(value: string): value is WebIngestMode {
  return WEB_INGEST_MODES.some((mode) => mode === value);
}

function isWebBoundaryPolicy(value: string): value is WebBoundaryPolicy {
  return WEB_BOUNDARY_POLICIES.some((policy) => policy === value);
}

function isWebIngestUrlFilterMode(
  value: string,
): value is WebIngestUrlFilterMode {
  return WEB_INGEST_URL_FILTER_MODES.some((mode) => mode === value);
}

type WebIngestDialogProps = {
  controller: WebIngestController;
  t: TFunction;
};

export function WebIngestDialog({ controller, t }: WebIngestDialogProps) {
  return (
    <Dialog
      open={controller.addLinkOpen}
      onOpenChange={controller.setAddLinkOpen}
    >
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t("documents.addWebContent")}</DialogTitle>
          <DialogDescription>
            {t("documents.addWebContentDesc")}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label>{t("documents.seedUrl")}</Label>
            <Input
              value={controller.seedUrl}
              onChange={(event) => controller.setSeedUrl(event.target.value)}
              placeholder={t("documents.seedUrlPlaceholder")}
              className="mt-2"
            />
          </div>
          <div className="grid grid-cols-2 gap-3">
            <div>
              <Label>{t("documents.mode")}</Label>
              <Select
                value={controller.crawlMode}
                onValueChange={(value) => {
                  if (isWebIngestMode(value)) controller.setCrawlMode(value);
                }}
              >
                <SelectTrigger className="mt-2">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="single_page">
                    {t("documents.singlePage")}
                  </SelectItem>
                  <SelectItem value="recursive_crawl">
                    {t("documents.recursiveCrawl")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div>
              <Label>{t("documents.boundary")}</Label>
              <Select
                value={controller.boundaryPolicy}
                onValueChange={(value) => {
                  if (isWebBoundaryPolicy(value)) {
                    controller.setBoundaryPolicy(value);
                  }
                }}
              >
                <SelectTrigger className="mt-2">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="same_host">
                    {t("documents.sameHost")}
                  </SelectItem>
                  <SelectItem value="allow_external">
                    {t("documents.allowExternal")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          {controller.crawlMode === "recursive_crawl" && (
            <div className="grid grid-cols-2 gap-3">
              <div>
                <Label>{t("documents.maxDepth")}</Label>
                <Input
                  type="number"
                  value={controller.maxDepth}
                  onChange={(event) => controller.setMaxDepth(event.target.value)}
                  min="1"
                  max="10"
                  className="mt-2"
                />
              </div>
              <div>
                <Label>{t("documents.maxPages")}</Label>
                <Input
                  type="number"
                  value={controller.maxPages}
                  onChange={(event) => controller.setMaxPages(event.target.value)}
                  min="1"
                  max="500"
                  className="mt-2"
                />
              </div>
            </div>
          )}
          <div className="grid gap-3 sm:grid-cols-[12rem_minmax(0,1fr)]">
            <div>
              <Label>{t("documents.urlFilterMode")}</Label>
              <Select
                value={controller.urlFilterMode}
                onValueChange={(value) => {
                  if (isWebIngestUrlFilterMode(value)) {
                    controller.setUrlFilterMode(value);
                  }
                }}
              >
                <SelectTrigger className="mt-2">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="blocklist">
                    {t("documents.urlFilterModeBlocklist")}
                  </SelectItem>
                  <SelectItem value="allowlist">
                    {t("documents.urlFilterModeAllowlist")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
            <div>
              <Label>{t("documents.urlFilterPatterns")}</Label>
              <Textarea
                value={controller.urlFilterPatternsText}
                onChange={(event) =>
                  controller.setUrlFilterPatternsText(event.target.value)
                }
                placeholder={t("documents.urlFilterPatternsPlaceholder")}
                className="mt-2 min-h-[118px] resize-y font-mono text-xs"
              />
            </div>
          </div>
          {controller.urlFilterMode === "allowlist" &&
            controller.urlFilterPatternsText.trim().length === 0 && (
              <p className="-mt-2 text-xs text-muted-foreground">
                {t("documents.urlFilterAllowlistHint")}
              </p>
            )}
        </div>
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => controller.setAddLinkOpen(false)}
          >
            {t("documents.cancel")}
          </Button>
          <Button
            disabled={
              !controller.seedUrl.trim() ||
              controller.webIngestLoading ||
              controller.urlFilterLoading ||
              (controller.urlFilterMode === "allowlist" &&
                controller.urlFilterPatternsText.trim().length === 0)
            }
            onClick={() => void controller.startWebIngest()}
          >
            {controller.webIngestLoading || controller.urlFilterLoading ? (
              <>
                <Loader2 className="h-3.5 w-3.5 mr-1.5 animate-spin" />
                {t("documents.starting")}
              </>
            ) : (
              t("documents.startIngest")
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
