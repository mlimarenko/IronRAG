import type { Meta, StoryObj } from "@storybook/react";
import {
  Select,
  SelectContent,
  SelectGroup,
  SelectItem,
  SelectLabel,
  SelectSeparator,
  SelectTrigger,
  SelectValue,
} from "./select";
import { Label } from "./label";

const meta = {
  title: "UI/Select",
  component: Select,
} satisfies Meta<typeof Select>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Basic: Story = {
  render: () => (
    <div className="w-64 space-y-2">
      <Label htmlFor="story-workspace-select">Workspace</Label>
      <Select defaultValue="documents">
        <SelectTrigger id="story-workspace-select">
          <SelectValue placeholder="Choose a workspace" />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="documents">Documents</SelectItem>
          <SelectItem value="research">Research</SelectItem>
          <SelectItem value="archive">Archive</SelectItem>
        </SelectContent>
      </Select>
    </div>
  ),
};

export const GroupedOptions: Story = {
  render: () => (
    <div className="w-72 space-y-2">
      <Label htmlFor="story-readiness-select">Readiness</Label>
      <Select defaultValue="graph-ready">
        <SelectTrigger id="story-readiness-select">
          <SelectValue placeholder="Choose readiness" />
        </SelectTrigger>
        <SelectContent>
          <SelectGroup>
            <SelectLabel>Ready</SelectLabel>
            <SelectItem value="graph-ready">Graph ready</SelectItem>
            <SelectItem value="readable">Readable</SelectItem>
          </SelectGroup>
          <SelectSeparator />
          <SelectGroup>
            <SelectLabel>Needs attention</SelectLabel>
            <SelectItem value="processing">Processing</SelectItem>
            <SelectItem value="failed">Failed</SelectItem>
          </SelectGroup>
        </SelectContent>
      </Select>
    </div>
  ),
};

export const Disabled: Story = {
  render: () => (
    <div className="w-64 space-y-2">
      <Label htmlFor="story-locked-select">Access policy</Label>
      <Select defaultValue="locked" disabled>
        <SelectTrigger id="story-locked-select">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="locked">Locked selection</SelectItem>
        </SelectContent>
      </Select>
    </div>
  ),
};

export const Placeholder: Story = {
  render: () => (
    <div className="w-64 space-y-2">
      <Label htmlFor="story-library-select">Library</Label>
      <Select>
        <SelectTrigger id="story-library-select">
          <SelectValue placeholder="Select a library" />
        </SelectTrigger>
        <SelectContent>
          <SelectItem value="default">Default library</SelectItem>
          <SelectItem value="contracts">Contracts</SelectItem>
          <SelectItem value="support">Support corpus</SelectItem>
        </SelectContent>
      </Select>
    </div>
  ),
};
