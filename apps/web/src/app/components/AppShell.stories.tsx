import { createElement } from "react";
import type { Meta, StoryObj } from "@storybook/react";
import { http, HttpResponse } from "msw";
import type { SessionResolveResponse } from "@/shared/api/auth";
import { AppProvider } from "@/shared/contexts/AppContext";
import { AppShell } from "./AppShell";

function makeSession(
  libraries: NonNullable<SessionResolveResponse["shellBootstrap"]>["libraries"],
  workspaces: NonNullable<SessionResolveResponse["shellBootstrap"]>["workspaces"] = [
    { id: "ws-default", slug: "default", name: "Default workspace", lifecycleState: "active" },
  ],
): SessionResolveResponse {
  return {
    mode: "authenticated",
    locale: "en",
    session: {
      sessionId: "storybook-session",
      expiresAt: "2026-05-05T12:00:00Z",
      user: {
        displayName: "Admin User",
        email: "admin@example.com",
        login: "admin",
        principalId: "principal-admin",
      },
    },
    me: {
      effectiveGrants: [],
      principal: {
        id: "principal-admin",
        displayLabel: "Admin User",
        principalKind: "user",
        status: "active",
      },
      user: {
        login: "admin",
        displayName: "Admin User",
        principalId: "principal-admin",
        role: "admin",
      },
      workspaceMemberships: [],
    },
    shellBootstrap: {
      capabilities: [],
      effectiveGrants: [],
      libraries,
      locale: "en",
      viewer: {
        accessLabel: "Admin User",
        displayName: "Admin User",
        isAdmin: true,
        login: "admin",
        principalId: "principal-admin",
        role: "admin",
      },
      warnings: [],
      workspaceMemberships: [],
      workspaces,
    },
    bootstrapStatus: { setupRequired: false },
    message: null,
  };
}

function shellHandlers(session: SessionResolveResponse) {
  return [
    http.get("/v1/iam/session/resolve", () => HttpResponse.json(session)),
    http.get("/v1/version/update", () =>
      HttpResponse.json({ status: "current", latestVersion: null, releaseUrl: null }),
    ),
  ];
}

const defaultLibraries: NonNullable<SessionResolveResponse["shellBootstrap"]>["libraries"] = [
  {
    id: "lib-default",
    workspaceId: "ws-default",
    slug: "default-library",
    name: "Default library",
    ingestionReady: true,
    lifecycleState: "active",
    missingBindingPurposes: [],
  },
];

const meta = {
  title: "App/AppShell",
  component: AppShell,
  decorators: [
    (Story) => {
      if (typeof window !== "undefined") {
        window.localStorage.removeItem("ironrag_active_workspace");
        window.localStorage.removeItem("ironrag_active_library");
      }

      return createElement(AppProvider, null, createElement(Story));
    },
  ],
  parameters: {
    layout: "fullscreen",
  },
} satisfies Meta<typeof AppShell>;

export default meta;
type Story = StoryObj<typeof meta>;

export const Authenticated: Story = {
  args: {
    children: (
      <div className="flex h-full items-center justify-center bg-muted/30 p-8">
        <div className="rounded-lg border bg-background px-6 py-5 text-sm shadow-sm">
          Dashboard content renders inside the shell.
        </div>
      </div>
    ),
  },
  parameters: {
    msw: {
      handlers: shellHandlers(makeSession(defaultLibraries)),
    },
  },
};

export const LibraryWarning: Story = {
  args: {
    children: (
      <div className="flex h-full items-center justify-center bg-muted/30 p-8">
        <div className="rounded-lg border bg-background px-6 py-5 text-sm shadow-sm">
          Library bindings require operator attention.
        </div>
      </div>
    ),
  },
  parameters: {
    msw: {
      handlers: shellHandlers(
        makeSession([
          {
            id: "lib-warning",
            workspaceId: "ws-default",
            slug: "warning-library",
            name: "Compliance library",
            ingestionReady: true,
            lifecycleState: "active",
            missingBindingPurposes: ["query_answer", "embed_chunk"],
          },
        ]),
      ),
    },
  },
};

export const EmptyWorkspace: Story = {
  args: {
    children: (
      <div className="flex h-full items-center justify-center bg-muted/30 p-8">
        <div className="rounded-lg border bg-background px-6 py-5 text-sm shadow-sm">
          No workspace or library is available yet.
        </div>
      </div>
    ),
  },
  parameters: {
    msw: {
      handlers: shellHandlers(makeSession([], [])),
    },
  },
};
