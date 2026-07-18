import type { Meta, StoryObj } from '@storybook/react'
import { BulkRerunProgressBanner, type BulkRerunProgressState } from './BulkRerunProgressBanner'
import i18n from '@/shared/i18n'

const t = i18n.t.bind(i18n)

const baseState = {
  kind: 'reprocess',
  operationId: 'operation-story',
  total: 24,
  completed: 0,
  failed: 0,
  inFlight: 4,
  status: 'processing',
} satisfies BulkRerunProgressState

const meta = {
  title: 'Features/Documents/BulkRerunProgressBanner',
  component: BulkRerunProgressBanner,
  args: {
    bulkRerun: baseState,
    onDismiss: () => undefined,
    t,
  },
  parameters: {
    layout: 'padded',
  },
} satisfies Meta<typeof BulkRerunProgressBanner>

export default meta
type Story = StoryObj<typeof meta>

export const ReprocessInFlight: Story = {
  args: {
    bulkRerun: {
      ...baseState,
      completed: 7,
      failed: 1,
      inFlight: 5,
    },
  },
}

export const Finalizing: Story = {
  args: {
    bulkRerun: {
      ...baseState,
      completed: 24,
      inFlight: 0,
    },
  },
}

export const Completed: Story = {
  args: {
    bulkRerun: {
      ...baseState,
      completed: 24,
      inFlight: 0,
      status: 'ready',
    },
  },
}

export const CompletedWithFailures: Story = {
  args: {
    bulkRerun: {
      ...baseState,
      completed: 21,
      failed: 3,
      inFlight: 0,
      status: 'ready',
    },
  },
}

export const DeleteCompleted: Story = {
  args: {
    bulkRerun: {
      ...baseState,
      kind: 'delete',
      total: 12,
      completed: 12,
      failed: 0,
      inFlight: 0,
      status: 'ready',
    },
  },
}
