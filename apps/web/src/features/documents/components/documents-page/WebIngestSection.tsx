import type { TFunction } from "i18next";

import { WebRunsPanel } from "@/features/documents/WebRunsPanel";
import { formatWebIngestPatterns } from "@/features/documents/model/webIngestPatterns";

import { WebIngestDialog } from "./WebIngestDialog";
import type { WebIngestController } from "./useWebIngestController";

type WebIngestSectionProps = {
  controller: WebIngestController;
  t: TFunction;
};

export function WebIngestSection({ controller, t }: WebIngestSectionProps) {
  return (
    <>
      <div className="flex h-full min-h-0 flex-col">
        <WebRunsPanel
          t={t}
          isRefreshingRuns={controller.webRunsRefreshing}
          onCancelRun={(runId) => controller.cancelRun(runId)}
          onRefreshRuns={() => void controller.refreshWebRuns()}
          onReuseRun={(run) => {
            controller.setSeedUrl(run.seedUrl);
            controller.setCrawlMode(
              run.mode === "single_page" ? "single_page" : "recursive_crawl",
            );
            controller.setBoundaryPolicy(
              run.boundaryPolicy === "allow_external"
                ? "allow_external"
                : "same_host",
            );
            controller.setMaxDepth(String(run.maxDepth ?? 3));
            controller.setMaxPages(String(run.maxPages ?? 100));
            controller.setCrawlAllowPatternsText(
              formatWebIngestPatterns(run.crawlFilter?.allowPatterns),
            );
            controller.setCrawlBlockPatternsText(
              formatWebIngestPatterns(run.crawlFilter?.blockPatterns),
            );
            controller.setMaterializationAllowPatternsText(
              formatWebIngestPatterns(
                run.materializationFilter?.allowPatterns,
              ),
            );
            controller.setMaterializationBlockPatternsText(
              formatWebIngestPatterns(
                run.materializationFilter?.blockPatterns,
              ),
            );
            controller.setAddLinkOpen(true);
          }}
          webRuns={controller.webRuns}
        />
      </div>
      <WebIngestDialog controller={controller} t={t} />
    </>
  );
}
