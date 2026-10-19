import { useMutation, useQueryClient } from '@tanstack/react-query'
import { queries, unwrap } from '@/shared/api'
import { Webhooks } from '@/shared/api/generated'
import type {
  CreateSubscriptionRequest,
  SubscriptionResponse,
  UpdateSubscriptionRequest,
} from '@/shared/api/generated'

const DELETE_RETRY_DELAY_MS = 1_000

type CreateWebhookVariables = Readonly<{
  body: CreateSubscriptionRequest
  workspaceId: string
}>

type UpdateWebhookVariables = Readonly<{
  body: UpdateSubscriptionRequest
  subscriptionId: string
  workspaceId: string
}>

type DeleteWebhookVariables = Readonly<{
  onDraining: () => void
  subscriptionId: string
  workspaceId: string
}>

function waitForDeleteRetry(): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, DELETE_RETRY_DELAY_MS))
}

async function deleteUntilDrained({
  onDraining,
  subscriptionId,
}: DeleteWebhookVariables): Promise<void> {
  while (true) {
    const result = await Webhooks.deleteWebhookSubscription({ path: { subscriptionId } })
    unwrap(result)
    if (result.response?.status === 204) return
    onDraining()
    await waitForDeleteRetry()
  }
}

export function useWebhookMutations(workspaceId: string) {
  const queryClient = useQueryClient()
  const listKey = queries.listWebhookSubscriptionsQueryKey({ path: { workspaceId } })

  const createWebhook = useMutation({
    mutationKey: ['admin', 'webhooks', 'create'],
    mutationFn: async ({ body, workspaceId: targetWorkspaceId }: CreateWebhookVariables) => {
      const result = await Webhooks.createWebhookSubscription({
        path: { workspaceId: targetWorkspaceId },
        body,
      })
      return unwrap<SubscriptionResponse>(result)
    },
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: listKey })
    },
  })

  const updateWebhook = useMutation({
    mutationKey: ['admin', 'webhooks', 'update'],
    mutationFn: async ({ body, subscriptionId }: UpdateWebhookVariables) => {
      const result = await Webhooks.updateWebhookSubscription({ path: { subscriptionId }, body })
      return unwrap<SubscriptionResponse>(result)
    },
    onSuccess: async (_subscription, variables) => {
      await Promise.all([
        queryClient.invalidateQueries({ queryKey: listKey }),
        queryClient.invalidateQueries({
          queryKey: queries.getWebhookSubscriptionQueryKey({
            path: { subscriptionId: variables.subscriptionId },
          }),
        }),
      ])
    },
  })

  const deleteWebhook = useMutation({
    mutationKey: ['admin', 'webhooks', 'delete'],
    mutationFn: deleteUntilDrained,
    onSuccess: async (_result, variables) => {
      queryClient.removeQueries({
        queryKey: queries.getWebhookSubscriptionQueryKey({
          path: { subscriptionId: variables.subscriptionId },
        }),
      })
      queryClient.removeQueries({
        predicate: (query) =>
          query.queryKey[0] !== null &&
          typeof query.queryKey[0] === 'object' &&
          '_id' in query.queryKey[0] &&
          query.queryKey[0]._id === 'listWebhookDeliveryAttempts',
      })
      await queryClient.invalidateQueries({ queryKey: listKey })
    },
  })

  return { createWebhook, deleteWebhook, updateWebhook }
}
