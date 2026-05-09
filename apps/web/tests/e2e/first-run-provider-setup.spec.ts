import { expect, test } from "@playwright/test";

import { installBrowserMocks, mockPath } from "./support/mocks";

test("completes first-run setup with a hosted router provider", async ({ page }) => {
  await installBrowserMocks(page, {
    authenticated: false,
    bootstrapRequired: true,
  });

  await page.goto(mockPath("/"));

  await expect(page.getByRole("heading", { name: "Initial Setup" })).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Provider" })).toContainText("Hosted Router");
  await expect(page.getByRole("textbox", { name: "https://router.example/api/v1" })).toBeVisible();

  await page.getByLabel("Admin login").fill("admin");
  await page.getByLabel("Password").fill("synthetic-admin-password");

  const completeButton = page.getByRole("button", { name: "Complete Setup" });
  await expect(completeButton).toBeDisabled();

  await page.getByLabel("API key").fill("test-key");
  await expect(completeButton).toBeEnabled();

  const setupResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/v1/iam/bootstrap/setup") &&
      response.request().method() === "POST",
  );
  await completeButton.click();
  await setupResponse;

  await expect(page).toHaveURL(/\/dashboard$/);
  await expect(page.getByRole("heading", { name: "Dashboard" })).toBeVisible();
});

test("supports first-run setup with an editable local provider endpoint", async ({ page }) => {
  await installBrowserMocks(page, {
    authenticated: false,
    bootstrapRequired: true,
  });

  await page.goto(mockPath("/"));

  await page.getByRole("combobox", { name: "Provider" }).click();
  await page.getByRole("option", { name: "Local Runtime" }).click();

  await expect(page.getByLabel("Provider address")).toBeVisible();
  await page.getByLabel("Admin login").fill("admin");
  await page.getByLabel("Password").fill("synthetic-admin-password");
  await page.getByLabel("Provider address").fill("http://127.0.0.1:18080/v1");

  const setupResponse = page.waitForResponse(
    (response) =>
      response.url().endsWith("/v1/iam/bootstrap/setup") &&
      response.request().method() === "POST",
  );
  await page.getByRole("button", { name: "Complete Setup" }).click();
  await setupResponse;

  await expect(page).toHaveURL(/\/dashboard$/);
});
