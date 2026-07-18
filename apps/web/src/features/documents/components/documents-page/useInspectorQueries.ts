import { useCallback } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'

import { queries } from '@/shared/api'
import type { DocumentItem } from '@/shared/types'

import { SELECTED_DETAIL_REFRESH_MS } from './documentsPageState'

export function useInspectorQueries(selectedDoc: DocumentItem | null) {
  const queryClient = useQueryClient()
  const isSelectedTerminal =
    selectedDoc?.status === 'ready' ||
    selectedDoc?.status === 'failed' ||
    selectedDoc?.status === 'canceled'
  const docQuery = useQuery({
    ...queries.getContentDocumentOptions({
      path: { documentId: selectedDoc?.id ?? '' },
    }),
    enabled: !!selectedDoc?.id,
    staleTime: 0,
    refetchInterval: isSelectedTerminal ? false : SELECTED_DETAIL_REFRESH_MS,
    refetchIntervalInBackground: false,
  })
  const fetchSelectedDetail = useCallback(
    async (documentId: string) => {
      await queryClient.invalidateQueries({
        queryKey: queries.getContentDocumentOptions({ path: { documentId } }).queryKey,
      })
    },
    [queryClient],
  )
  return {
    fetchSelectedDetail,
    inspectorLifecycle: docQuery.data?.lifecycle ?? null,
  }
}
