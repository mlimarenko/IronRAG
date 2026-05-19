import { useEffect, useMemo } from "react";
import type { TFunction } from "i18next";
import {
  CheckCircle2,
  Compass,
  FileText,
  HelpCircle,
  Link2,
  Loader2,
  XCircle,
} from "lucide-react";

import type { WebIngestMode } from "@/shared/api";
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

import {
  WEB_BOUNDARY_POLICIES,
  isSubdomainBoundaryAvailableForSeed,
  isWebBoundaryPolicy,
} from "@/features/documents/model/webIngestBoundary";
import {
  buildWebIngestUrlFilter,
  evaluateWebIngestUrlFilter,
  formatWebIngestPattern,
  type WebIngestFilterEvaluation,
} from "@/features/documents/model/webIngestPatterns";

import type { WebIngestController } from "./useWebIngestController";

const WEB_INGEST_MODES: readonly WebIngestMode[] = [
  "single_page",
  "recursive_crawl",
];

function isWebIngestMode(value: string): value is WebIngestMode {
  return WEB_INGEST_MODES.some((mode) => mode === value);
}

type FilterEditorProps = {
  allowId: string;
  allowValue: string;
  blockId: string;
  blockValue: string;
  icon: "crawl" | "document";
  onAllowChange: (value: string) => void;
  onBlockChange: (value: string) => void;
  title: string;
  t: TFunction;
};

function FilterEditor({
  allowId,
  allowValue,
  blockId,
  blockValue,
  icon,
  onAllowChange,
  onBlockChange,
  title,
  t,
}: FilterEditorProps) {
  const Icon = icon === "crawl" ? Compass : FileText;
  return (
    <section className="rounded-md border bg-background p-3 shadow-[inset_0_1px_0_hsl(0_0%_100%/0.6)]">
      <div className="mb-3 flex items-center gap-2">
        <span className="flex h-8 w-8 items-center justify-center rounded-md bg-muted text-muted-foreground">
          <Icon className="h-4 w-4" />
        </span>
        <h3 className="text-sm font-medium leading-none">{title}</h3>
      </div>
      <div className="grid gap-3 md:grid-cols-2">
        <div>
          <Label htmlFor={allowId}>{t("documents.filterAllowPatterns")}</Label>
          <Textarea
            id={allowId}
            value={allowValue}
            onChange={(event) => onAllowChange(event.target.value)}
            placeholder={t("documents.filterAllowPatternsPlaceholder")}
            className="mt-2 min-h-[116px] resize-y font-mono text-xs"
          />
        </div>
        <div>
          <Label htmlFor={blockId}>{t("documents.filterBlockPatterns")}</Label>
          <Textarea
            id={blockId}
            value={blockValue}
            onChange={(event) => onBlockChange(event.target.value)}
            placeholder={t("documents.filterBlockPatternsPlaceholder")}
            className="mt-2 min-h-[116px] resize-y font-mono text-xs"
          />
        </div>
      </div>
    </section>
  );
}

type ParsedRuleFilters =
  | {
      ok: true;
      crawlFilter: ReturnType<typeof buildWebIngestUrlFilter>;
      materializationFilter: ReturnType<typeof buildWebIngestUrlFilter>;
    }
  | {
      ok: false;
      error: string;
    };

function buildParsedRuleFilters(
  crawlAllowPatternsText: string,
  crawlBlockPatternsText: string,
  materializationAllowPatternsText: string,
  materializationBlockPatternsText: string,
): ParsedRuleFilters {
  try {
    return {
      ok: true,
      crawlFilter: buildWebIngestUrlFilter(
        crawlAllowPatternsText,
        crawlBlockPatternsText,
      ),
      materializationFilter: buildWebIngestUrlFilter(
        materializationAllowPatternsText,
        materializationBlockPatternsText,
      ),
    };
  } catch (error) {
    return {
      ok: false,
      error: error instanceof Error ? error.message : String(error),
    };
  }
}

function resultBadgeClass(result: WebIngestFilterEvaluation | null): string {
  if (!result) return "status-sparse";
  if (result.status === "no_allow_match") return "status-warning";
  return result.passes ? "status-ready" : "status-failed";
}

function resultIcon(result: WebIngestFilterEvaluation | null) {
  if (!result) return <HelpCircle className="h-3.5 w-3.5" />;
  return result.passes ? (
    <CheckCircle2 className="h-3.5 w-3.5" />
  ) : (
    <XCircle className="h-3.5 w-3.5" />
  );
}

