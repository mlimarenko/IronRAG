import { useCallback, useMemo, useState } from "react";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import type { TFunction } from "i18next";
import { toast } from "sonner";

import {
  adminApi,
  documentsApi,
  queries,
  type CatalogLibraryResponse,
  type WebBoundaryPolicy,
  type WebIngestMode,
  type WebIngestRunListItem,
  type WebIngestUrlFilter,
} from "@/shared/api";
import type { Library } from "@/shared/types";

import {
  buildWebIngestUrlFilter,
  formatWebIngestPatterns,
} from "@/features/documents/model/webIngestPatterns";

import type {
  WebIngestPolicyDraft,
  WebIngestPolicySnapshot,
} from "./documentsPageState";

const DEFAULT_WEB_INGEST_MAX_PAGES = "100";

type WebIngestControllerInput = {
  activeLibrary: Library | null;
  errorMessage: (error: unknown, fallback: string) => string;
  fetchLibraryWebIngestPolicy: (
    libraryId: string,
  ) => Promise<WebIngestPolicySnapshot | null>;
  libraryPolicyData: CatalogLibraryResponse | undefined;
  libraryPolicyLoading: boolean;
  loadFirstPage: () => Promise<void>;
  loadedWebIngestPolicy: WebIngestPolicySnapshot;
  refreshWebRuns: () => Promise<void>;
  t: TFunction;
  webRuns: WebIngestRunListItem[];
  webRunsRefreshing: boolean;
};

type WebIngestPolicyVariables = {
  libraryId: string;
  crawlFilter: WebIngestUrlFilter;
  materializationFilter: WebIngestUrlFilter;
};

type WebIngestPolicyContext = {
  previousDraft: WebIngestPolicyDraft | null;
  previousLibrary: CatalogLibraryResponse | undefined;
};

function draftFromPolicy(
  libraryId: string,
  policy: WebIngestPolicySnapshot,
): WebIngestPolicyDraft {
  return {
    libraryId,
    crawlAllowText: policy.crawlFilter.allowText,
    crawlBlockText: policy.crawlFilter.blockText,
    materializationAllowText: policy.materializationFilter.allowText,
    materializationBlockText: policy.materializationFilter.blockText,
  };
}

function draftFromFilters(
  libraryId: string,
  crawlFilter: WebIngestUrlFilter,
  materializationFilter: WebIngestUrlFilter,
): WebIngestPolicyDraft {
  return {
    libraryId,
    crawlAllowText: formatWebIngestPatterns(crawlFilter.allowPatterns),
    crawlBlockText: formatWebIngestPatterns(crawlFilter.blockPatterns),
    materializationAllowText: formatWebIngestPatterns(
      materializationFilter.allowPatterns,
    ),
    materializationBlockText: formatWebIngestPatterns(
      materializationFilter.blockPatterns,
    ),
  };
}

