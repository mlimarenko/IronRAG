import { act } from "react";
import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ProviderCredentialFields } from "./ProviderCredentialFields";

const labels = {
  apiKeyRequired: "Provider token", // pragma: allowlist secret
  apiKeyOptional: "Provider token optional", // pragma: allowlist secret
  apiKeyPlaceholder: "Provider token", // pragma: allowlist secret
  apiKeyRequiredHint: "Enter the provider token before saving this credential.", // pragma: allowlist secret
  baseUrlRequired: "Base URL",
  baseUrlOptional: "Base URL optional",
  baseUrlRequiredHint: "Enter the provider endpoint before saving this credential.",
  fixedBaseUrlHint: "This endpoint is fixed by the provider catalog.",
};

const providerBase = {
  credentialSource: "missing",
  credentialPolicy: {
    apiKeyRequired: true,
    baseUrlRequired: true,
    baseUrlMode: "required",
    validationMode: "model_list",
  },
  defaultBaseUrl: "https://provider.example/v1",
  uiHints: {},
};

describe("ProviderCredentialFields", () => {
  let container: HTMLDivElement;
  let root: Root | null;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => {
      root?.unmount();
    });
    container.remove();
    root = null;
  });

  async function render(ui: ReactNode) {
    await act(async () => {
      root?.render(ui);
    });
  }

  it("renders a fixed endpoint as readable read-only text instead of a disabled input", async () => {
    await render(
      <ProviderCredentialFields
        provider={{
          ...providerBase,
          baseUrlPolicy: {
            allowOverride: false,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: [],
          },
        }}
        idPrefix="provider"
        apiKey=""
        baseUrl=""
        labels={labels}
        onApiKeyChange={vi.fn()}
        onBaseUrlChange={vi.fn()}
      />,
    );

    expect(container.querySelector("input#provider-base-url")).toBeNull();
    expect(container.querySelector("#provider-base-url")?.textContent).toBe("https://provider.example/v1");
    expect(container.querySelector("#provider-base-url")?.getAttribute("aria-readonly")).toBe("true");
    expect(container.querySelector("#provider-base-url")?.getAttribute("role")).toBe("textbox");
    expect(container.querySelector("#provider-base-url")?.getAttribute("tabindex")).toBe("0");
    expect(container.textContent).toContain(labels.fixedBaseUrlHint);
  });

  it("adds field-level required hints and aria wiring for missing editable credentials", async () => {
    await render(
      <ProviderCredentialFields
        provider={{
          ...providerBase,
          baseUrlPolicy: {
            allowOverride: true,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: [],
          },
        }}
        idPrefix="provider"
        apiKey=""
        baseUrl=""
        labels={labels}
        onApiKeyChange={vi.fn()}
        onBaseUrlChange={vi.fn()}
        apiKeyError={labels.apiKeyRequiredHint}
        baseUrlError={labels.baseUrlRequiredHint}
      />,
    );

    const baseUrlInput = container.querySelector<HTMLInputElement>("input#provider-base-url");
    const apiKeyInput = container.querySelector<HTMLInputElement>("input#provider-api-key");

    expect(baseUrlInput?.getAttribute("aria-invalid")).toBe("true");
    expect(baseUrlInput?.getAttribute("aria-describedby")).toContain("provider-base-url-error");
    expect(apiKeyInput?.getAttribute("aria-invalid")).toBe("true");
    expect(apiKeyInput?.getAttribute("aria-describedby")).toContain("provider-api-key-error");
    expect(container.textContent).toContain(labels.baseUrlRequiredHint);
    expect(container.textContent).toContain(labels.apiKeyRequiredHint);
  });

  it("does not require a token when credentials come from environment", async () => {
    await render(
      <ProviderCredentialFields
        provider={{
          ...providerBase,
          credentialSource: "env",
          baseUrlPolicy: {
            allowOverride: true,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: [],
          },
        }}
        idPrefix="provider"
        apiKey=""
        baseUrl=""
        labels={labels}
        onApiKeyChange={vi.fn()}
        onBaseUrlChange={vi.fn()}
      />,
    );

    const apiKeyInput = container.querySelector<HTMLInputElement>("input#provider-api-key");

    expect(apiKeyInput?.disabled).toBe(true);
    expect(apiKeyInput?.hasAttribute("required")).toBe(false);
    expect(apiKeyInput?.getAttribute("aria-invalid")).toBeNull();
    expect(container.textContent).not.toContain(labels.apiKeyRequiredHint);
  });

  it("does not require a replacement token when editing a preserved secret", async () => {
    await render(
      <ProviderCredentialFields
        provider={{
          ...providerBase,
          baseUrlPolicy: {
            allowOverride: true,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: [],
          },
        }}
        idPrefix="provider"
        apiKey=""
        baseUrl="https://provider.example/v1"
        labels={{
          ...labels,
          keepSecretPlaceholder: "Leave blank to keep current credential", // pragma: allowlist secret
        }}
        onApiKeyChange={vi.fn()}
        onBaseUrlChange={vi.fn()}
        preserveExistingSecret
      />,
    );

    const apiKeyInput = container.querySelector<HTMLInputElement>("input#provider-api-key");

    expect(apiKeyInput?.hasAttribute("required")).toBe(false);
    expect(apiKeyInput?.getAttribute("aria-invalid")).toBeNull();
    expect(container.textContent).not.toContain(labels.apiKeyRequiredHint);
  });

  it("surfaces a field-level hint when a required fixed endpoint has no catalog value", async () => {
    await render(
      <ProviderCredentialFields
        provider={{
          ...providerBase,
          defaultBaseUrl: null,
          baseUrlPolicy: {
            allowOverride: false,
            requireHttps: true,
            allowPrivateNetwork: false,
            trimSuffixes: [],
          },
        }}
        idPrefix="provider"
        apiKey={"token" /* pragma: allowlist secret */}
        baseUrl=""
        labels={labels}
        onApiKeyChange={vi.fn()}
        onBaseUrlChange={vi.fn()}
      />,
    );

    const endpointValue = container.querySelector("#provider-base-url");
    const describedBy = endpointValue?.getAttribute("aria-describedby") ?? "";

    expect(endpointValue?.textContent).toBe(labels.baseUrlRequiredHint);
    expect(container.textContent).toContain(labels.baseUrlRequiredHint);
    expect(describedBy.split(" ")).toEqual([...new Set(describedBy.split(" "))]);
  });
});