function resultLabel(
  result: WebIngestFilterEvaluation | null,
  t: TFunction,
): string {
  if (!result) return t("documents.ruleTesterWaiting");
  if (result.passes) return t("documents.ruleTesterPass");
  if (result.status === "no_allow_match") {
    return t("documents.ruleTesterNoAllowBadge");
  }
  if (result.status === "invalid_url") return t("documents.ruleTesterInvalidBadge");
  return t("documents.ruleTesterFail");
}

function resultDetail(
  result: WebIngestFilterEvaluation | null,
  t: TFunction,
): string {
  if (!result) return t("documents.ruleTesterEmpty");
  if (result.status === "invalid_url") return t("documents.ruleTesterInvalid");
  if (result.status === "blocked" && result.matchedPattern) {
    return t("documents.ruleTesterMatchedBlock", {
      pattern: formatWebIngestPattern(result.matchedPattern),
    });
  }
  if (result.status === "allowed" && result.matchedPattern) {
    return t("documents.ruleTesterMatchedAllow", {
      pattern: formatWebIngestPattern(result.matchedPattern),
    });
  }
  if (result.status === "allowed_without_rules") {
    return t("documents.ruleTesterNoRules");
  }
  return t("documents.ruleTesterNoAllowMatch");
}

type RuleTestRowProps = {
  icon: "crawl" | "document";
  result: WebIngestFilterEvaluation | null;
  title: string;
  t: TFunction;
};

function RuleTestRow({ icon, result, title, t }: RuleTestRowProps) {
  const Icon = icon === "crawl" ? Compass : FileText;
  return (
    <div className="flex min-w-0 items-start gap-3 rounded-md border bg-background px-3 py-2.5">
      <span className="mt-0.5 flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
        <Icon className="h-3.5 w-3.5" />
      </span>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-sm font-medium leading-5">{title}</span>
          <span className={`status-badge ${resultBadgeClass(result)}`}>
            {resultIcon(result)}
            {resultLabel(result, t)}
          </span>
        </div>
        <p className="mt-1 text-xs leading-5 text-muted-foreground [overflow-wrap:anywhere]">
          {resultDetail(result, t)}
        </p>
      </div>
    </div>
  );
}

type RuleTesterProps = {
  controller: WebIngestController;
  isRecursive: boolean;
  parsedFilters: ParsedRuleFilters;
  t: TFunction;
};

function RuleTester({
  controller,
  isRecursive,
  parsedFilters,
  t,
}: RuleTesterProps) {
  const testUrl = controller.ruleTestUrl.trim();
  const crawlResult =
    parsedFilters.ok && testUrl
      ? evaluateWebIngestUrlFilter(testUrl, parsedFilters.crawlFilter)
      : null;
  const materializationResult =
    parsedFilters.ok && testUrl
      ? evaluateWebIngestUrlFilter(testUrl, parsedFilters.materializationFilter)
      : null;

  return (
    <section className="rounded-md border bg-surface-sunken p-3">
      <div className="mb-3 flex items-center gap-2">
        <span className="flex h-8 w-8 items-center justify-center rounded-md bg-background text-muted-foreground">
          <Link2 className="h-4 w-4" />
        </span>
        <h3 className="text-sm font-medium leading-none">
          {t("documents.ruleTesterTitle")}
        </h3>
      </div>
      <div>
        <Label htmlFor="web-ingest-rule-test-url">
          {t("documents.ruleTesterUrl")}
        </Label>
        <Input
          id="web-ingest-rule-test-url"
          value={controller.ruleTestUrl}
          onChange={(event) => controller.setRuleTestUrl(event.target.value)}
          placeholder={t("documents.ruleTesterPlaceholder")}
          className="mt-2"
        />
      </div>
      {!parsedFilters.ok && (
        <p className="mt-2 text-xs leading-5 text-destructive [overflow-wrap:anywhere]">
          {t("documents.ruleTesterRulesInvalid", { error: parsedFilters.error })}
        </p>
      )}
      <div className="mt-3 grid gap-2 md:grid-cols-2">
        {isRecursive && (
          <RuleTestRow
            icon="crawl"
            result={crawlResult}
            title={t("documents.crawlFilterTitle")}
            t={t}
          />
        )}
        <RuleTestRow
          icon="document"
          result={materializationResult}
          title={t("documents.materializationFilterTitle")}
          t={t}
        />
      </div>
    </section>
  );
}

