import type { Meta, StoryObj } from "@storybook/react";
import { Separator } from "./separator";

const meta = {
  title: "UI/Separator",
  component: Separator,
} satisfies Meta<typeof Separator>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Horizontal: Story = {
  render: () => (
    <div className="w-80 space-y-3 text-sm">
      <div>
        <div className="font-medium">Default library</div>
        <div className="text-muted-foreground">Ready for retrieval</div>
      </div>
      <Separator />
      <div className="text-muted-foreground">Updated 12 minutes ago</div>
    </div>
  ),
};

export const Vertical: Story = {
  render: () => (
    <div className="flex h-16 items-center gap-4 text-sm">
      <div>
        <div className="font-medium">128</div>
        <div className="text-muted-foreground">Documents</div>
      </div>
      <Separator orientation="vertical" />
      <div>
        <div className="font-medium">1,482</div>
        <div className="text-muted-foreground">Entities</div>
      </div>
      <Separator orientation="vertical" />
      <div>
        <div className="font-medium">94%</div>
        <div className="text-muted-foreground">Coverage</div>
      </div>
    </div>
  ),
};

export const SectionBreaks: Story = {
  render: () => (
    <div className="w-96 space-y-4 text-sm">
      <section>
        <h3 className="font-semibold">Workspace</h3>
        <p className="text-muted-foreground">Research operations</p>
      </section>
      <Separator decorative={false} />
      <section>
        <h3 className="font-semibold">Library</h3>
        <p className="text-muted-foreground">Contracts and policy documents</p>
      </section>
      <Separator decorative={false} />
      <section>
        <h3 className="font-semibold">Status</h3>
        <p className="text-muted-foreground">All bindings are ready.</p>
      </section>
    </div>
  ),
};
