import { Search } from "lucide-react";
import type { Meta, StoryObj } from "@storybook/react";
import { Input } from "./input";
import { Label } from "./label";

const meta = {
  title: "UI/Input",
  component: Input,
  args: {
    placeholder: "Search documents",
  },
} satisfies Meta<typeof Input>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {};

export const WithValue: Story = {
  args: {
    value: "policy-handbook.pdf",
    readOnly: true,
  },
};

export const Disabled: Story = {
  args: {
    disabled: true,
    value: "Locked field",
    readOnly: true,
  },
};

export const SearchField: Story = {
  render: () => (
    <div className="w-80 space-y-2">
      <Label htmlFor="story-search">Document search</Label>
      <div className="relative">
        <Search className="pointer-events-none absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2 text-muted-foreground" />
        <Input id="story-search" placeholder="Search title or source URL" className="pl-9" />
      </div>
    </div>
  ),
};

export const FileInput: Story = {
  args: {
    type: "file",
  },
};
