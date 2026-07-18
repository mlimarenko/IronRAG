import type { Meta, StoryObj } from '@storybook/react'
import { Tabs, TabsContent, TabsList, TabsTrigger } from './tabs'

const meta = {
  title: 'UI/Tabs',
  component: Tabs,
} satisfies Meta<typeof Tabs>

export default meta
type Story = StoryObj<typeof meta>

export const Default: Story = {
  render: () => (
    <Tabs defaultValue="overview" className="w-[420px]">
      <TabsList>
        <TabsTrigger value="overview">Overview</TabsTrigger>
        <TabsTrigger value="activity">Activity</TabsTrigger>
      </TabsList>
      <TabsContent value="overview" className="rounded-md border p-4 text-sm">
        The library has 128 indexed documents and 94 percent graph coverage.
      </TabsContent>
      <TabsContent value="activity" className="rounded-md border p-4 text-sm">
        Last ingestion completed 12 minutes ago with no failed documents.
      </TabsContent>
    </Tabs>
  ),
}

export const DisabledTab: Story = {
  render: () => (
    <Tabs defaultValue="documents" className="w-[420px]">
      <TabsList>
        <TabsTrigger value="documents">Documents</TabsTrigger>
        <TabsTrigger value="graph" disabled>
          Graph
        </TabsTrigger>
        <TabsTrigger value="settings">Settings</TabsTrigger>
      </TabsList>
      <TabsContent value="documents" className="rounded-md border p-4 text-sm">
        Graph tools stay disabled until the library has generated entities.
      </TabsContent>
      <TabsContent value="settings" className="rounded-md border p-4 text-sm">
        Library policy controls appear here.
      </TabsContent>
    </Tabs>
  ),
}

export const ThreePanelWorkbench: Story = {
  render: () => (
    <Tabs defaultValue="queue" className="w-[520px]">
      <TabsList className="grid w-full grid-cols-3">
        <TabsTrigger value="queue">Queue</TabsTrigger>
        <TabsTrigger value="ready">Ready</TabsTrigger>
        <TabsTrigger value="failed">Failed</TabsTrigger>
      </TabsList>
      <TabsContent value="queue" className="rounded-md border p-4 text-sm">
        18 documents are queued for extraction.
      </TabsContent>
      <TabsContent value="ready" className="rounded-md border p-4 text-sm">
        102 documents are ready for retrieval.
      </TabsContent>
      <TabsContent value="failed" className="rounded-md border p-4 text-sm">
        3 documents need operator review.
      </TabsContent>
    </Tabs>
  ),
}
