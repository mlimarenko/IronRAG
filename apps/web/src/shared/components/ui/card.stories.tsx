import type { Meta, StoryObj } from "@storybook/react";
import { Badge } from "./badge";
import { Button } from "./button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "./card";
import { Separator } from "./separator";

const meta = {
  title: "UI/Card",
  component: Card,
} satisfies Meta<typeof Card>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  render: () => (
    <Card className="w-80">
      <CardHeader>
        <CardTitle>Library health</CardTitle>
        <CardDescription>Current ingestion and query readiness.</CardDescription>
      </CardHeader>
      <CardContent className="text-sm">
        The active library is ready for ingestion and query answering.
      </CardContent>
    </Card>
  ),
};

export const WithFooterActions: Story = {
  render: () => (
    <Card className="w-96">
      <CardHeader>
        <CardTitle>Reprocess selection</CardTitle>
        <CardDescription>Run extraction again for selected documents.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <div className="flex items-center justify-between">
          <span>Selected documents</span>
          <Badge variant="secondary">24</Badge>
        </div>
        <Separator />
        <p className="text-muted-foreground">
          Existing successful revisions are retained while the operation runs.
        </p>
      </CardContent>
      <CardFooter className="justify-end gap-2">
        <Button variant="outline">Cancel</Button>
        <Button>Reprocess</Button>
      </CardFooter>
    </Card>
  ),
};

export const MetricCard: Story = {
  render: () => (
    <Card className="w-72">
      <CardHeader className="pb-3">
        <CardDescription>Graph coverage</CardDescription>
        <CardTitle className="text-4xl">94%</CardTitle>
      </CardHeader>
      <CardContent className="text-sm text-muted-foreground">
        1,482 entities linked across 128 documents.
      </CardContent>
    </Card>
  ),
};
