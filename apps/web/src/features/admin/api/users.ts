import { useMutation, useQueryClient, type QueryFunctionContext } from '@tanstack/react-query'
import { queries, unwrap } from '@/shared/api'
import { Iam } from '@/shared/api/generated'
import type { SetUserAccessRequest, UserAccessResponse } from '@/shared/api/generated'

export type UserAccessRevision = Readonly<{
  access: UserAccessResponse
  etag: string | null
}>

type UserAccessQueryKey = ReturnType<typeof queries.getIamUserAccessQueryKey>

export function userAccessOptions(userId: string) {
  const generated = queries.getIamUserAccessOptions({ path: { userId } })
  return {
    queryKey: generated.queryKey,
    queryFn: async ({
      queryKey,
      signal,
    }: QueryFunctionContext<UserAccessQueryKey>): Promise<UserAccessRevision> => {
      const keyedUserId = queryKey[0].path?.userId
      if (!keyedUserId) throw new Error('Missing user id in getIamUserAccess query key')
      const result = await Iam.getIamUserAccess({ path: { userId: keyedUserId }, signal })
      return {
        access: unwrap<UserAccessResponse>(result),
        etag: result.response?.headers.get('etag') ?? null,
      }
    },
  }
}

type SetAccessVariables = Readonly<{
  body: SetUserAccessRequest
  etag: string | null
  userId: string
}>

export function errorStatus(error: unknown): number | undefined {
  if (error && typeof error === 'object' && 'status' in error) {
    const status = (error as { status?: unknown }).status
    return typeof status === 'number' ? status : undefined
  }
  return undefined
}

export function useUserMutations() {
  const queryClient = useQueryClient()
  const usersKey = queries.listIamUsersQueryKey()

  const createUser = useMutation({
    ...queries.createIamUserMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: usersKey })
    },
  })

  const patchUser = useMutation({
    ...queries.patchIamUserMutation(),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: usersKey })
    },
  })

  const deleteUser = useMutation({
    ...queries.deleteIamUserMutation(),
    onSuccess: async (_result, variables) => {
      queryClient.removeQueries({
        queryKey: queries.getIamUserAccessQueryKey({ path: variables.path }),
      })
      await queryClient.invalidateQueries({ queryKey: usersKey })
    },
  })

  const setUserAccess = useMutation({
    mutationKey: ['admin', 'iam', 'users', 'setAccess'],
    mutationFn: async ({ body, etag, userId }: SetAccessVariables) => {
      const result = await Iam.setIamUserAccess({
        path: { userId },
        body,
        ...(etag ? { headers: { 'If-Match': etag } } : {}),
      })
      return unwrap<UserAccessResponse>(result)
    },
    onSuccess: async (_access, variables) => {
      await queryClient.invalidateQueries({
        queryKey: queries.getIamUserAccessQueryKey({ path: { userId: variables.userId } }),
      })
    },
  })

  return { createUser, deleteUser, patchUser, setUserAccess }
}
