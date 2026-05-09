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
  type WebIngestPattern,
  type WebIngestRunListItem,
  type WebIngestUrlFilterMode,
} from "@/shared/api";
import type { Library } from "@/shared/types";

import {
  formatWebIngestPatterns,
  parseWebIngestPatternText,
} from "@/features/documents/model/webIngestPatterns";

import type {
  WebIngestUrlFilterSnapshot,
} from "./documentsPageState";

type WebIngestControllerInput = {
  activeLibrary: Library | null;
  errorMessage: (error: unknown, fallback: string) => string;
  fetchLibraryWebIngestPolicy: (
    libraryId: string,
  ) => Promise<WebIngestUrlFilterSnapshot | null>;
  libraryPolicyData: CatalogLibraryResponse | undefined;
  libraryPolicyLoading: boolean;
  loadFirstPage: () => Promise<void>;
  loadedUrlFilter: WebIngestUrlFilterSnapshot;
  refreshWebRuns: () => Promise<void>;
  t: TFunction;
  webRuns: WebIngestRunListItem[];
  webRunsRefreshing: boolean;
};

type WebIngestPolicyVariables = {
  libraryId: string;
  mode: WebIngestUrlFilterMode;
  patterns: WebIngestPattern[];
};

type WebIngestPolicyContext = {
  previousDraft: {
    libraryId: string;
    mode: WebIngestUrlFilterMode;
    patternsText: string;
  } | null;
  previousLibrary: CatalogLibraryResponse | undefined;
};

