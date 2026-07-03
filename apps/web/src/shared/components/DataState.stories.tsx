import type { Meta, StoryObj } from "@storybook/react";

import { DataState } from "./DataState";

type ExampleDocument = {
  id: string;
  title: string;
};

const documents: ExampleDocument[] = [
  { id: "doc-1", title: "Architecture Notes" },
  { id: "doc-2", title: "Release Checklist" },
];

const renderDocuments = (items: ExampleDocument[]) => (
  <div className="w-80 rounded-lg border bg-card p-4">
    <div className="text-sm font-semibold">Documents</div>
    <div className="mt-3 space-y-2">
      {items.map((item) => (
        <div key={item.id} className="rounded-md bg-surface-sunken px-3 py-2 text-sm">
          {item.title}
        </div>
      ))}
    </div>
  </div>
);

const meta = {
  title: "Shared/DataState",
  component: DataState<ExampleDocument[]>,
  args: {
    query: { isLoading: false, error: null, data: documents },
    children: renderDocuments,
  },
} satisfies Meta<typeof DataState<ExampleDocument[]>>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Loading: Story = {
  render: () => (
    <DataState query={{ isLoading: true, error: null, data: undefined }}>
      {renderDocuments}
    </DataState>
  ),
};

export const Error: Story = {
  render: () => (
    <DataState
      query={{ isLoading: false, error: new globalThis.Error("Request timed out"), data: undefined }}
    >
      {renderDocuments}
    </DataState>
  ),
};

export const Empty: Story = {
  render: () => (
    <DataState
      query={{ isLoading: false, error: null, data: [] }}
      emptyRender={
        <div className="w-80 rounded-lg border border-dashed p-4 text-sm text-muted-foreground">
          No documents found.
        </div>
      }
    >
      {renderDocuments}
    </DataState>
  ),
};

export const Content: Story = {
  render: () => (
    <DataState query={{ isLoading: false, error: null, data: documents }}>
      {renderDocuments}
    </DataState>
  ),
};
