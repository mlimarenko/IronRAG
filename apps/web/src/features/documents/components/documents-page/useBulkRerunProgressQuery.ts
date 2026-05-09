import { useMemo } from "react";
import { useQuery } from "@tanstack/react-query";

import { ASYNC_OPERATION_TERMINAL_STATES, queries } from "@/shared/api";

import type { BulkRerunState } from "./documentsPageState";

export function useBulkRerunProgressQuery(bulkRerun: BulkRerunState | null) {
  const operationQuery = useQuery({
    ...queries.getAsyncOperationOptions({
      path: { operationId: bulkRerun?.operationId ?? "" },
    }),
    enabled: !!bulkRerun?.operationId,
    refetchInterval: (q) => {
      const data = q.state.data;
      return data && ASYNC_OPERATION_TERMINAL_STATES.has(data.status)
        ? false
        : 2000;
    },
    refetchIntervalInBackground: false,
    staleTime: 0,
  });
  const progress = useMemo(() => {
    const detail = operationQuery.data;
    if (!bulkRerun) return null;
    if (!detail) return bulkRerun;
    return {
      kind: bulkRerun.kind,
      operationId: bulkRerun.operationId,
      total: Math.max(bulkRerun.total, detail.progress.total || 0),
      completed: detail.progress.completed,
      failed: detail.progress.failed,
      inFlight: detail.progress.inFlight,
      status: detail.status,
    };
  }, [bulkRerun, operationQuery.data]);
  return { operationQuery, progress };
}
