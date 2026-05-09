import { expect, test } from "@playwright/test";

import { iamSession } from "../../src/shared/api/mocks/fixtures";
import { installBrowserMocks, mockPath } from "./support/mocks";

test("logs in with browser MSW session stubs", async ({ page }) => {
  const session = iamSession();
  await installBrowserMocks(page, { authenticated: false, session });

  await page.goto(mockPath("/"));

  await expect(page.getByRole("heading", { name: "Sign in" })).toBeVisible();
  await page.getByLabel("Login").fill(session.user.login);
  await page.getByLabel("Password").fill("synthetic-admin-password");

  const loginResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/v1/iam/session/login") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "Sign In" }).click();
  await loginResponse;

  await expect(page).toHaveURL(/\/dashboard$/);
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
});
