import { useCallback } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";

import { queries } from "@/shared/api";
import type { DocumentItem } from "@/shared/types";

import { SELECTED_DETAIL_REFRESH_MS } from "./documentsPageState";

export function useInspectorQueries(selectedDoc: DocumentItem | null) {
  const queryClient = useQueryClient();
  const isSelectedTerminal =
    selectedDoc?.status === "ready" ||
    selectedDoc?.status === "failed" ||
    selectedDoc?.status === "canceled";
  const docQuery = useQuery({
    ...queries.getContentDocumentOptions({
      path: { documentId: selectedDoc?.id ?? "" },
    }),
    enabled: !!selectedDoc?.id,
    staleTime: 0,
    refetchInterval: isSelectedTerminal ? false : SELECTED_DETAIL_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const segmentsQuery = useQuery({
    ...queries.listContentPreparedSegmentsOptions({
      path: { documentId: selectedDoc?.id ?? "" },
      query: { limit: 1 },
    }),
    enabled: !!selectedDoc?.id,
    staleTime: 0,
    refetchInterval: isSelectedTerminal ? false : SELECTED_DETAIL_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const factsQuery = useQuery({
    ...queries.listContentTechnicalFactsOptions({
      path: { documentId: selectedDoc?.id ?? "" },
    }),
    enabled: !!selectedDoc?.id,
    staleTime: 0,
    refetchInterval: isSelectedTerminal ? false : SELECTED_DETAIL_REFRESH_MS,
    refetchIntervalInBackground: false,
  });
  const fetchSelectedDetail = useCallback(
    async (documentId: string) => {
      await Promise.all([
        queryClient.invalidateQueries({
          queryKey: queries.getContentDocumentOptions({ path: { documentId } }).queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: queries.listContentPreparedSegmentsOptions({
            path: { documentId },
            query: { limit: 1 },
          }).queryKey,
        }),
        queryClient.invalidateQueries({
          queryKey: queries.listContentTechnicalFactsOptions({
            path: { documentId },
          }).queryKey,
        }),
      ]);
    },
    [queryClient],
  );
  return {
    fetchSelectedDetail,
    inspectorFacts: factsQuery.data?.total ?? null,
    inspectorLifecycle: docQuery.data?.lifecycle ?? null,
    inspectorSegments: segmentsQuery.data?.total ?? null,
  };
}
