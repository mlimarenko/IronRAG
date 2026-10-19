import { useMutation, useQueryClient } from '@tanstack/react-query'
import { queries } from '@/shared/api'

export function useAccessMutations() {
  const queryClient = useQueryClient()
  const tokenListKey = queries.listIamTokensQueryKey()

  const mintToken = useMutation({
    ...queries.mintIamTokenMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: tokenListKey })
    },
  })

  const patchToken = useMutation({
    ...queries.patchIamTokenMutation(),
    onSuccess: async (_token, variables) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: tokenListKey }),
        queryClient.invalidateQueries({
          queryKey: queries.getIamTokenQueryKey({ path: variables.path }),
        }),
      ])
    },
  })

  const deleteToken = useMutation({
    ...queries.deleteIamTokenMutation(),
    onSuccess: async (_result, variables) => {
      queryClient.removeQueries({
        queryKey: queries.getIamTokenQueryKey({ path: variables.path }),
      })
      await queryClient.invalidateQueries({ queryKey: tokenListKey })
    },
  })

  return { deleteToken, mintToken, patchToken }
}