type WebIngestDialogProps = {
  controller: WebIngestController;
  t: TFunction;
};

export function WebIngestDialog({ controller, t }: WebIngestDialogProps) {
  const isRecursive = controller.crawlMode === "recursive_crawl";
  const subdomainBoundaryAvailable = isSubdomainBoundaryAvailableForSeed(
    controller.seedUrl,
  );
  const parsedFilters = useMemo(
    () =>
      buildParsedRuleFilters(
        controller.crawlAllowPatternsText,
        controller.crawlBlockPatternsText,
        controller.materializationAllowPatternsText,
        controller.materializationBlockPatternsText,
      ),
    [
      controller.crawlAllowPatternsText,
      controller.crawlBlockPatternsText,
      controller.materializationAllowPatternsText,
      controller.materializationBlockPatternsText,
    ],
  );
  useEffect(() => {
    if (
      !subdomainBoundaryAvailable &&
      controller.boundaryPolicy === "same_host_and_subdomains"
    ) {
      controller.setBoundaryPolicy("same_host");
    }
  }, [controller, subdomainBoundaryAvailable]);
  return (
    <Dialog
      open={controller.addLinkOpen}
      onOpenChange={controller.setAddLinkOpen}
    >
      <DialogContent className="max-h-[90vh] max-w-4xl overflow-y-auto">
        <DialogHeader>
          <DialogTitle>{t("documents.addWebContent")}</DialogTitle>
          <DialogDescription>
            {t("documents.addWebContentDesc")}
          </DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <div>
            <Label htmlFor="web-ingest-seed-url">{t("documents.seedUrl")}</Label>
            <Input
              id="web-ingest-seed-url"
              value={controller.seedUrl}
              onChange={(event) => controller.setSeedUrl(event.target.value)}
              placeholder={t("documents.seedUrlPlaceholder")}
              className="mt-2"
            />
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
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
                  <SelectItem
                    value="same_host_and_subdomains"
                    disabled={!subdomainBoundaryAvailable}
                  >
                    {t("documents.sameHostAndSubdomains")}
                  </SelectItem>
                  <SelectItem value="allow_external">
                    {t("documents.allowExternal")}
                  </SelectItem>
                </SelectContent>
              </Select>
            </div>
          </div>
          {isRecursive && (
            <div className="grid gap-3 sm:grid-cols-2">
              <div>
                <Label htmlFor="web-ingest-max-depth">
                  {t("documents.maxDepth")}
                </Label>
                <Input
                  id="web-ingest-max-depth"
                  type="number"
                  value={controller.maxDepth}
                  onChange={(event) => controller.setMaxDepth(event.target.value)}
                  min="1"
                  max="100"
                  className="mt-2"
                />
              </div>
              <div>
                <Label htmlFor="web-ingest-max-pages">
                  {t("documents.maxPages")}
                </Label>
                <Input
                  id="web-ingest-max-pages"
                  type="number"
                  value={controller.maxPages}
                  onChange={(event) => controller.setMaxPages(event.target.value)}
                  min="1"
                  max="10000"
                  className="mt-2"
                />
              </div>
            </div>
          )}
          {isRecursive && (
            <FilterEditor
              allowId="web-ingest-crawl-allow"
              allowValue={controller.crawlAllowPatternsText}
              blockId="web-ingest-crawl-block"
              blockValue={controller.crawlBlockPatternsText}
              icon="crawl"
              onAllowChange={controller.setCrawlAllowPatternsText}
              onBlockChange={controller.setCrawlBlockPatternsText}
              title={t("documents.crawlFilterTitle")}
              t={t}
            />
          )}
          <FilterEditor
            allowId="web-ingest-materialization-allow"
            allowValue={controller.materializationAllowPatternsText}
            blockId="web-ingest-materialization-block"
            blockValue={controller.materializationBlockPatternsText}
            icon="document"
            onAllowChange={controller.setMaterializationAllowPatternsText}
            onBlockChange={controller.setMaterializationBlockPatternsText}
            title={t("documents.materializationFilterTitle")}
            t={t}
          />
          <RuleTester
            controller={controller}
            isRecursive={isRecursive}
            parsedFilters={parsedFilters}
            t={t}
          />
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
              controller.webIngestPolicyLoading
            }
            onClick={() => void controller.startWebIngest()}
          >
            {controller.webIngestLoading || controller.webIngestPolicyLoading ? (
              <>
                <Loader2 className="mr-1.5 h-3.5 w-3.5 animate-spin" />
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
