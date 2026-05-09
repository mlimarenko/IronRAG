Run component stories without booting the full IronRAG app:

```bash
npm run storybook
```

Build the static Storybook bundle:

```bash
npm run build-storybook
```

Stories live next to the component they exercise under `src/` and use:

```text
component-name.stories.tsx
```

Use CSF3 only:

```tsx
import type { Meta, StoryObj } from "@storybook/react";
import { Component } from "./component";

const meta = {
  title: "UI/Component",
  component: Component,
} satisfies Meta<typeof Component>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Default: Story = {
  args: {},
};
```

Storybook imports `src/index.css`, so Tailwind v4 classes and app tokens match Vite.

React Query and `MemoryRouter` providers are installed globally in
`preview.ts`; stories can render components that call query or router hooks.

MSW is initialized globally from `src/shared/api/mocks/handlers`. Override handlers
per story with `parameters.msw.handlers`:

```tsx
import { http, HttpResponse } from "msw";

export const Loaded: Story = {
  parameters: {
    msw: {
      handlers: [
        http.get("/v1/health", () => HttpResponse.json({ status: "ok" })),
      ],
    },
  },
};
```
