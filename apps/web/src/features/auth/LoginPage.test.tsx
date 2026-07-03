import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { fireEvent, waitFor } from "@testing-library/react";
import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { BootstrapBindingPurpose } from "@/shared/api/generated";
import type * as SharedApi from "@/shared/api";

import LoginPage from "./LoginPage";

const mocks = vi.hoisted(() => ({
  app: {
    login: vi.fn(),
    bootstrapSetup: vi.fn(),
    isBootstrapRequired: false,
    locale: "en",
    setLocale: vi.fn(),
  },
  bootstrapQueryFn: vi.fn(),
}));

vi.mock("@/shared/contexts/app-context", () => ({
  useApp: () => mocks.app,
}));

vi.mock("@/shared/api", async (importOriginal) => {
  const actual = await importOriginal<typeof SharedApi>();
  return {
    ...actual,
    queries: {
      ...actual.queries,
      getBootstrapStatusOptions: () => ({
        queryKey: ["bootstrap-status-test"],
        queryFn: mocks.bootstrapQueryFn,
        retry: false,
      }),
    },
  };
});

const readyBootstrapStatus = {
  setupRequired: true,
  aiSetup: {
    bindingBundles: [
      {
        providerCatalogId: "provider-alpha-bootstrap",
        providerKind: "provider_alpha",
        displayName: "Provider Alpha",
        credentialSource: "env",
        defaultBaseUrl: "https://provider.example/v1",
        credentialPolicy: {
          apiKeyRequired: true,
          baseUrlRequired: false,
          baseUrlMode: "fixed",
          validationMode: "model_list",
        },
        baseUrlPolicy: {
          allowOverride: false,
          requireHttps: true,
          allowPrivateNetwork: false,
          trimSuffixes: [],
        },
        modelDiscovery: { mode: "shared", paths: [] },
        capabilities: {},
        runtime: {
          kind: "openai_compatible",
          authScheme: "bearer",
          chatPath: "/chat/completions",
          embeddingsPath: "/embeddings",
          modelsPath: "/models",
          structuredOutput: "json_object",
          tokenLimitParameter: "max_tokens",
        },
        uiHints: {},
        bindings: [],
      },
    ],
  },
};

const bootstrapBindingPurposes: BootstrapBindingPurpose[] = [
  "extract_text",
  "extract_graph",
  "embed_chunk",
  "query_compile",
  "query_retrieve",
  "query_answer",
  "vision",
];

function bootstrapBinding(bindingPurpose: BootstrapBindingPurpose, index: number) {
  return {
    bindingPurpose,
    modelCatalogId: `model-${index}`,
    modelName: `model-${index}`,
    systemPrompt: null,
    temperature: null,
    topP: null,
    maxOutputTokensOverride: null,
  };
}

describe("LoginPage operator-safe errors", () => {
  let container: HTMLDivElement;
  let root: Root | null;
  let queryClient: QueryClient;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
    queryClient = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    mocks.app.login.mockReset();
    mocks.app.bootstrapSetup.mockReset();
    mocks.app.setLocale.mockReset();
    mocks.app.isBootstrapRequired = false;
    mocks.bootstrapQueryFn.mockReset();
    mocks.bootstrapQueryFn.mockResolvedValue(readyBootstrapStatus);
  });

  afterEach(async () => {
    await act(async () => {
      root?.unmount();
    });
    queryClient.clear();
    container.remove();
    root = null;
  });

  async function render(ui: ReactNode = <LoginPage />) {
    await act(async () => {
      root?.render(
        <QueryClientProvider client={queryClient}>
          <MemoryRouter>
            {ui}
          </MemoryRouter>
        </QueryClientProvider>,
      );
    });
  }

  function change(selector: string, value: string) {
    const input = container.querySelector<HTMLInputElement>(selector);
    if (!input) {
      throw new Error(`Missing input ${selector}`);
    }
    fireEvent.change(input, { target: { value } });
  }

  function completeSetupButton() {
    const button = Array.from(container.querySelectorAll("button"))
      .find((candidate) => candidate.textContent?.includes("Complete Setup"));
    if (!button) {
      throw new Error("Missing Complete Setup button");
    }
    return button;
  }

  it("does not render raw login errors", async () => {
    mocks.app.login.mockRejectedValue(new Error("database password hash leaked"));
    await render();

    await act(async () => {
      change("#login", "admin");
      change("#password", "secret");
    });
    await act(async () => {
      Array.from(container.querySelectorAll("button"))
        .find((button) => button.textContent?.includes("Sign In"))
        ?.click();
    });

    expect(container.querySelector("[role='alert']")).toHaveTextContent("Login failed");
    expect(container.textContent).not.toContain("database password hash leaked");
  });

  it("does not render raw bootstrap status errors", async () => {
    mocks.app.isBootstrapRequired = true;
    mocks.bootstrapQueryFn.mockRejectedValue(new Error("upstream stack trace"));
    await render();

    await waitFor(() => {
      expect(container.querySelector("[role='alert']")).toHaveTextContent(
        "Failed to load initial setup configuration",
      );
    });
    expect(completeSetupButton()).toBeDisabled();
    expect(container.textContent).not.toContain("upstream stack trace");
  });

  it("does not complete setup without a ready provider bundle", async () => {
    mocks.app.isBootstrapRequired = true;
    mocks.bootstrapQueryFn.mockResolvedValue({
      setupRequired: true,
      aiSetup: { bindingBundles: [] },
    });
    await render();

    await waitFor(() => {
      expect(container.textContent).toContain(
        "No bootstrap provider bundles are available for the current provider catalog.",
      );
    });
    await act(async () => {
      change("#admin-login", "admin");
      change("#admin-password", "secret");
    });

    expect(completeSetupButton()).toBeDisabled();
    expect(mocks.app.bootstrapSetup).not.toHaveBeenCalled();
  });

  it("does not render raw setup errors", async () => {
    mocks.app.isBootstrapRequired = true;
    mocks.app.bootstrapSetup.mockRejectedValue(new Error("insert into iam_user failed"));
    await render();

    await waitFor(() => {
      expect(container.textContent).toContain("Complete Setup");
    });
    await act(async () => {
      change("#admin-login", "admin");
      change("#admin-password", "secret");
    });
    await act(async () => {
      completeSetupButton().click();
    });

    expect(container.querySelector("[role='alert']")).toHaveTextContent("Setup failed");
    expect(container.textContent).not.toContain("insert into iam_user failed");
  });

  it("renders every generated bootstrap binding purpose from i18n metadata", async () => {
    mocks.app.isBootstrapRequired = true;
    mocks.bootstrapQueryFn.mockResolvedValue({
      ...readyBootstrapStatus,
      aiSetup: {
        bindingBundles: [
          {
            ...readyBootstrapStatus.aiSetup.bindingBundles[0],
            bindings: bootstrapBindingPurposes.map(bootstrapBinding),
          },
        ],
      },
    });

    await render();

    await waitFor(() => {
      expect(container.textContent).toContain("Text Extraction");
      expect(container.textContent).toContain("Query Retrieval");
    });

    expect(container.textContent).toContain("Read source content and prepare document text for the ingestion pipeline");
    expect(container.textContent).toContain("Retrieve the grounded context used to answer user questions");
    for (const purpose of bootstrapBindingPurposes) {
      expect(container.textContent).not.toContain(purpose);
    }
  });
});
