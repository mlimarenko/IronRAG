import { expect, test } from "@playwright/test";

import { installBrowserMocks, mockPath } from "./support/mocks";

const WORKSPACE_ID = "workspace-alpha";
const LIBRARY_ID = "library-demo-1";
const SESSION_ID = "session-source-links";

test("renders assistant markdown source links as visible links", async ({ page }, testInfo) => {
  await installBrowserMocks(page, {
    querySessions: [
      {
        conversationState: "active",
        createdAt: "2026-05-13T00:00:00.000Z",
        id: SESSION_ID,
        libraryId: LIBRARY_ID,
        title: "Source answer",
        turnCount: 2,
        updatedAt: "2026-05-13T00:00:01.000Z",
        workspaceId: WORKSPACE_ID,
      },
    ],
    queryConversations: {
      [SESSION_ID]: {
        session: {
          conversationState: "active",
          createdAt: "2026-05-13T00:00:00.000Z",
          id: SESSION_ID,
          libraryId: LIBRARY_ID,
          title: "Source answer",
          turnCount: 2,
          updatedAt: "2026-05-13T00:00:01.000Z",
          workspaceId: WORKSPACE_ID,
        },
        messages: [
          {
            content: "Where is the source?",
            id: "turn-user",
            role: "user",
            timestamp: "2026-05-13T00:00:00.000Z",
          },
          {
            content: "The answer cites a source.\n\n---\nSources\nAlpha Guide",
            executionId: "execution-source-link",
            id: "turn-assistant",
            role: "assistant",
            timestamp: "2026-05-13T00:00:01.000Z",
            evidence: {
              preparedSegmentReferences: [
                {
                  blockKind: "heading",
                  documentId: "doc-1",
                  documentTitle: "Alpha Guide",
                  headingTrail: ["Alpha Guide"],
                  rank: 1,
                  score: 0.91,
                  sectionPath: [],
                  segmentId: "seg-1",
                  sourceAccess: {
                    href: "/v1/content/documents/doc-1/source",
                    kind: "stored_document",
                  },
                  sourceUri: "upload://doc-1",
                },
              ],
              technicalFactReferences: [],
              entityReferences: [],
              relationReferences: [],
              verificationState: "verified",
              verificationWarnings: [],
              runtimeStageSummaries: [],
            },
          },
        ],
      },
    },
  });

  await page.goto(mockPath("/assistant"));

  await page.getByRole("button", { name: /Source answer/ }).click();

  const sourceLink = page.getByRole("link", { name: "Alpha Guide" });
  await expect(sourceLink).toBeVisible();
  await expect(sourceLink).toHaveAttribute("href", /\/v1\/content\/documents\/doc-1\/source$/);
  await expect(sourceLink).toHaveAttribute("target", "_blank");
  await expect(sourceLink).toHaveCSS("text-decoration-line", "underline");

  await page.screenshot({
    path: testInfo.outputPath("assistant-source-links.png"),
    fullPage: false,
  });
});