export function useWebIngestController({
  activeLibrary,
  errorMessage,
  fetchLibraryWebIngestPolicy,
  libraryPolicyData,
  libraryPolicyLoading,
  loadedUrlFilter,
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
  const [maxPages, setMaxPages] = useState("100");
  const [urlFilterDraft, setUrlFilterDraft] = useState<{
    libraryId: string;
    mode: WebIngestUrlFilterMode;
    patternsText: string;
  } | null>(null);
  const [webIngestLoading, setWebIngestLoading] = useState(false);
  const activeUrlFilterDraft =
    urlFilterDraft?.libraryId === activeLibraryId ? urlFilterDraft : null;
  const urlFilterMode = activeUrlFilterDraft?.mode ?? loadedUrlFilter.mode;
  const urlFilterPatternsText =
    activeUrlFilterDraft?.patternsText ?? loadedUrlFilter.text;
  const urlFilterLoading =
    libraryPolicyLoading && !!activeLibraryId && !libraryPolicyData;

  const setUrlFilterMode = useCallback(
    (mode: WebIngestUrlFilterMode) => {
      if (!activeLibraryId) return;
      setUrlFilterDraft((prev) => ({
        libraryId: activeLibraryId,
        mode,
        patternsText:
          prev?.libraryId === activeLibraryId
            ? prev.patternsText
            : loadedUrlFilter.text,
      }));
    },
    [activeLibraryId, loadedUrlFilter.text],
  );
  const setUrlFilterPatternsText = useCallback(
    (patternsText: string) => {
      if (!activeLibraryId) return;
      setUrlFilterDraft((prev) => ({
        libraryId: activeLibraryId,
        mode:
          prev?.libraryId === activeLibraryId
            ? prev.mode
            : loadedUrlFilter.mode,
        patternsText,
      }));
    },
    [activeLibraryId, loadedUrlFilter.mode],
  );
  const resetCreateForm = useCallback(() => {
    setSeedUrl("");
    setCrawlMode("recursive_crawl");
    setBoundaryPolicy("same_host");
    setMaxDepth("3");
    setMaxPages("30");
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
    mutationFn: ({ libraryId, mode, patterns }) =>
      adminApi.updateWebIngestPolicy(libraryId, {
        urlFilter: { mode, patterns },
      }),
    onMutate: async ({ libraryId, mode, patterns }) => {
      const queryKey = queries.getCatalogLibraryOptions({
        path: { libraryId },
      }).queryKey;
      await queryClient.cancelQueries({ queryKey });
      const previousLibrary =
        queryClient.getQueryData<CatalogLibraryResponse>(queryKey);
      const previousDraft = urlFilterDraft;
      queryClient.setQueryData<CatalogLibraryResponse | undefined>(
        queryKey,
        (current) =>
          current
            ? {
                ...current,
                webIngestPolicy: {
                  ...current.webIngestPolicy,
                  urlFilter: { mode, patterns },
                },
              }
            : current,
      );
      setUrlFilterDraft({
        libraryId,
        mode,
        patternsText: formatWebIngestPatterns(patterns),
      });
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
        setUrlFilterDraft(context.previousDraft);
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
        ? loadedUrlFilter
        : await fetchLibraryWebIngestPolicy(activeLibrary.id);
      if (savedPolicy == null) return;
      let nextMode = activeUrlFilterDraft ? urlFilterMode : savedPolicy.mode;
      const nextText = activeUrlFilterDraft
        ? urlFilterPatternsText
        : savedPolicy.text;
      const urlPatterns = parseWebIngestPatternText(nextText);
      if (nextMode === "allowlist" && urlPatterns.length === 0) {
        toast.error(t("documents.urlFilterAllowlistEmpty"));
        return;
      }
      let normalizedText = formatWebIngestPatterns(urlPatterns);
      if (nextMode !== savedPolicy.mode || normalizedText !== savedPolicy.text) {
        const updatedLibrary = await saveWebIngestPolicy({
            libraryId: activeLibrary.id,
            mode: nextMode,
            patterns: urlPatterns,
          })
          .catch((err: unknown) => {
            policyRollbackToastShown = true;
            throw err;
          });
        const updatedFilter = updatedLibrary.webIngestPolicy?.urlFilter ?? {
          mode: nextMode,
          patterns: urlPatterns,
        };
        nextMode = updatedFilter.mode ?? nextMode;
        normalizedText = formatWebIngestPatterns(
          updatedFilter.patterns ?? urlPatterns,
        );
      }
      setUrlFilterDraft({
        libraryId: activeLibrary.id,
        mode: nextMode,
        patternsText: normalizedText,
      });
      await documentsApi.createWebIngestRun({
        libraryId: activeLibrary.id,
        seedUrl: url,
        mode: crawlMode,
        boundaryPolicy,
        maxDepth: Number.parseInt(maxDepth, 10),
        maxPages: Number.parseInt(maxPages, 10),
        urlFilter: { mode: nextMode, patterns: urlPatterns },
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
    activeUrlFilterDraft,
    boundaryPolicy,
    crawlMode,
    errorMessage,
    fetchLibraryWebIngestPolicy,
    libraryPolicyData,
    loadedUrlFilter,
    loadFirstPage,
    maxDepth,
    maxPages,
    refreshWebRuns,
    resetCreateForm,
    seedUrl,
    saveWebIngestPolicy,
    t,
    urlFilterMode,
    urlFilterPatternsText,
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
      crawlMode,
      maxDepth,
      maxPages,
      openCreateDialog,
      refreshWebRuns,
      seedUrl,
      setAddLinkOpen,
      setBoundaryPolicy,
      setCrawlMode,
      setMaxDepth,
      setMaxPages,
      setSeedUrl,
      setUrlFilterMode,
      setUrlFilterPatternsText,
      startWebIngest,
      urlFilterLoading,
      urlFilterMode,
      urlFilterPatternsText,
      webIngestLoading,
      webRuns,
      webRunsRefreshing,
    }),
    [
      addLinkOpen,
      boundaryPolicy,
      cancelRun,
      crawlMode,
      maxDepth,
      maxPages,
      openCreateDialog,
      refreshWebRuns,
      seedUrl,
      setUrlFilterMode,
      setUrlFilterPatternsText,
      startWebIngest,
      urlFilterLoading,
      urlFilterMode,
      urlFilterPatternsText,
      webIngestLoading,
      webRuns,
      webRunsRefreshing,
    ],
  );
}

export type WebIngestController = ReturnType<typeof useWebIngestController>;
