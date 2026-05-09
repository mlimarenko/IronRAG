import { expect, test } from "@playwright/test";

type StorybookIndexEntry = {
  id: string;
  importPath?: string;
  name?: string;
  title?: string;
  type: string;
};

type StorybookIndex = {
  entries: Record<string, StorybookIndexEntry>;
};

const storybookUrl = "http://localhost:6006";
const storyPathPattern = /^src\/.*\.stories\.tsx$/;

function normalizedImportPath(entry: StorybookIndexEntry): string | null {
  if (!entry.importPath) {
    return null;
  }

  return entry.importPath.replace(/\\/g, "/").replace(/^(\.\/|\/)/, "");
}

function collectSrcStories(index: StorybookIndex): StorybookIndexEntry[] {
  return Object.values(index.entries)
    .filter((entry) => entry.type === "story")
    .filter((entry) => {
      const importPath = normalizedImportPath(entry);
      return importPath !== null && storyPathPattern.test(importPath);
    })
    .sort((left, right) => {
      const leftPath = normalizedImportPath(left) ?? "";
      const rightPath = normalizedImportPath(right) ?? "";
      return leftPath.localeCompare(rightPath) || left.id.localeCompare(right.id);
    });
}

test("Storybook stories match committed visual baselines", async ({
  page,
  request,
}) => {
  const indexResponse = await request.get(`${storybookUrl}/index.json`);
  expect(indexResponse.ok()).toBeTruthy();

  const storybookIndex = (await indexResponse.json()) as StorybookIndex;
  const stories = collectSrcStories(storybookIndex);

  expect(
    stories,
    "expected Storybook stories under src/**/*.stories.tsx",
  ).not.toHaveLength(0);

  for (const story of stories) {
    await test.step(story.id, async () => {
      await page.goto(`${storybookUrl}/iframe.html?id=${story.id}&viewMode=story`, {
        waitUntil: "domcontentloaded",
      });

      await page.locator("#storybook-root").waitFor({ state: "attached" });
      await page.evaluate(async () => {
        await document.fonts.ready;
        await new Promise<void>((resolve) => {
          requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
        });
      });

      // Storybook error pages include hashed asset stack traces after each build.
      const dynamicStorybookErrorStack = page.locator(".sb-errordisplay code");
      await expect(page).toHaveScreenshot(`${story.id}.png`, {
        animations: "disabled",
        caret: "hide",
        fullPage: false,
        mask: [dynamicStorybookErrorStack],
        maskColor: "#222222",
      });
    });
  }
});
