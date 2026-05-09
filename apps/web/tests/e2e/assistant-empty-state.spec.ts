import { expect, test } from "@playwright/test";

import { installBrowserMocks, mockPath } from "./support/mocks";

test("renders the assistant empty state with browser MSW stubs", async ({ page }) => {
  await installBrowserMocks(page);

  await page.goto(mockPath("/assistant"));

  await expect(page.getByRole("heading", { name: "AI Assistant" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Ask a question" })).toBeVisible();
  await expect(page.getByPlaceholder("Ask a question...")).toBeVisible();
});
