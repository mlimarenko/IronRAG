import { describe, expect, it } from "vitest";

import {
  buildBootstrapAiSetup,
  canEditProviderBaseUrl,
  hasStoredApiKeySummary,
  normalizeProviderBaseUrl,
  resolveProviderCredentialPolicy,
  shouldRenderBaseUrlInput,
  shouldRefreshCredentialModels,
} from "@/shared/lib/ai-provider";

const credentialPolicy = {
  apiKeyRequired: false,
  baseUrlRequired: true,
  baseUrlMode: "required" as const,
  validationMode: "model_list" as const,
};

const baseUrlPolicy = {
  allowOverride: true,
  requireHttps: false,
  allowPrivateNetwork: true,
  trimSuffixes: ["/v1", "/api"],
};

describe("normalizeProviderBaseUrl", () => {
  it("strips metadata-declared suffixes without checking provider names", () => {
    const provider = {
      baseUrlPolicy,
    };

    expect(normalizeProviderBaseUrl(provider, "http://localhost:11434/v1")).toBe(
      "http://localhost:11434",
    );
    expect(normalizeProviderBaseUrl(provider, "http://localhost:11434/api")).toBe(
      "http://localhost:11434",
    );
  });

  it("leaves URLs unchanged when policy has no trim suffixes", () => {
    expect(
      normalizeProviderBaseUrl(
        { baseUrlPolicy: { allowOverride: true, requireHttps: true, allowPrivateNetwork: false, trimSuffixes: [] } },
        "https://provider.example/v1",
      ),
    ).toBe(
      "https://provider.example/v1",
    );
  });
});

describe("provider policy helpers", () => {
  it("uses canonical credential policy from backend metadata", () => {
    expect(
      resolveProviderCredentialPolicy({
        credentialPolicy,
      }),
    ).toEqual(credentialPolicy);
  });

  it("treats fixed hosted URLs as non-editable", () => {
    expect(
      canEditProviderBaseUrl({
        credentialSource: "missing",
        baseUrlPolicy: { allowOverride: false, requireHttps: true, allowPrivateNetwork: false, trimSuffixes: [] },
      }),
    ).toBe(false);
  });

  it("refreshes credential models only for dynamic discovery metadata", () => {
    expect(shouldRefreshCredentialModels({ modelDiscovery: { mode: "credential", paths: [{ capabilityKind: "chat", path: "/models" }] } })).toBe(true);
    expect(shouldRefreshCredentialModels({ modelDiscovery: { mode: "shared", paths: [] } })).toBe(false);
    expect(shouldRefreshCredentialModels({ modelDiscovery: { mode: "unsupported", paths: [] } })).toBe(false);
  });

  it("renders base URL input when canonical policy requires a URL without a default", () => {
    expect(
      shouldRenderBaseUrlInput({
        credentialPolicy: {
          apiKeyRequired: false,
          baseUrlRequired: true,
          baseUrlMode: "required",
          validationMode: "model_list",
        },
        defaultBaseUrl: null,
      }),
    ).toBe(true);
  });

  it("fails loudly when provider metadata is missing required canonical fields", () => {
    expect(() =>
      resolveProviderCredentialPolicy({
        credentialPolicy: {
          apiKeyRequired: true,
          baseUrlRequired: false,
          baseUrlMode: "optional",
        },
      }),
    ).toThrow("credentialPolicy.validationMode");

    expect(() =>
      canEditProviderBaseUrl({
        credentialSource: "missing",
        baseUrlPolicy: { allowOverride: true, trimSuffixes: [] },
      }),
    ).toThrow("baseUrlPolicy.requireHttps");
  });
});

describe("hasStoredApiKeySummary", () => {
  it("treats missing summaries as absent tokens", () => {
    expect(hasStoredApiKeySummary("not_configured")).toBe(false);
    expect(hasStoredApiKeySummary("")).toBe(false);
  });

  it("keeps masked summaries as configured tokens", () => {
    expect(hasStoredApiKeySummary("sk-1••••1234")).toBe(true);
  });
});

describe("buildBootstrapAiSetup", () => {
  it("keeps bootstrap keyed by provider kind", () => {
    expect(
      buildBootstrapAiSetup(
        {
          providerKind: "provider-alpha",
          credentialSource: "missing",
          baseUrlPolicy,
        },
        " test-key ",
        "",
      ),
    ).toEqual({
      providerKind: "provider-alpha",
      apiKey: "test-key", // pragma: allowlist secret
    });
  });

  it("keeps metadata-normalized base url even when token is empty", () => {
    expect(
      buildBootstrapAiSetup(
        {
          providerKind: "provider-beta",
          credentialSource: "missing",
          baseUrlPolicy: { allowOverride: true, requireHttps: false, allowPrivateNetwork: true, trimSuffixes: ["/v1"] },
        },
        "",
        " http://localhost:11434/v1 ",
      ),
    ).toEqual({
      providerKind: "provider-beta",
      baseUrl: "http://localhost:11434",
    });
  });

  it("skips explicit setup when provider is already configured from env", () => {
    expect(
      buildBootstrapAiSetup(
        {
          providerKind: "provider-alpha",
          credentialSource: "env",
          baseUrlPolicy,
        },
        "test-key",
        "",
      ),
    ).toBeUndefined();
  });
});