export function useWebIngestController({
  activeLibrary,
  errorMessage,
  fetchLibraryWebIngestPolicy,
  libraryPolicyData,
  libraryPolicyLoading,
  loadedWebIngestPolicy,
  loadFirstPage,
  refreshWebRuns,
  t,
  webRuns,
  webRunsRefreshing,
}: WebIngestControllerInput) {
  const queryClient = useQueryClient();
  const activeLibraryId = activeLibrary?.id ?? null;
  const [addLinkOpen, setAddLinkOpen] = useState(false);
  const [seedUrl, setSeedUrl] = useState("");
  const [crawlMode, setCrawlMode] =
    useState<WebIngestMode>("recursive_crawl");
  const [boundaryPolicy, setBoundaryPolicy] =
    useState<WebBoundaryPolicy>("same_host");
  const [maxDepth, setMaxDepth] = useState("3");
  const [maxPages, setMaxPages] = useState(DEFAULT_WEB_INGEST_MAX_PAGES);
  const [ruleTestUrl, setRuleTestUrl] = useState("");
  const [webIngestPolicyDraft, setWebIngestPolicyDraft] =
    useState<WebIngestPolicyDraft | null>(null);
  const [webIngestLoading, setWebIngestLoading] = useState(false);
  const activePolicyDraft =
    webIngestPolicyDraft?.libraryId === activeLibraryId
      ? webIngestPolicyDraft
      : null;
  const webIngestPolicyLoading =
    libraryPolicyLoading && !!activeLibraryId && !libraryPolicyData;
  const crawlAllowPatternsText =
    activePolicyDraft?.crawlAllowText ??
    loadedWebIngestPolicy.crawlFilter.allowText;
  const crawlBlockPatternsText =
    activePolicyDraft?.crawlBlockText ??
    loadedWebIngestPolicy.crawlFilter.blockText;
  const materializationAllowPatternsText =
    activePolicyDraft?.materializationAllowText ??
    loadedWebIngestPolicy.materializationFilter.allowText;
  const materializationBlockPatternsText =
    activePolicyDraft?.materializationBlockText ??
    loadedWebIngestPolicy.materializationFilter.blockText;

  const updatePolicyDraft = useCallback(
    (patch: Partial<Omit<WebIngestPolicyDraft, "libraryId">>) => {
      if (!activeLibraryId) return;
      setWebIngestPolicyDraft((prev) => {
        const base =
          prev?.libraryId === activeLibraryId
            ? prev
            : draftFromPolicy(activeLibraryId, loadedWebIngestPolicy);
        return { ...base, ...patch };
      });
    },
    [activeLibraryId, loadedWebIngestPolicy],
  );
  const resetCreateForm = useCallback(() => {
    setSeedUrl("");
    setCrawlMode("recursive_crawl");
    setBoundaryPolicy("same_host");
    setMaxDepth("3");
    setMaxPages(DEFAULT_WEB_INGEST_MAX_PAGES);
    setRuleTestUrl("");
  }, []);
  const openCreateDialog = useCallback(() => {
    resetCreateForm();
    setAddLinkOpen(true);
  }, [resetCreateForm]);

  const saveWebIngestPolicyMutation = useMutation<
    CatalogLibraryResponse,
    unknown,
    WebIngestPolicyVariables,
    WebIngestPolicyContext
  >({
    mutationKey: ["documents", "web-ingest-policy", activeLibraryId],
    scope: { id: `documents:web-ingest-policy:${activeLibraryId ?? "none"}` },
    mutationFn: ({ libraryId, crawlFilter, materializationFilter }) =>
      adminApi.updateWebIngestPolicy(libraryId, {
        crawlFilter,
        materializationFilter,
      }),
    onMutate: async ({ libraryId, crawlFilter, materializationFilter }) => {
      const queryKey = queries.getCatalogLibraryOptions({
        path: { libraryId },
      }).queryKey;
      await queryClient.cancelQueries({ queryKey });
      const previousLibrary =
        queryClient.getQueryData<CatalogLibraryResponse>(queryKey);
      const previousDraft = webIngestPolicyDraft;
      queryClient.setQueryData<CatalogLibraryResponse | undefined>(
        queryKey,
        (current) =>
          current
            ? {
                ...current,
                webIngestPolicy: {
                  crawlFilter,
                  materializationFilter,
                },
              }
            : current,
      );
      setWebIngestPolicyDraft(
        draftFromFilters(libraryId, crawlFilter, materializationFilter),
      );
      return { previousDraft, previousLibrary };
    },
    onSuccess: (updatedLibrary, { libraryId }) => {
      queryClient.setQueryData(
        queries.getCatalogLibraryOptions({ path: { libraryId } }).queryKey,
        updatedLibrary,
      );
    },
    onError: (err, { libraryId }, context) => {
      if (context) {
        queryClient.setQueryData(
          queries.getCatalogLibraryOptions({ path: { libraryId } }).queryKey,
          context.previousLibrary,
        );
        setWebIngestPolicyDraft(context.previousDraft);
      }
      toast.error(
        t("documents.mutations.webIngestPolicy.failed", {
          error: errorMessage(err, t("documents.webIngestFailed")),
        }),
      );
    },
    onSettled: (_data, _err, variables) => {
      if (!variables) return;
      void queryClient.invalidateQueries({
        queryKey: queries.getCatalogLibraryOptions({
          path: { libraryId: variables.libraryId },
        }).queryKey,
      });
    },
  });
  const { mutateAsync: saveWebIngestPolicy } = saveWebIngestPolicyMutation;

  const startWebIngest = useCallback(async () => {
    if (!activeLibrary || !seedUrl.trim()) return;
    let url = seedUrl.trim();
    if (!/^https?:\/\//i.test(url)) url = `https://${url}`;
    try {
      new URL(url);
    } catch {
      toast.error(t("documents.invalidUrl"));
      return;
    }
    setWebIngestLoading(true);
    let policyRollbackToastShown = false;
    try {
      const savedPolicy = libraryPolicyData
        ? loadedWebIngestPolicy
        : await fetchLibraryWebIngestPolicy(activeLibrary.id);
      if (savedPolicy == null) return;
      const nextDraft = activePolicyDraft ?? draftFromPolicy(activeLibrary.id, savedPolicy);
      const crawlFilter = buildWebIngestUrlFilter(
        nextDraft.crawlAllowText,
        nextDraft.crawlBlockText,
      );
      const materializationFilter = buildWebIngestUrlFilter(
        nextDraft.materializationAllowText,
        nextDraft.materializationBlockText,
      );
      const normalizedDraft = draftFromFilters(
        activeLibrary.id,
        crawlFilter,
        materializationFilter,
      );
      const savedDraft = draftFromPolicy(activeLibrary.id, savedPolicy);
      let effectiveCrawlFilter = crawlFilter;
      let effectiveMaterializationFilter = materializationFilter;
      if (
        normalizedDraft.crawlAllowText !== savedDraft.crawlAllowText ||
        normalizedDraft.crawlBlockText !== savedDraft.crawlBlockText ||
        normalizedDraft.materializationAllowText !==
          savedDraft.materializationAllowText ||
        normalizedDraft.materializationBlockText !==
          savedDraft.materializationBlockText
      ) {
        const updatedLibrary = await saveWebIngestPolicy({
          libraryId: activeLibrary.id,
          crawlFilter,
          materializationFilter,
        }).catch((err: unknown) => {
          policyRollbackToastShown = true;
          throw err;
        });
        effectiveCrawlFilter =
          updatedLibrary.webIngestPolicy?.crawlFilter ?? crawlFilter;
        effectiveMaterializationFilter =
          updatedLibrary.webIngestPolicy?.materializationFilter ??
          materializationFilter;
      }
      setWebIngestPolicyDraft(
        draftFromFilters(
          activeLibrary.id,
          effectiveCrawlFilter,
          effectiveMaterializationFilter,
        ),
      );
      await documentsApi.createWebIngestRun({
        libraryId: activeLibrary.id,
        seedUrl: url,
        mode: crawlMode,
        boundaryPolicy,
        maxDepth: Number.parseInt(maxDepth, 10),
        maxPages: Number.parseInt(maxPages, 10),
        crawlFilter: effectiveCrawlFilter,
        materializationFilter: effectiveMaterializationFilter,
      });
      toast.success(t("documents.webIngestStarted"));
      setAddLinkOpen(false);
      resetCreateForm();
      await refreshWebRuns();
      await loadFirstPage();
    } catch (err) {
      if (!policyRollbackToastShown) {
        toast.error(errorMessage(err, t("documents.webIngestFailed")));
      }
    } finally {
      setWebIngestLoading(false);
    }
  }, [
    activeLibrary,
    activePolicyDraft,
    boundaryPolicy,
    crawlMode,
    errorMessage,
    fetchLibraryWebIngestPolicy,
    libraryPolicyData,
    loadedWebIngestPolicy,
    loadFirstPage,
    maxDepth,
    maxPages,
    refreshWebRuns,
    resetCreateForm,
    seedUrl,
    saveWebIngestPolicy,
    t,
  ]);

  const cancelRun = useCallback(
    async (runId: string) => {
      try {
        await documentsApi.cancelWebRun(runId);
        toast.success(t("documents.webIngestCancelRequested"));
        await refreshWebRuns();
      } catch (err) {
        toast.error(errorMessage(err, t("documents.webIngestCancelFailed")));
      }
    },
    [errorMessage, refreshWebRuns, t],
  );

  return useMemo(
    () => ({
      addLinkOpen,
      boundaryPolicy,
      cancelRun,
      crawlAllowPatternsText,
      crawlBlockPatternsText,
      crawlMode,
      materializationAllowPatternsText,
      materializationBlockPatternsText,
      maxDepth,
      maxPages,
      openCreateDialog,
      refreshWebRuns,
      ruleTestUrl,
      seedUrl,
      setAddLinkOpen,
      setBoundaryPolicy,
      setCrawlAllowPatternsText: (value: string) =>
        updatePolicyDraft({ crawlAllowText: value }),
      setCrawlBlockPatternsText: (value: string) =>
        updatePolicyDraft({ crawlBlockText: value }),
      setCrawlMode,
      setMaterializationAllowPatternsText: (value: string) =>
        updatePolicyDraft({ materializationAllowText: value }),
      setMaterializationBlockPatternsText: (value: string) =>
        updatePolicyDraft({ materializationBlockText: value }),
      setMaxDepth,
      setMaxPages,
      setRuleTestUrl,
      setSeedUrl,
      startWebIngest,
      webIngestLoading,
      webIngestPolicyLoading,
      webRuns,
      webRunsRefreshing,
    }),
    [
      addLinkOpen,
      boundaryPolicy,
      cancelRun,
      crawlAllowPatternsText,
      crawlBlockPatternsText,
      crawlMode,
      materializationAllowPatternsText,
      materializationBlockPatternsText,
      maxDepth,
      maxPages,
      openCreateDialog,
      refreshWebRuns,
      ruleTestUrl,
      seedUrl,
      startWebIngest,
      updatePolicyDraft,
      webIngestLoading,
      webIngestPolicyLoading,
      webRuns,
      webRunsRefreshing,
    ],
  );
}

export type WebIngestController = ReturnType<typeof useWebIngestController>;
